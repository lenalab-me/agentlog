//! Reads Claude Code session logs (`~/.claude/projects/<project>/<session>.jsonl`, and the
//! subagents under `<session>/subagents/`) into tool calls. Each file is one context: a
//! subagent starts with an empty context, so it is counted apart from its parent. A
//! subagent whose lines sit in its parent's file (`isSidechain`, with its `agentId`) is a
//! context of its own too. A session that starts with a copy of another's history (the same
//! tool_use ids and times) does not count those calls again.
//!
//! The window is applied to each call's own timestamp. A file's mtime only rules files out
//! (a file last written before the window has no call in it); resuming an old session
//! rewrites the file, so mtime alone would let months-old calls in.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use memchr::memmem;
use serde_json::Value;

use crate::{jsonl, lines, talk, time};

/// Longest command or result kept per call. Enough for signatures and examples.
const KEEP: usize = 4000;

pub struct Ctx {
    /// `project/session8` or `project/session8/agent-id`, for printing.
    pub label: String,
}

pub struct Call {
    pub ctx: usize,
    pub ts: i64,
    pub tool: String,
    /// Bash: the command. Read/Write/Edit: the file path. Others: the input as JSON.
    pub input: String,
    pub failed: bool,
    pub result: String,
}

pub struct Filter {
    pub since: i64,
    pub until: i64,
    /// Keep only project directories whose name contains one of these.
    pub projects: Vec<String>,
    /// Leave out files whose path contains one of these.
    pub excludes: Vec<String>,
}

pub struct Log {
    pub ctxs: Vec<Ctx>,
    /// Grouped by context, in the order the calls were made.
    pub calls: Vec<Call>,
}

pub fn clip(s: &str) -> String {
    if s.len() <= KEEP {
        return s.to_string();
    }
    let mut end = KEEP;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

pub fn jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        // file_type does not follow symlinks; metadata does, so linked directories count.
        let Ok(md) = fs::metadata(&p) else { continue };
        if md.is_dir() {
            jsonl_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
}

pub fn mtime(p: &Path) -> i64 {
    fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `-Users-me-project-prod-x` → `project-prod-x`, from the home directory's spelling.
pub fn short_project(dir: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let prefix: String = home
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    let s = dir.strip_prefix(&prefix).unwrap_or(dir);
    let s = s.trim_start_matches('-');
    if s.is_empty() {
        "~".to_string()
    } else {
        s.to_string()
    }
}

fn label(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let parts: Vec<String> = rel
        .iter()
        .map(|c| c.to_string_lossy().into_owned())
        .collect();
    let project = short_project(parts.first().map(String::as_str).unwrap_or(""));
    let stem = |s: &str| s.trim_end_matches(".jsonl").to_string();
    let short = |s: &str| s.chars().take(8).collect::<String>();
    match parts.len() {
        0 | 1 => project,
        2 => format!("{project}/{}", short(&stem(&parts[1]))),
        _ => format!(
            "{project}/{}/{}",
            short(&parts[1]),
            stem(parts.last().unwrap())
        ),
    }
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|x| x.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn summarize(tool: &str, input: &Value) -> String {
    let field = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_string);
    let s = match tool {
        "Bash" => field("command"),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => field("file_path"),
        _ => None,
    };
    clip(&s.unwrap_or_else(|| input.to_string()))
}

/// A Bash call that exited non-zero reports `Exit code N` first; other tools set is_error.
fn is_failure(is_error: bool, text: &str) -> bool {
    if is_error {
        return true;
    }
    let first = text.lines().next().unwrap_or("");
    first.starts_with("Exit code ") && first != "Exit code 0"
}

/// A call as read from a file, before it is given a context.
struct Read {
    /// The subagent whose lines these are, when they sit in the parent's file
    /// (isSidechain set, with an agentId).
    agent: Option<String>,
    /// The tool_use id: a session that carries a copy of another's history has the same.
    id: Option<String>,
    call: Call,
}

/// The id of the subagent a line is from, when a subagent's lines sit in its parent's file.
/// A subagent's own file is one context already.
fn sidechain(v: &Value) -> Option<String> {
    if !v
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    Some(
        v.get("agentId")
            .and_then(Value::as_str)
            .unwrap_or("sidechain")
            .to_string(),
    )
}

/// The calls of one log file. A line that is not JSON is passed over, and so is a last line
/// with no newline (still being written), as `search` does.
fn read_file(path: &Path, f: &Filter) -> io::Result<(Vec<Read>, Option<String>)> {
    let agent_file = path
        .file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with("agent-"));
    let mut calls: Vec<Read> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut cwd: Option<String> = None;
    lines::scan_from(
        path,
        0,
        |_, _| true,
        |_, line| {
            // Cheap skip for lines that cannot hold a tool call or result.
            if memmem::find(line, b"\"tool_use").is_none()
                && memmem::find(line, b"\"tool_result").is_none()
            {
                return;
            }
            let Some(v) = jsonl::parse(line) else { return };
            if cwd.is_none() {
                cwd = v.get("cwd").and_then(Value::as_str).map(str::to_string);
            }
            read_line(&v, f, agent_file, &mut calls, &mut by_id);
        },
    )?;
    Ok((calls, cwd))
}

/// Whether `--project` keeps a log file: its folder (`-Users-me-project-x`) or the name
/// `search` shows for its working directory (`x`) holds one of the names, so that the
/// name `search` printed picks the same sessions here.
pub fn project_wanted(root: &Path, path: &Path, cwd: Option<&str>, projects: &[String]) -> bool {
    let folder = folder_of(root, path);
    let shown = cwd.map(talk::project_name);
    projects.is_empty()
        || projects.iter().any(|p| {
            folder.contains(p.as_str()) || shown.as_ref().is_some_and(|s| s.contains(p.as_str()))
        })
}

/// The filter without `--project`: which project a file is for is known once it is read.
pub fn any_project(f: &Filter) -> Filter {
    Filter {
        since: f.since,
        until: f.until,
        projects: Vec::new(),
        excludes: f.excludes.clone(),
    }
}

fn read_line(
    v: &Value,
    f: &Filter,
    agent_file: bool,
    calls: &mut Vec<Read>,
    by_id: &mut HashMap<String, usize>,
) {
    let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else {
        return;
    };
    let ts = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(time::parse_iso);
    let agent = if agent_file { None } else { sidechain(v) };
    for b in content {
        match b.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                let Some(ts) = ts else { continue };
                if ts < f.since || ts >= f.until {
                    continue;
                }
                let tool = b.get("name").and_then(Value::as_str).unwrap_or("?");
                let input = summarize(tool, b.get("input").unwrap_or(&Value::Null));
                let id = b.get("id").and_then(Value::as_str).map(str::to_string);
                if let Some(id) = &id {
                    by_id.insert(id.clone(), calls.len());
                }
                calls.push(Read {
                    agent: agent.clone(),
                    id,
                    call: Call {
                        ctx: 0,
                        ts,
                        tool: tool.to_string(),
                        input,
                        failed: false,
                        result: String::new(),
                    },
                });
            }
            Some("tool_result") => {
                let id = b.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
                let Some(&i) = by_id.get(id) else { continue };
                let text = text_of(b.get("content").unwrap_or(&Value::Null));
                let is_error = b.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                calls[i].call.failed = is_failure(is_error, &text);
                calls[i].call.result = clip(&text);
            }
            _ => {}
        }
    }
}

/// Every log file under the root, in path order.
pub fn all_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    fs::read_dir(root)?;
    let mut files = Vec::new();
    jsonl_files(root, &mut files);
    files.sort();
    Ok(files)
}

/// The files of `all` that a window and the project/exclude filters leave to read.
pub fn select_from(root: &Path, all: &[PathBuf], f: &Filter) -> Vec<PathBuf> {
    all.iter()
        .filter(|p| {
            let s = p.to_string_lossy();
            mtime(p) >= f.since
                && !f.excludes.iter().any(|x| s.contains(x.as_str()))
                && (f.projects.is_empty()
                    || f.projects
                        .iter()
                        .any(|x| folder_of(root, p).contains(x.as_str())))
        })
        .cloned()
        .collect()
}

/// The files a window and the project/exclude filters leave to read, in path order.
pub fn select_files(root: &Path, f: &Filter) -> io::Result<Vec<PathBuf>> {
    Ok(select_from(root, &all_files(root)?, f))
}

/// The project directory a log file sits in (`-Users-me-project-x`).
pub fn folder_of(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .iter()
        .next()
        .map(|c| c.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reads every file on worker threads; results come back in file order. A file that
/// `read` gives up on (unreadable, half-written, nothing in it) is left out.
pub fn read_all<T: Send>(
    files: &[PathBuf],
    read: impl Fn(&Path) -> Option<T> + Sync,
) -> Vec<(usize, T)> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(usize, T)>> = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(files.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(p) = files.get(i) else { break };
                    if let Some(r) = read(p) {
                        results.lock().unwrap().push((i, r));
                    }
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(i, _)| *i);
    results
}

pub fn load(root: &Path, f: &Filter) -> io::Result<Log> {
    let files = select_files(root, &any_project(f))?;
    // An unreadable file is skipped, not fatal.
    let results = read_all(&files, |p| {
        let (calls, cwd) = read_file(p, f).ok()?;
        let keep = !calls.is_empty() && project_wanted(root, p, cwd.as_deref(), &f.projects);
        keep.then_some(calls)
    });
    let mut log = Log {
        ctxs: Vec::new(),
        calls: Vec::new(),
    };
    // A session can start with a copy of another's history (the same tool_use ids and
    // times): each call is counted once, in the first file that has it.
    let mut seen: HashSet<String> = HashSet::new();
    for (i, reads) in results {
        let file_label = label(root, &files[i]);
        // The file's own calls first, then each subagent whose lines sit in it.
        let mut ctx_of: Vec<(Option<String>, usize)> = Vec::new();
        let mut grouped: Vec<Vec<Call>> = Vec::new();
        for r in reads {
            if let Some(id) = r.id
                && !seen.insert(id)
            {
                continue;
            }
            let k = match ctx_of.iter().position(|(a, _)| *a == r.agent) {
                Some(k) => k,
                None => {
                    ctx_of.push((r.agent.clone(), grouped.len()));
                    grouped.push(Vec::new());
                    grouped.len() - 1
                }
            };
            grouped[k].push(r.call);
        }
        let mut order: Vec<usize> = (0..ctx_of.len()).collect();
        order.sort_by_key(|&k| ctx_of[k].0.is_some());
        for k in order {
            let ctx = log.ctxs.len();
            log.ctxs.push(Ctx {
                label: match &ctx_of[k].0 {
                    None => file_label.clone(),
                    Some(a) => format!("{file_label}/agent-{a}"),
                },
            });
            log.calls
                .extend(std::mem::take(&mut grouped[k]).into_iter().map(|mut c| {
                    c.ctx = ctx;
                    c
                }));
        }
    }
    Ok(log)
}
