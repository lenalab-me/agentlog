//! The conversation in a session log, for `search` and `show`: what people and the
//! assistant said (text blocks, in order), and a one-line note for each tool call.
//!
//! Only what a person typed counts as the user's words. A `user` line also carries tool
//! results and text the harness injected (system reminders, task notifications, the
//! output of local commands); `human_text` is the one place that tells them apart.
//! Thinking blocks, tool inputs and tool results are not conversation and are left out.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::sync::OnceLock;

use regex_lite::Regex;
use serde_json::Value;

use crate::load::{self, Filter, short_project};
use crate::time;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// `/Users/me/project/prod/network/designer` → `network/designer`: the session's first
/// working directory, without the home directory and the usual `project/prod/`.
pub fn project_name(cwd: &str) -> String {
    let mut parts: Vec<&str> = cwd.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 && matches!(parts[0], "Users" | "home") {
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

fn read_session(root: &Path, path: &Path, f: &Filter, tools: bool) -> io::Result<Session> {
    let reader = BufReader::new(fs::File::open(path)?);
    let mut events = Vec::new();
    let mut cwd: Option<String> = None;
    let mut cwds: HashMap<String, u32> = HashMap::new();
    let mut inner: Option<String> = None;
    let agent_file = is_agent_file(path).is_some();
    for line in reader.lines() {
        let line = line?;
        // Cheap skips: no text or tool call in the line, or a tool result with no text.
        let holds_text = line.contains("\"text\"") || line.contains("\"content\":\"");
        if !(holds_text || tools && line.contains("\"tool_use\"")) {
            continue;
        }
        if line.contains("\"type\":\"tool_result\"") && !line.contains("\"type\":\"text\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let kind = str_field(&v, "type");
        if kind != Some("user") && kind != Some("assistant") {
            continue;
        }
        if cwd.is_none() {
            cwd = str_field(&v, "cwd").map(str::to_string);
        }
        if inner.is_none() {
            inner = str_field(&v, "sessionId").map(str::to_string);
        }
        let Some(ts) = str_field(&v, "timestamp").and_then(time::parse_iso) else {
            continue;
        };
        if ts < f.since || ts >= f.until {
            continue;
        }
        if let Some(c) = str_field(&v, "cwd") {
            match cwds.get_mut(c) {
                Some(n) => *n += 1,
                None => {
                    cwds.insert(c.to_string(), 1);
                }
            }
        }
        let Some(content) = v.pointer("/message/content") else {
            continue;
        };
        if kind == Some("assistant") {
            if str_field(&v["message"], "model") == Some("<synthetic>") {
                continue;
            }
            let Some(blocks) = content.as_array() else {
                continue;
            };
            for b in blocks {
                match str_field(b, "type") {
                    Some("text") => {
                        let t = str_field(b, "text").unwrap_or("").trim();
                        if !t.is_empty() {
                            events.push(Event {
                                ts,
                                who: Some(Who::Assistant),
                                text: t.to_string(),
                            });
                        }
                    }
                    Some("tool_use") if tools => events.push(Event {
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
            continue;
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
            continue;
        }
        let Some(text) = human_text(&blocks_text(content)) else {
            continue;
        };
        let who = if agent_file || flag(&v, "isSidechain") || sent_for_a_task(&text) {
            Who::Task
        } else {
            Who::User
        };
        events.push(Event {
            ts,
            who: Some(who),
            text,
        });
    }
    let (sid, agent) = ids(path, inner.as_deref());
    let folder = load::folder_of(root, path);
    Ok(Session {
        source: Source::Claude,
        project: cwd
            .as_deref()
            .map(project_name)
            .unwrap_or_else(|| short_project(&folder)),
        sid,
        agent,
        events,
        cwds,
    })
}

fn project_matches(s: &Session, folder: &str, projects: &[String]) -> bool {
    projects.is_empty()
        || projects
            .iter()
            .any(|p| folder.contains(p.as_str()) || s.project.contains(p.as_str()))
}

/// Every session in the window with something said in it. `--project` matches the
/// folder name or the working directory shown, so it is checked after reading.
pub fn load_sessions(root: &Path, f: &Filter, tools: bool) -> io::Result<Vec<Session>> {
    let wide = Filter {
        since: f.since,
        until: f.until,
        projects: Vec::new(),
        excludes: f.excludes.clone(),
    };
    let files = load::select_files(root, &wide)?;
    let read = load::read_all(&files, |p| {
        let s = read_session(root, p, &wide, tools).ok()?;
        let keep =
            !s.events.is_empty() && project_matches(&s, &load::folder_of(root, p), &f.projects);
        keep.then_some(s)
    });
    Ok(read.into_iter().map(|(_, s)| s).collect())
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
pub fn load_one(root: &Path, path: &Path, f: &Filter) -> io::Result<Session> {
    read_session(root, path, f, true)
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
    fn project_names_drop_home_and_project_prod() {
        assert_eq!(
            project_name("/Users/me/project/prod/network/designer"),
            "network/designer"
        );
        assert_eq!(project_name("/home/x/project/designer"), "designer");
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
