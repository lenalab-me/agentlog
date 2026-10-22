//! The conversation in a session log, for `search` and `show`: what people and the
//! assistant said (text blocks, in order), and a one-line note for each tool call.
//!
//! Only what a person typed counts as the user's words. A `user` line also carries tool
//! results and text the harness injected (system reminders, task notifications, the
//! output of local commands); `human_text` is the one place that tells them apart.
//! Thinking blocks, tool inputs and tool results are not conversation and are left out.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::sync::OnceLock;

use memchr::memmem;
use regex_lite::Regex;
use serde_json::Value;

use crate::cache::{self, Cache, Extract, Need, Slot};
use crate::jsonl;
use crate::lines;
use crate::load::{self, Filter, short_project};
use crate::needle::Prefilter;
use crate::time;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Who {
    User,
    Assistant,
    /// The first prompt of a subagent: the parent wrote it, a person did not.
    Task,
}

impl Who {
    pub fn name(self) -> &'static str {
        match self {
            Who::User => "user",
            Who::Assistant => "assistant",
            Who::Task => "task",
        }
    }
}

pub struct Event {
    pub ts: i64,
    /// `None` is a tool call; `text` is then `[Bash] the command's first line…`.
    pub who: Option<Who>,
    pub text: String,
}

/// Which agent's log a session came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Claude,
    Codex,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Claude => "claude",
            Source::Codex => "codex",
        }
    }
}

pub struct Session {
    pub source: Source,
    pub project: String,
    pub sid: String,
    /// Set for a subagent's file: the agent id, with the parent in `sid`.
    pub agent: Option<String>,
    pub events: Vec<Event>,
    /// Where the session was working: each `cwd` its lines carried in the window, with the
    /// number of lines. `timeline` uses it to tell which repository a session belongs to.
    pub cwds: HashMap<String, u32>,
}

impl Session {
    /// What `search` prints and `show` takes. Claude: 8 characters of the session, then
    /// `/` and 8 of the agent for a subagent. Codex: the last 8 characters (its ids are
    /// UUIDv7, so sessions of the same period share the first ones).
    pub fn id(&self) -> String {
        let short = |s: &str| s.chars().take(8).collect::<String>();
        if self.source == Source::Codex {
            let n = self.sid.chars().count();
            return self.sid.chars().skip(n.saturating_sub(8)).collect();
        }
        match &self.agent {
            Some(a) => format!("{}/{}", short(&self.sid), short(a)),
            None => short(&self.sid),
        }
    }
}

// ---------------------------------------------------------------------------------------
// What a person said

/// Blocks the harness puts inside a user message. Removed wherever they sit.
const INJECTED: &[&str] = &[
    "system-reminder",
    "local-command-caveat",
    "local-command-stdout",
    "local-command-stderr",
    "bash-input",
    "bash-stdout",
    "bash-stderr",
    "task-notification",
];

/// A message that starts like this was written by the harness, not typed.
const INJECTED_STARTS: &[&str] = &[
    "<command-message>",
    "<scheduled-task",
    "[Request interrupted by user",
    "[SYSTEM NOTIFICATION",
    "[Image: source:",
    "[Image: original",
    "This session is being continued from a previous conversation",
    "Base directory for this skill:",
];

/// A prompt that a tool or the orchestrator sent in the user's place (the T3 Code app
/// hands tasks to subagents and asks for titles). The log marks it like a typed one, so
/// only its wording tells; it counts as `task`, not as the user's words.
pub fn sent_for_a_task(text: &str) -> bool {
    const STARTS: &[&str] = &[
        "Delegated task",
        "Act as the ",
        "Generate a title ",
        "You generate concise git branch names",
        "Continue where you left off",
    ];
    STARTS.iter().any(|p| text.starts_with(p))
        || text.starts_with("You have ") && text.contains("orchestration message")
}

fn injected_blocks() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alts: Vec<String> = INJECTED.iter().map(|t| format!("<{t}>.*?</{t}>")).collect();
        Regex::new(&format!("(?s){}", alts.join("|"))).unwrap()
    })
}

fn slash_command() -> &'static (Regex, Regex) {
    static RE: OnceLock<(Regex, Regex)> = OnceLock::new();
    RE.get_or_init(|| {
        (
            Regex::new("(?s)<command-name>(.*?)</command-name>").unwrap(),
            Regex::new("(?s)<command-args>(.*?)</command-args>").unwrap(),
        )
    })
}

/// The part of a user message a person typed, or `None` if there is none.
/// A slash command is typed, so `/name args` stands for the three tags that carry it.
pub fn human_text(raw: &str) -> Option<String> {
    let (name, args) = slash_command();
    if let Some(c) = name.captures(raw) {
        let a = args
            .captures(raw)
            .map(|c| c[1].trim().to_string())
            .unwrap_or_default();
        return Some(format!("{} {a}", c[1].trim()).trim().to_string());
    }
    let text = injected_blocks().replace_all(raw, "");
    let text = text.trim();
    if text.is_empty() || INJECTED_STARTS.iter().any(|p| text.starts_with(p)) {
        return None;
    }
    Some(text.to_string())
}

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn flag(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// Text of a message's content: a string, or the text blocks of an array.
fn blocks_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter(|b| str_field(b, "type") == Some("text"))
            .filter_map(|b| str_field(b, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `[Bash] first line of the command…`. Results are never shown.
fn tool_line(name: &str, input: &Value) -> String {
    let detail = match name {
        "Bash" => str_field(input, "command"),
        "Read" | "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => str_field(input, "file_path"),
        "Grep" | "Glob" => str_field(input, "pattern"),
        _ => ["description", "query", "pattern", "url", "path", "name"]
            .iter()
            .find_map(|k| str_field(input, k)),
    }
    .unwrap_or("");
    note(name, detail)
}

/// `[name] first line of the detail…`, cut at 100 characters.
pub fn note(name: &str, detail: &str) -> String {
    let first = detail
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut s: String = first.chars().take(100).collect();
    if first.chars().count() > 100 || detail.trim().lines().count() > 1 {
        s.push('…');
    }
    if s.is_empty() {
        format!("[{name}]")
    } else {
        format!("[{name}] {s}")
    }
}

// ---------------------------------------------------------------------------------------
// Reading

/// `/Users/me/project/prod/shop/web` → `shop/web`: the session's first
/// working directory, without the home directory and a leading `project/prod/` or
/// `project/` (the author's layout; other paths are shown as they are).
pub fn project_name(cwd: &str) -> String {
    project_name_in(cwd, &std::env::var("HOME").unwrap_or_default())
}

/// `project_name` with `home` for the home directory: it is left out wherever it is
/// (`/root`), and so is a home under `/Users` or `/home` (another machine's, over ssh).
fn project_name_in(cwd: &str, home: &str) -> String {
    let home = home.trim_end_matches('/');
    let in_home = !home.is_empty()
        && cwd
            .strip_prefix(home)
            .is_some_and(|r| r.is_empty() || r.starts_with('/'));
    let rest = if in_home { &cwd[home.len()..] } else { cwd };
    let mut parts: Vec<&str> = rest.split('/').filter(|s| !s.is_empty()).collect();
    if !in_home && parts.len() >= 2 && matches!(parts[0], "Users" | "home") {
        parts.drain(..2);
    }
    if parts.len() > 1 && parts[0] == "project" {
        parts.remove(0);
        if parts.len() > 1 && parts[0] == "prod" {
            parts.remove(0);
        }
    }
    if parts.is_empty() {
        "~".into()
    } else {
        parts.join("/")
    }
}

fn is_agent_file(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy();
    stem.strip_prefix("agent-").map(str::to_string)
}

/// The session a file belongs to and the agent id for a subagent's file. A subagent
/// file sits under `<session>/subagents/`, or (older logs) next to the sessions with the
/// parent named inside; `inner` is that name from the file's lines.
fn ids(path: &Path, inner: Option<&str>) -> (String, Option<String>) {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some(agent) = is_agent_file(path) else {
        return (stem, None);
    };
    let dir = path.parent();
    let under_subagents = dir
        .and_then(Path::file_name)
        .is_some_and(|n| n == "subagents");
    let sid = if under_subagents {
        dir.and_then(Path::parent)
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
    } else {
        inner.map(str::to_string)
    };
    (sid.unwrap_or_else(|| "?".into()), Some(agent))
}

/// The session id and agent a file stands for, read from its first lines if need be.
pub fn file_ids(path: &Path) -> (String, Option<String>) {
    let mut inner = None;
    if is_agent_file(path).is_some()
        && let Ok(f) = fs::File::open(path)
    {
        for line in BufReader::new(f).lines().take(40).map_while(Result::ok) {
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && let Some(s) = str_field(&v, "sessionId")
            {
                inner = Some(s.to_string());
                break;
            }
        }
    }
    ids(path, inner.as_deref())
}

fn has(line: &[u8], needle: &[u8]) -> bool {
    memmem::find(line, needle).is_some()
}

/// Takes what `search`, `show` and `timeline` use out of a Claude Code log, from byte `from`
/// on, into `ex`; returns where the next read should start. `tools` also takes a note for
/// each tool call. What counts as a person's words, and the rest, is decided here and
/// nowhere else: `--raw` and the cache both come through this function.
pub fn read_lines(path: &Path, from: u64, ex: &mut Extract, tools: bool) -> io::Result<u64> {
    let agent_file = is_agent_file(path).is_some();
    lines::scan_from(
        path,
        from,
        |_, _| true,
        |_, line| claude_line(line, agent_file, tools, ex),
    )
}

fn claude_line(line: &[u8], agent_file: bool, tools: bool, ex: &mut Extract) {
    // Cheap skips: no text or tool call in the line, or a tool result with no text.
    let holds_text = has(line, b"\"text\"") || has(line, b"\"content\":\"");
    if !(holds_text || tools && has(line, b"\"tool_use\"")) {
        return;
    }
    if has(line, b"\"type\":\"tool_result\"") && !has(line, b"\"type\":\"text\"") {
        return;
    }
    let Some(v) = jsonl::parse(line) else {
        return;
    };
    let kind = str_field(&v, "type");
    if kind != Some("user") && kind != Some("assistant") {
        return;
    }
    // Only a line with words in it says where the session worked and what it is called.
    if holds_text {
        if ex.cwd.is_none() {
            ex.cwd = str_field(&v, "cwd").map(str::to_string);
        }
        if ex.inner.is_none() {
            ex.inner = str_field(&v, "sessionId").map(str::to_string);
        }
    }
    let Some(ts) = str_field(&v, "timestamp").and_then(time::parse_iso) else {
        return;
    };
    if holds_text && let Some(c) = str_field(&v, "cwd") {
        ex.cwd_lines.entry(c.to_string()).or_default().push(ts);
    }
    let Some(content) = v.pointer("/message/content") else {
        return;
    };
    if kind == Some("assistant") {
        if str_field(&v["message"], "model") == Some("<synthetic>") {
            return;
        }
        let Some(blocks) = content.as_array() else {
            return;
        };
        for b in blocks {
            match str_field(b, "type") {
                Some("text") => {
                    let t = str_field(b, "text").unwrap_or("").trim();
                    if !t.is_empty() {
                        ex.events.push(Event {
                            ts,
                            who: Some(Who::Assistant),
                            text: t.to_string(),
                        });
                    }
                }
                Some("tool_use") if tools => ex.events.push(Event {
                    ts,
                    who: None,
                    text: tool_line(
                        str_field(b, "name").unwrap_or("?"),
                        b.get("input").unwrap_or(&Value::Null),
                    ),
                }),
                _ => {}
            }
        }
        return;
    }
    // A person's line: not a tool result, not meta, not sent by the system.
    if flag(&v, "isMeta")
        || flag(&v, "isCompactSummary")
        || flag(&v, "isVisibleInTranscriptOnly")
        || str_field(&v, "promptSource") == Some("system")
        || matches!(
            str_field(&v, "turnOrigin"),
            Some("task_notification" | "peer")
        )
    {
        return;
    }
    let Some(text) = human_text(&blocks_text(content)) else {
        return;
    };
    let who = if agent_file || flag(&v, "isSidechain") || sent_for_a_task(&text) {
        Who::Task
    } else {
        Who::User
    };
    ex.events.push(Event {
        ts,
        who: Some(who),
        text,
    });
}

/// The session an extract stands for, cut to the window.
fn session_of(root: &Path, path: &Path, ex: Extract, f: &Filter) -> Session {
    let (sid, agent) = ids(path, ex.inner.as_deref());
    let folder = load::folder_of(root, path);
    let in_window = |ts: i64| ts >= f.since && ts < f.until;
    let cwds = ex
        .cwd_lines
        .iter()
        .filter_map(|(c, times)| {
            let n = times.iter().filter(|&&t| in_window(t)).count();
            (n > 0).then(|| (c.clone(), n as u32))
        })
        .collect();
    Session {
        source: Source::Claude,
        project: ex
            .cwd
            .as_deref()
            .map(project_name)
            .unwrap_or_else(|| short_project(&folder)),
        sid,
        agent,
        events: ex.events.into_iter().filter(|e| in_window(e.ts)).collect(),
        cwds,
    }
}

fn read_session(
    root: &Path,
    path: &Path,
    f: &Filter,
    need: Need,
    pre: Option<&Prefilter>,
    slot: Option<&Slot>,
) -> io::Result<Session> {
    let ex = cache::fetch(slot, path, need, pre, |from, ex, tools, _| {
        read_lines(path, from, ex, tools)
    })?;
    Ok(session_of(root, path, ex, f))
}

fn project_matches(s: &Session, folder: &str, projects: &[String]) -> bool {
    projects.is_empty()
        || projects
            .iter()
            .any(|p| folder.contains(p.as_str()) || s.project.contains(p.as_str()))
}

/// Every session in the window with something said in it. `--project` matches the
/// folder name or the working directory shown, so it is checked after reading.
pub fn load_sessions(
    root: &Path,
    f: &Filter,
    need: Need,
    pre: Option<&Prefilter>,
    cache: &Cache,
) -> io::Result<Vec<Session>> {
    let wide = Filter {
        since: f.since,
        until: f.until,
        projects: Vec::new(),
        excludes: f.excludes.clone(),
    };
    let all = load::all_files(root)?;
    let slot = cache.slot("claude", root);
    if let Some(s) = &slot {
        s.sweep(&all);
    }
    let files = load::select_from(root, &all, &wide);
    let read = load::read_all(&files, |p| {
        read_session(root, p, &wide, need, pre, slot.as_ref()).ok()
    });
    let mut seen = HashSet::new();
    Ok(read
        .into_iter()
        .filter_map(|(i, mut s)| {
            once(&mut s, &mut seen);
            let keep = !s.events.is_empty()
                && project_matches(&s, &load::folder_of(root, &files[i]), &f.projects);
            keep.then_some(s)
        })
        .collect())
}

/// Leaves out what an earlier session already had: a session can start with a copy of
/// another's history, the same lines at the same times. Sessions come in file order, so the
/// first file that has a line keeps it; within one session nothing is left out.
fn once(s: &mut Session, seen: &mut HashSet<(Option<Who>, i64, u64)>) {
    let key = |e: &Event| {
        let mut h = DefaultHasher::new();
        e.text.hash(&mut h);
        (e.who, e.ts, h.finish())
    };
    let keys: Vec<_> = s.events.iter().map(key).collect();
    let mut k = keys.iter();
    s.events.retain(|_| !seen.contains(k.next().unwrap()));
    seen.extend(keys);
}

/// `SESSION` as `show` takes it: a session id prefix, or `prefix/agent-prefix`.
/// The files it may mean, before any is read.
pub fn session_files(root: &Path, f: &Filter, spec: &str) -> io::Result<Vec<std::path::PathBuf>> {
    let (sid, agent) = match spec.split_once('/') {
        Some((s, a)) => (s, Some(a)),
        None => (spec, None),
    };
    let all = load::select_files(
        root,
        &Filter {
            since: f.since,
            until: f.until,
            projects: Vec::new(),
            excludes: f.excludes.clone(),
        },
    )?;
    Ok(all
        .into_iter()
        .filter(|p| {
            let (s, a) = file_ids(p);
            s.starts_with(sid)
                && match (&a, agent) {
                    (None, None) => true,
                    (Some(a), Some(want)) => a.starts_with(want),
                    _ => false,
                }
        })
        .collect())
}

/// Reads one file as a session, tool calls included.
pub fn load_one(root: &Path, path: &Path, f: &Filter, cache: &Cache) -> io::Result<Session> {
    let slot = cache.slot("claude", root);
    let need = Need {
        tools: true,
        cwds: false,
    };
    read_session(root, path, f, need, None, slot.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_survives_and_injected_text_does_not() {
        let plain = "do the thing";
        assert_eq!(human_text(plain).as_deref(), Some(plain));
        let mixed = "<system-reminder>\nrules\n</system-reminder>\nwhat I typed";
        assert_eq!(human_text(mixed).as_deref(), Some("what I typed"));
        assert_eq!(
            human_text("<system-reminder>only this</system-reminder>"),
            None
        );
        assert_eq!(
            human_text("<task-notification><x/></task-notification>"),
            None
        );
        assert_eq!(
            human_text("[Request interrupted by user for tool use]"),
            None
        );
        assert_eq!(
            human_text("<local-command-stdout>ok</local-command-stdout>"),
            None
        );
    }

    #[test]
    fn a_slash_command_is_what_was_typed() {
        let raw = "<command-name>/review</command-name>";
        assert_eq!(human_text(raw).as_deref(), Some("/review"));
        let raw = "<command-message>x</command-message>\n<command-name>/review</command-name>\n<command-args>the branch</command-args>";
        assert_eq!(human_text(raw).as_deref(), Some("/review the branch"));
    }

    #[test]
    fn prompts_sent_by_the_app_are_tasks_not_the_users_words() {
        assert!(sent_for_a_task(
            "Act as the review sub-agent for this task."
        ));
        assert!(sent_for_a_task("Delegated task node:x"));
        assert!(sent_for_a_task("You have 2 orchestration messages. Run it"));
        assert!(!sent_for_a_task("Please act as the reviewer"));
        assert!(!sent_for_a_task("You have a point about the design"));
    }

    #[test]
    fn the_home_directory_is_left_out_wherever_it_is() {
        assert_eq!(project_name_in("/root/project/x", "/root"), "x");
        assert_eq!(project_name_in("/root", "/root/"), "~");
        assert_eq!(project_name_in("/rooted/x", "/root"), "rooted/x");
        assert_eq!(project_name_in("/home/u/shop", "/root"), "shop");
        assert_eq!(project_name_in("/srv/shop", ""), "srv/shop");
    }

    #[test]
    fn project_names_drop_home_and_project_prod() {
        assert_eq!(
            project_name("/Users/me/project/prod/shop/web"),
            "shop/web"
        );
        assert_eq!(project_name("/home/x/project/web"), "web");
        assert_eq!(project_name("/Users/me"), "~");
        assert_eq!(project_name("/Users/me/.t3/scratch/a"), ".t3/scratch/a");
    }

    #[test]
    fn ids_mark_a_subagent() {
        let p = Path::new("/r/proj/abcdef12-0000/subagents/agent-1234567890abc.jsonl");
        assert_eq!(
            ids(p, None),
            (
                "abcdef12-0000".to_string(),
                Some("1234567890abc".to_string())
            )
        );
        let flat = Path::new("/r/proj/agent-1234567890abc.jsonl");
        assert_eq!(
            ids(flat, Some("abcdef12-0000")),
            (
                "abcdef12-0000".to_string(),
                Some("1234567890abc".to_string())
            )
        );
        let main = Path::new("/r/proj/abcdef12-0000.jsonl");
        assert_eq!(ids(main, None), ("abcdef12-0000".to_string(), None));
    }

    #[test]
    fn tool_lines_keep_the_first_line_only() {
        let line = tool_line("Bash", &serde_json::json!({"command": "git status\nrm x"}));
        assert_eq!(line, "[Bash] git status…");
        assert_eq!(
            tool_line("Read", &serde_json::json!({"file_path": "/a/b"})),
            "[Read] /a/b"
        );
        assert_eq!(tool_line("Foo", &serde_json::json!({})), "[Foo]");
    }
}
