//! Codex session logs (`~/.codex/sessions/YYYY/MM/DD/rollout-<local time>-<uuid>.jsonl` and
//! `~/.codex/archived_sessions/`), read as the same `Session`s `talk.rs` makes from Claude
//! Code's logs, for `search` and `show`.
//!
//! The logs are big (nearly all tool output), so a file is not read
//! as JSON line by line. A line's first bytes say what it is (`"payload":{"type":"message"`);
//! only messages (and, for `show`, tool calls) are parsed, and for `search` only the messages
//! that pass `Prefilter`, the pattern's own literal looked for in the raw line.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use memchr::memmem;
use regex_lite::Regex;
use serde_json::Value;

use crate::cache::{self, Cache, Extract, Need, Slot};
use crate::lines;
use crate::load::{self, Filter};
use crate::needle::Prefilter;
use crate::talk::{self, Event, Session, Source, Who};
use crate::time;

/// Hours a file name's clock can be ahead of UTC (the name is the local time of the
/// machine that wrote the log), plus an hour of slack.
const NAME_SLACK: i64 = 15 * 3600;

pub fn default_roots() -> Vec<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    ["sessions", "archived_sessions"]
        .iter()
        .map(|d| PathBuf::from(format!("{home}/.codex/{d}")))
        .collect()
}

// ---------------------------------------------------------------------------------------
// Which files

/// `rollout-2026-06-22T11-24-00-019eed24-4fae-7653-9a71-2b3b219b4dcd` → the session id.
fn id_of(stem: &str) -> String {
    let n = stem.chars().count();
    if stem.starts_with("rollout-") && n >= 36 {
        stem.chars().skip(n - 36).collect()
    } else {
        stem.to_string()
    }
}

/// The time in the file name, read as UTC. It is the local time the session started.
fn name_time(stem: &str) -> Option<i64> {
    let ts = stem.strip_prefix("rollout-")?.get(..19)?;
    let (date, clock) = ts.split_once('T')?;
    time::parse_iso(&format!("{date}T{}Z", clock.replace('-', ":")))
}

fn stem_of(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The files the window and the filters leave. One session may sit in several places
/// (`sessions/` and `archived_sessions/`, a live directory and its copy): the largest
/// file of an id is read, the first listed when sizes are equal.
pub fn select_files(roots: &[PathBuf], f: &Filter) -> io::Result<Vec<PathBuf>> {
    Ok(list(roots, f)?.0)
}

/// The files `select_files` gives, and every log file under the roots.
fn list(roots: &[PathBuf], f: &Filter) -> io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut all = Vec::new();
    for r in roots {
        fs::read_dir(r).map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", r.display())))?;
        load::jsonl_files(r, &mut all);
    }
    let mut best: HashMap<String, (u64, PathBuf)> = HashMap::new();
    for p in all.iter().cloned() {
        let s = p.to_string_lossy();
        if f.excludes.iter().any(|x| s.contains(x.as_str())) {
            continue;
        }
        let stem = stem_of(&p);
        // Events are never earlier than the session's start, and never later than the
        // file's last write.
        if load::mtime(&p) < f.since {
            continue;
        }
        if name_time(&stem).is_some_and(|t| t - NAME_SLACK >= f.until) {
            continue;
        }
        let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        let e = best.entry(id_of(&stem)).or_insert((size, p.clone()));
        if size > e.0 {
            *e = (size, p);
        }
    }
    let mut files: Vec<PathBuf> = best.into_values().map(|(_, p)| p).collect();
    files.sort();
    Ok((files, all))
}

/// The files a `show SESSION` may mean: those whose id contains `spec`.
pub fn session_files(roots: &[PathBuf], f: &Filter, spec: &str) -> io::Result<Vec<PathBuf>> {
    let want = spec.to_lowercase();
    Ok(select_files(roots, f)?
        .into_iter()
        .filter(|p| id_of(&stem_of(p)).to_lowercase().contains(&want))
        .collect())
}

// ---------------------------------------------------------------------------------------
// What a line is

#[derive(PartialEq, Eq)]
enum Kind {
    Message,
    Call,
    Other,
}

/// What a line is, from its first bytes: `…,"type":"response_item","payload":{"type":"message"…`.
fn kind(head: &[u8]) -> Kind {
    const KEY: &[u8] = b"\"type\":\"response_item\",\"payload\":{\"type\":\"";
    let Some(i) = memmem::find(head, KEY) else {
        return Kind::Other;
    };
    let rest = &head[i + KEY.len()..];
    if rest.starts_with(b"message\"") {
        Kind::Message
    } else if rest.starts_with(b"function_call\"") || rest.starts_with(b"custom_tool_call\"") {
        Kind::Call
    } else {
        Kind::Other
    }
}

// ---------------------------------------------------------------------------------------
// What a person said

/// Blocks the harness puts inside a user message, with or without attributes. Removed
/// wherever they sit.
const INJECTED: &[&str] = &[
    "in-app-browser-context",
    "environment_context",
    "recommended_plugins",
    "subagent_notification",
    "turn_aborted",
    "codex_internal_context",
    "heartbeat",
    "skill",
    "realtime_delegation",
    "external_codex_apps_open_page",
    "image",
];

/// A text block that starts like this was written by the harness, not typed.
const INJECTED_STARTS: &[&str] = &[
    "# AGENTS.md instructions",
    "<INSTRUCTIONS>",
    "Here is a list of plugins",
    "# In app browser:",
    "[User attached one or more files",
    "[coordinator] ",
    "[oracle-completion:",
    "The next image shows the browser",
    "<image",
    "</image>",
];

/// The attachment list and the IDE context come first; what was typed follows this line
/// (`## My request:` in logs written by the phone app).
fn request_marker() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"## My request(?: for Codex)?:").unwrap())
}

/// Prompts another program sent in the user's place.
const SENT_STARTS: &[&str] = &["You are working inside Orca"];

fn injected_blocks() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let alts: Vec<String> = INJECTED
            .iter()
            .map(|t| format!(r"<{t}(?:\s[^>]*)?>.*?</{t}>"))
            .collect();
        Regex::new(&format!("(?s){}", alts.join("|"))).unwrap()
    })
}

fn answer_blocks() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new("(?s)<send_user_message_question_reply>(.*?)</send_user_message_question_reply>")
            .unwrap()
    })
}

/// The answers a person gave to the assistant's questions (the app writes them as a
/// block of JSON), in place of the block.
fn answers(text: &str) -> (String, Vec<String>) {
    let mut given = Vec::new();
    for c in answer_blocks().captures_iter(text) {
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(c[1].trim()) {
            given.extend(
                items
                    .iter()
                    .filter_map(|i| i.get("answer").and_then(Value::as_str))
                    .map(str::to_string),
            );
        }
    }
    (answer_blocks().replace_all(text, "").into_owned(), given)
}

/// The part of a user message a person typed, or `None` if there is none. A message is
/// several text blocks; the harness's blocks are separate from the typed one.
pub fn human_text(blocks: &[&str]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for b in blocks {
        let (rest, given) = answers(b);
        parts.extend(given);
        let rest = injected_blocks().replace_all(&rest, "");
        let t = rest.trim();
        if t.is_empty() || INJECTED_STARTS.iter().any(|p| t.starts_with(p)) {
            continue;
        }
        let t = match request_marker().find(t) {
            Some(m) => t[m.end()..].trim(),
            // An attachment list with nothing typed after it.
            None if t.starts_with("# Files mentioned by the user:") => continue,
            None => t,
        };
        if !t.is_empty() {
            parts.push(t.to_string());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

// ---------------------------------------------------------------------------------------
// Reading a session

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// The text blocks of a message's content (`input_text` / `output_text`).
fn texts(content: &Value) -> Vec<&str> {
    content
        .as_array()
        .map(|a| a.iter().filter_map(|b| str_field(b, "text")).collect())
        .unwrap_or_default()
}

/// `[exec_command] first line of the command…` for a `function_call` (arguments are a JSON
/// string) or a `custom_tool_call` (the input is the code or the patch). Results are never
/// shown.
fn call_line(p: &Value) -> String {
    let name = str_field(p, "name").unwrap_or("?");
    let detail = if let Some(input) = str_field(p, "input") {
        if name == "apply_patch" {
            input
                .lines()
                .find(|l| l.starts_with("*** ") && l.contains(" File: "))
                .unwrap_or("")
                .trim_start_matches("*** ")
                .to_string()
        } else if name == "exec"
            && let Some(c) = exec_cmd().captures(input)
        {
            // The code-mode tool wraps the shell command: `tools.exec_command({cmd: "ls"})`.
            return talk::note("exec_command", &c[1]);
        } else {
            input.to_string()
        }
    } else {
        let args: Value = str_field(p, "arguments")
            .and_then(|a| serde_json::from_str(a).ok())
            .unwrap_or(Value::Null);
        [
            "cmd",
            "command",
            "path",
            "file_path",
            "query",
            "pattern",
            "url",
        ]
        .iter()
        .find_map(|k| match args.get(*k) {
            Some(Value::String(s)) => Some(s.clone()),
            // The older shell tool: ["bash", "-lc", "the command"].
            Some(Value::Array(a)) => a.last().and_then(Value::as_str).map(str::to_string),
            _ => None,
        })
        .unwrap_or_default()
    };
    talk::note(name, &detail)
}

fn exec_cmd() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"exec_command\(\{\s*cmd\s*:\s*["'`]([^"'`\n]*)"#).unwrap())
}

/// Takes what `search`, `show` and `timeline` use out of a Codex log, from byte `from` on,
/// into `ex`; returns where the next read should start. `tools` also takes a note for each
/// tool call; `pre` lets messages that cannot match be passed over unparsed. What counts
/// as a person's words, and the rest, is decided here and nowhere else: `--raw` and the
/// cache both come through this function.
pub fn read_lines(
    path: &Path,
    from: u64,
    ex: &mut Extract,
    tools: bool,
    pre: Option<&Prefilter>,
) -> io::Result<u64> {
    lines::scan_from(
        path,
        from,
        |n, head| {
            n == 0 || matches!(kind(head), Kind::Message) || tools && kind(head) == Kind::Call
        },
        |n, line| codex_line(n, line, pre, ex),
    )
}

fn codex_line(n: usize, line: &[u8], pre: Option<&Prefilter>, ex: &mut Extract) {
    let head = &line[..line.len().min(lines::HEAD)];
    if n == 0 && memmem::find(head, b"\"type\":\"session_meta\"").is_some() {
        meta(line, ex);
        return;
    }
    let kind = kind(head);
    if kind == Kind::Other {
        return;
    }
    let is_message = kind == Kind::Message;
    if is_message
        && let Some(p) = pre
        && !p.may_match(line)
    {
        return;
    }
    // A line that is not JSON (half written, damaged) is passed over.
    let Ok(v) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    if v.get("ordinal")
        .and_then(Value::as_u64)
        .is_some_and(|o| o < ex.own_from)
    {
        return;
    }
    let Some(ts) = str_field(&v, "timestamp").and_then(time::parse_iso) else {
        return;
    };
    let Some(p) = v.get("payload") else { return };
    if !is_message {
        ex.events.push(Event {
            ts,
            who: None,
            text: call_line(p),
        });
        return;
    }
    let content = p.get("content").unwrap_or(&Value::Null);
    match str_field(p, "role") {
        Some("assistant") => {
            let text = texts(content).join("\n");
            let text = text.trim();
            if !text.is_empty() {
                ex.events.push(Event {
                    ts,
                    who: Some(Who::Assistant),
                    text: text.to_string(),
                });
            }
        }
        Some("user") => {
            let Some(text) = human_text(&texts(content)) else {
                return;
            };
            let who = if ex.subagent
                || talk::sent_for_a_task(&text)
                || SENT_STARTS.iter().any(|p| text.starts_with(p))
            {
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
        _ => {}
    }
}

fn meta(line: &[u8], ex: &mut Extract) {
    let Ok(v) = serde_json::from_slice::<Value>(line) else {
        return;
    };
    let Some(p) = v.get("payload") else { return };
    ex.cwd = str_field(p, "cwd").map(str::to_string);
    // A subagent's file starts with a copy of its parent's lines, up to this ordinal; and
    // its user lines are the parent's prompts, not a person's words.
    ex.own_from = p
        .get("subagent_history_start_ordinal")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    ex.subagent = p.pointer("/source/subagent").is_some()
        || p.get("parent_thread_id").is_some()
        || str_field(p, "thread_source") == Some("subagent");
}

/// The session an extract stands for, cut to the window.
fn session_of(path: &Path, ex: Extract, f: &Filter) -> Session {
    Session {
        source: Source::Codex,
        project: ex
            .cwd
            .as_deref()
            .map(talk::project_name)
            .unwrap_or_else(|| "?".into()),
        sid: id_of(&stem_of(path)),
        agent: None,
        events: ex
            .events
            .into_iter()
            .filter(|e| e.ts >= f.since && e.ts < f.until)
            .collect(),
        cwds: ex.cwd.into_iter().map(|c| (c, 1)).collect(),
    }
}

fn read_session(
    path: &Path,
    f: &Filter,
    need: Need,
    pre: Option<&Prefilter>,
    slot: Option<&Slot>,
) -> io::Result<Session> {
    let ex = cache::fetch(slot, path, need, pre, |from, ex, tools, pre| {
        read_lines(path, from, ex, tools, pre)
    })?;
    Ok(session_of(path, ex, f))
}

/// Every session in the window with something said in it. With `pre`, messages that cannot
/// match are left out (all `search` looks at).
pub fn load_sessions(
    roots: &[PathBuf],
    f: &Filter,
    need: Need,
    pre: Option<&Prefilter>,
    cache: &Cache,
) -> io::Result<Vec<Session>> {
    let (files, all) = list(roots, f)?;
    for r in roots {
        if let Some(s) = cache.slot("codex", r) {
            let mine: Vec<PathBuf> = all.iter().filter(|p| p.starts_with(r)).cloned().collect();
            s.sweep(&mine);
        }
    }
    let slots = slots(cache, roots);
    let read = load::read_all(&files, |p| {
        let s = read_session(p, f, need, pre, slot_of(&slots, p)).ok()?;
        let keep = !s.events.is_empty()
            && (f.projects.is_empty() || f.projects.iter().any(|x| s.project.contains(x.as_str())));
        keep.then_some(s)
    });
    Ok(read.into_iter().map(|(_, s)| s).collect())
}

fn slots(cache: &Cache, roots: &[PathBuf]) -> Vec<(PathBuf, Slot)> {
    roots
        .iter()
        .filter_map(|r| Some((r.clone(), cache.slot("codex", r)?)))
        .collect()
}

/// The cache slot of the root a log file sits under.
fn slot_of<'a>(slots: &'a [(PathBuf, Slot)], path: &Path) -> Option<&'a Slot> {
    slots
        .iter()
        .find(|(r, _)| path.starts_with(r))
        .map(|(_, s)| s)
}

/// Reads one file as a session, tool calls included.
pub fn load_one(roots: &[PathBuf], path: &Path, f: &Filter, cache: &Cache) -> io::Result<Session> {
    let slots = slots(cache, roots);
    let need = Need {
        tools: true,
        cwds: false,
    };
    read_session(path, f, need, None, slot_of(&slots, path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_text_survives_and_what_the_app_adds_does_not() {
        assert_eq!(
            human_text(&["do the thing"]).as_deref(),
            Some("do the thing")
        );
        assert_eq!(
            human_text(&[
                "# AGENTS.md instructions for /x\n\n<INSTRUCTIONS>\nrules\n</INSTRUCTIONS>",
                "<environment_context>\n  <cwd>/x</cwd>\n</environment_context>",
            ]),
            None
        );
        // Blocks apart from the typed one, and a block with the tag and the words together.
        let got = human_text(&[
            "<in-app-browser-context source=\"ambient\">\nopen tabs\n</in-app-browser-context>",
            "# In app browser:\n- The user has the in-app browser open",
            "what I typed\n<turn_aborted>\nThe user interrupted\n</turn_aborted>",
            "<image name=[Image #1]>",
            "</image>",
        ]);
        assert_eq!(got.as_deref(), Some("what I typed"));
    }

    #[test]
    fn the_request_after_an_attachment_list_is_what_was_typed() {
        let raw = "# Files mentioned by the user:\n\n## a.png: /tmp/a.png\n\n## My request for Codex:\n\nlook at this";
        assert_eq!(human_text(&[raw]).as_deref(), Some("look at this"));
        let phone = "# Files mentioned by the user:\n\n## Photo 1.jpg: /tmp/p.jpg\n\n## My request:\nlook at that";
        assert_eq!(human_text(&[phone]).as_deref(), Some("look at that"));
        assert_eq!(
            human_text(&["# Files mentioned by the user:\n\n## a.png: /tmp/a.png"]),
            None
        );
        let none = "# Files mentioned by the user:\n\n## a.png: /tmp/a.png\n\n## My request for Codex:\n\n";
        assert_eq!(human_text(&[none]), None);
    }

    #[test]
    fn an_answer_to_a_question_is_the_persons_but_the_question_is_not() {
        let raw = "<send_user_message_question_reply>\n[{\"questionItemId\":\"q\",\"question\":\"Which?\",\"answer\":\"the first one\"}]\n</send_user_message_question_reply>";
        assert_eq!(human_text(&[raw]).as_deref(), Some("the first one"));
        assert_eq!(
            human_text(&[
                "<send_user_message_question_reply>not json</send_user_message_question_reply>"
            ]),
            None
        );
    }

    #[test]
    fn ids_and_times_come_from_the_file_name() {
        let stem = "rollout-2026-06-22T11-24-00-019eed24-4fae-7653-9a71-2b3b219b4dcd";
        assert_eq!(id_of(stem), "019eed24-4fae-7653-9a71-2b3b219b4dcd");
        assert_eq!(name_time(stem), time::parse_iso("2026-06-22T11:24:00Z"));
        assert_eq!(id_of("odd-name"), "odd-name");
        assert_eq!(name_time("odd-name"), None);
    }

    #[test]
    fn a_line_is_known_by_its_first_bytes() {
        let m = br#"{"timestamp":"2026-06-22T02:24:13.083Z","ordinal":3,"type":"response_item","payload":{"type":"message","role":"user""#;
        assert!(kind(m) == Kind::Message);
        let c = br#"{"timestamp":"t","ordinal":3,"type":"response_item","payload":{"type":"function_call","name":"x""#;
        assert!(kind(c) == Kind::Call);
        let o = br#"{"timestamp":"t","ordinal":3,"type":"response_item","payload":{"type":"function_call_output","call_id":"x""#;
        assert!(kind(o) == Kind::Other);
        let e = br#"{"timestamp":"t","ordinal":3,"type":"event_msg","payload":{"type":"message""#;
        assert!(kind(e) == Kind::Other);
    }
}
