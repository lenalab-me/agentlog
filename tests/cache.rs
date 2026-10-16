//! The cache of `search`, `show` and `timeline` against the logs themselves. The same
//! question is asked with the cache and with `--raw` (which reads the logs whole) after each
//! thing that can happen to a log, and the two answers must be the same. Logs are written by
//! hand with made-up words.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

type Out = (i32, String, String);

const SID1: &str = "11111111-aaaa-4000-8000-000000000001";
const SID2: &str = "22222222-bbbb-4000-8000-000000000002";
const CX1: &str = "019f0000-aaaa-7000-8000-00000000c001";
const CXSUB: &str = "019f0000-aaaa-7000-8000-00000000c002";

// ---------------------------------------------------------------------------------------
// Log lines

fn claude(sid: &str, cwd: &str, ts: &str, kind: &str, message: Value) -> Value {
    json!({"type": kind, "timestamp": ts, "sessionId": sid, "cwd": cwd, "message": message})
}

fn said(sid: &str, cwd: &str, ts: &str, text: &str) -> Value {
    claude(
        sid,
        cwd,
        ts,
        "user",
        json!({"role": "user", "content": text}),
    )
}

fn replied(sid: &str, cwd: &str, ts: &str, blocks: Value) -> Value {
    claude(
        sid,
        cwd,
        ts,
        "assistant",
        json!({"role": "assistant", "content": blocks}),
    )
}

fn jsonl(lines: &[Value]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

fn cx_line(ord: u32, ts: &str, kind: &str, payload: &str) -> String {
    format!(r#"{{"timestamp":"{ts}","ordinal":{ord},"type":"{kind}","payload":{payload}}}"#)
}

fn cx_meta(sid: &str, ts: &str, cwd: &str, extra: &str) -> String {
    cx_line(
        0,
        ts,
        "session_meta",
        &format!(r#"{{"id":"{sid}","cwd":"{cwd}"{extra}}}"#),
    )
}

fn cx_msg(ord: u32, ts: &str, role: &str, text: &str) -> String {
    let kind = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    let block = json!({"type": kind, "text": text});
    cx_line(
        ord,
        ts,
        "response_item",
        &format!(r#"{{"type":"message","role":"{role}","content":[{block}]}}"#),
    )
}

fn cx_call(ord: u32, ts: &str, cmd: &str) -> String {
    let args = json!({"cmd": cmd}).to_string();
    let rest = json!({"name": "exec_command", "arguments": args, "call_id": "c1"}).to_string();
    cx_line(
        ord,
        ts,
        "response_item",
        &format!(r#"{{"type":"function_call",{}"#, &rest[1..]),
    )
}

fn cx_out(ord: u32, ts: &str, text: &str) -> String {
    let rest = json!({"output": text}).to_string();
    cx_line(
        ord,
        ts,
        "response_item",
        &format!(r#"{{"type":"function_call_output",{}"#, &rest[1..]),
    )
}

fn lines(v: &[String]) -> String {
    v.iter().map(|l| format!("{l}\n")).collect()
}

// ---------------------------------------------------------------------------------------
// A directory with logs in it, a cache next to them, and a git repository

struct World {
    dir: tempfile::TempDir,
    repo: PathBuf,
    other: String,
}

impl World {
    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn cache(&self) -> PathBuf {
        self.path("cache")
    }

    fn s1(&self) -> PathBuf {
        self.path("claude/-p-net").join(format!("{SID1}.jsonl"))
    }

    fn s2(&self) -> PathBuf {
        self.path("claude/-p-other").join(format!("{SID2}.jsonl"))
    }

    fn agent(&self) -> PathBuf {
        self.path("claude/-p-net")
            .join(SID1)
            .join("subagents/agent-a1.jsonl")
    }

    fn cx1(&self) -> PathBuf {
        self.path(&format!(
            "codex/sessions/2026/10/02/rollout-2026-10-02T14-00-00-{CX1}.jsonl"
        ))
    }

    fn cx1_archived(&self) -> PathBuf {
        self.path(&format!(
            "codex/archived_sessions/rollout-2026-10-02T14-00-00-{CX1}.jsonl"
        ))
    }

    fn cxsub(&self) -> PathBuf {
        self.path(&format!(
            "codex/sessions/2026/10/02/rollout-2026-10-02T15-00-00-{CXSUB}.jsonl"
        ))
    }

    fn repo_cwd(&self) -> String {
        self.repo.to_str().unwrap().to_string()
    }

    /// `agentlog ARGS` over the world's logs, with the cache or without (`--raw`).
    fn ask(&self, raw: bool, args: &[&str]) -> Out {
        self.ask_with(&self.cache(), raw, args)
    }

    fn ask_with(&self, cache: &Path, raw: bool, args: &[&str]) -> Out {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentlog"));
        cmd.args(args)
            .args(["--root", self.path("claude").to_str().unwrap()])
            .args([
                "--codex-root",
                self.path("codex/sessions").to_str().unwrap(),
            ])
            .args([
                "--codex-root",
                self.path("codex/archived_sessions").to_str().unwrap(),
            ])
            .args(["--source", "all"]);
        if raw {
            cmd.arg("--raw");
        }
        let o = cmd
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("HOME", self.path("home"))
            .env("XDG_CACHE_HOME", cache)
            .env("TZ", "UTC")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    /// The questions asked of the cache and of the logs.
    fn questions(&self) -> Vec<Vec<String>> {
        let repo = self.repo_cwd();
        let q = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        vec![
            q(&["search", ".", "--who", "any", "--limit", "1000", "--json"]),
            q(&["search", "ALPHA|beta", "-i", "--who", "user", "--json"]),
            q(&["search", "zzzz", "--json"]),
            q(&[
                "search",
                ".",
                "--who",
                "any",
                "--since",
                "2026-10-02T08:00Z",
                "--until",
                "2026-10-02T12:00Z",
                "--json",
            ]),
            q(&["search", ".", "--who", "any", "--project", "net", "--json"]),
            q(&[
                "search",
                ".",
                "--who",
                "any",
                "--project",
                "other",
                "--json",
            ]),
            q(&[
                "search",
                ".",
                "--who",
                "any",
                "--exclude",
                "22222222",
                "--json",
            ]),
            q(&[
                "search", "note", "--who", "any", "--source", "codex", "--json",
            ]),
            q(&["show", "11111111", "--full", "--json"]),
            q(&["show", "11111111", "--grep", "second", "--around", "1"]),
            q(&["show", "11111111/a1", "--full", "--json"]),
            q(&["show", "22222222", "--full", "--json"]),
            q(&["show", "0000c001", "--full", "--json"]),
            q(&["show", "0000c002", "--full", "--json"]),
            q(&["timeline", "--repo", &repo, "--who", "any", "--json"]),
            q(&[
                "timeline",
                "--repo",
                &repo,
                "--since",
                "2026-10-02T09:00Z",
                "--json",
            ]),
        ]
    }

    /// The cache and the logs must answer every question alike.
    fn agree(&self, when: &str) {
        for q in self.questions() {
            let args: Vec<&str> = q.iter().map(String::as_str).collect();
            let cached = self.ask(false, &args);
            let raw = self.ask(true, &args);
            if cached != raw {
                panic!(
                    "{when}: `{}` with the cache is not what the logs say\n{}",
                    q.join(" "),
                    first_difference(&cached, &raw)
                );
            }
        }
    }

    /// How many hits `pattern` has among the talk, per `--raw` or the cache.
    fn hits(&self, raw: bool, pattern: &str) -> usize {
        let (code, out, err) = self.ask(raw, &["search", pattern, "--who", "any", "--json"]);
        assert_eq!(code, 0, "{err}");
        out.lines().count()
    }

    /// Both ways of reading find `pattern` this many times.
    fn expect_hits(&self, pattern: &str, n: usize) {
        for raw in [false, true] {
            assert_eq!(
                self.hits(raw, pattern),
                n,
                "`{pattern}` (raw: {raw}) in {:?}",
                self.ask(raw, &["search", pattern, "--who", "any"]).1
            );
        }
    }

    fn cache_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let Ok(top) = fs::read_dir(self.cache().join("agentlog")) else {
            return out;
        };
        for slot in top.flatten() {
            for f in fs::read_dir(slot.path()).unwrap().flatten() {
                out.push(f.path());
            }
        }
        out.sort();
        out
    }
}

/// Where two answers first part, short enough to read.
fn first_difference(cached: &Out, raw: &Out) -> String {
    // From a little before the first character where the two lines part.
    let (la, lb) = (
        cached.1.lines().collect::<Vec<_>>(),
        raw.1.lines().collect::<Vec<_>>(),
    );
    let at = |i: usize| {
        let (x, y) = (
            la.get(i).copied().unwrap_or(""),
            lb.get(i).copied().unwrap_or(""),
        );
        x.chars()
            .zip(y.chars())
            .take_while(|(p, q)| p == q)
            .count()
            .saturating_sub(30)
    };
    if cached.0 != raw.0 {
        return format!("exit status: cache {}, logs {}\n{}", cached.0, raw.0, raw.2);
    }
    let i = (0..la.len().max(lb.len()))
        .find(|&i| la.get(i) != lb.get(i))
        .unwrap_or(0);
    let cut = |s: Option<&&str>| match s {
        None => "(no line)".to_string(),
        Some(s) => s.chars().skip(at(i)).take(140).collect(),
    };
    format!(
        "line {}:\n  cache: …{}\n  logs:  …{}",
        i + 1,
        cut(la.get(i)),
        cut(lb.get(i))
    )
}

fn append(path: &Path, text: &str) {
    let mut f = OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=tester", "-c", "user.email=t@example.com"])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_DATE", "2026-10-02T10:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-10-02T10:00:00Z")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
}

/// Logs of every shape the readers tell apart: typed words and the text the harness adds, a
/// subagent, tool calls, a Codex session in two places, a forked Codex subagent.
fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("work/repo");
    fs::create_dir_all(&repo).unwrap();
    let repo = fs::canonicalize(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "--allow-empty", "-q", "-m", "a commit"]);
    let w = World {
        dir,
        repo,
        other: "/h/elsewhere/other".into(),
    };
    let cwd = w.repo_cwd();
    let read = json!([{"type": "tool_use", "id": "t1", "name": "Read", "input": {"file_path": "/a/b.rs"}}]);
    let result = claude(
        SID1,
        &cwd,
        "2026-10-02T09:01:30Z",
        "user",
        json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "RESULT body"}]}),
    );
    let s1 = [
        said(SID1, &cwd, "2026-10-02T09:00:00Z", "ALPHA first question"),
        replied(
            SID1,
            &cwd,
            "2026-10-02T09:01:00Z",
            json!([{"type": "text", "text": "second answer, with \"quotes\" and a\nnewline"}]),
        ),
        replied(SID1, &cwd, "2026-10-02T09:01:20Z", read),
        result,
        said(
            SID1,
            &cwd,
            "2026-10-02T09:02:00Z",
            "<system-reminder>injected</system-reminder>beta typed after it",
        ),
        said(
            SID1,
            &cwd,
            "2026-10-02T09:03:00Z",
            "<command-name>/review</command-name><command-args>the branch</command-args>",
        ),
        replied(
            SID1,
            &cwd,
            "2026-10-02T09:04:00Z",
            json!([{"type": "text", "text": "日本語の返答 note"}]),
        ),
        said(SID1, &cwd, "2026-10-02T13:00:00Z", "later in the day"),
    ];
    fs::create_dir_all(w.s1().parent().unwrap()).unwrap();
    fs::write(w.s1(), jsonl(&s1)).unwrap();
    fs::create_dir_all(w.agent().parent().unwrap()).unwrap();
    fs::write(
        w.agent(),
        jsonl(&[
            said(
                SID1,
                &cwd,
                "2026-10-02T09:05:00Z",
                "task given to the subagent",
            ),
            replied(
                SID1,
                &cwd,
                "2026-10-02T09:06:00Z",
                json!([{"type": "text", "text": "subagent says beta"}]),
            ),
        ]),
    )
    .unwrap();
    fs::create_dir_all(w.s2().parent().unwrap()).unwrap();
    fs::write(
        w.s2(),
        jsonl(&[
            said(
                SID2,
                &w.other,
                "2026-10-03T09:00:00Z",
                "other project ALPHA",
            ),
            replied(
                SID2,
                &w.other,
                "2026-10-03T09:01:00Z",
                json!([{"type": "text", "text": "other reply"}]),
            ),
        ]),
    )
    .unwrap();
    let t = |m: u32| format!("2026-10-02T11:{m:02}:00.000Z");
    let small = [
        cx_meta(CX1, &t(0), &cwd, ""),
        cx_msg(1, &t(1), "user", "codex ALPHA question"),
    ];
    let large = [
        cx_meta(CX1, &t(0), &cwd, ""),
        cx_msg(1, &t(1), "user", "codex ALPHA question"),
        cx_call(2, &t(2), "ls dir\nsecond line"),
        cx_out(3, &t(3), "RESULT of the call"),
        cx_msg(4, &t(4), "assistant", "codex answer note"),
    ];
    for (p, l) in [(w.cx1(), &large[..]), (w.cx1_archived(), &small[..])] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, lines(l)).unwrap();
    }
    let parent = "019f0000-aaaa-7000-8000-00000000c0aa";
    let extra = format!(
        r#","parent_thread_id":"{parent}","subagent_history_start_ordinal":2,"source":{{"subagent":{{}}}}"#
    );
    fs::write(
        w.cxsub(),
        lines(&[
            cx_meta(CXSUB, &t(10), &cwd, &extra),
            cx_msg(1, &t(11), "user", "copied from the parent"),
            cx_msg(2, &t(12), "user", "own task for the codex subagent"),
            cx_msg(3, &t(13), "assistant", "codex subagent note"),
        ]),
    )
    .unwrap();
    w
}

// ---------------------------------------------------------------------------------------

#[test]
fn the_cache_answers_as_the_logs_do_from_the_first_run_on() {
    let w = world();
    w.agree("first run, the cache is made");
    w.agree("second run, the cache is used");
    assert!(!w.cache_files().is_empty(), "something was cached");
    w.expect_hits("beta", 2);
    w.expect_hits("ALPHA", 3);
    w.expect_hits("RESULT", 0);
    w.expect_hits("copied", 0);
}

#[test]
fn what_is_appended_is_read_from_where_the_cache_stopped() {
    let w = world();
    w.agree("start");
    let cwd = w.repo_cwd();
    append(
        &w.s1(),
        &jsonl(&[
            said(SID1, &cwd, "2026-10-02T14:00:00Z", "APPENDED user line"),
            replied(
                SID1,
                &cwd,
                "2026-10-02T14:01:00Z",
                json!([{"type": "text", "text": "APPENDED reply"}]),
            ),
        ]),
    );
    append(
        &w.cx1(),
        &lines(&[
            cx_msg(5, "2026-10-02T14:10:00.000Z", "user", "APPENDED codex line"),
            cx_call(6, "2026-10-02T14:11:00.000Z", "git status"),
        ]),
    );
    w.agree("lines appended");
    w.expect_hits("APPENDED", 3);
    // A second append goes on from the first.
    append(
        &w.s1(),
        &jsonl(&[said(SID1, &cwd, "2026-10-02T15:00:00Z", "APPENDED again")]),
    );
    w.agree("appended twice");
    w.expect_hits("APPENDED", 4);
}

#[test]
fn a_line_still_being_written_is_left_for_the_next_read() {
    let w = world();
    w.agree("start");
    let cwd = w.repo_cwd();
    let whole = said(SID1, &cwd, "2026-10-02T14:00:00Z", "HALFWAY done").to_string();
    let (head, tail) = whole.split_at(whole.len() / 2);
    append(&w.s1(), head);
    w.agree("half a line");
    w.expect_hits("HALFWAY", 0);
    // The rest arrives: the line is read whole, from its start.
    append(&w.s1(), &format!("{tail}\n"));
    w.agree("the line completed");
    w.expect_hits("HALFWAY", 1);
    // The same in a Codex log.
    let line = cx_msg(5, "2026-10-02T14:10:00.000Z", "user", "CODEXHALF done");
    let (head, tail) = line.split_at(line.len() / 2);
    append(&w.cx1(), head);
    w.agree("half a Codex line");
    w.expect_hits("CODEXHALF", 0);
    append(&w.cx1(), &format!("{tail}\n"));
    w.agree("the Codex line completed");
    w.expect_hits("CODEXHALF", 1);
}

#[test]
fn a_log_that_became_smaller_or_other_is_read_again_from_the_start() {
    let w = world();
    let cwd = w.repo_cwd();
    w.agree("start");
    w.expect_hits("later in the day", 1);

    // Smaller: the file was cut.
    fs::write(
        w.s1(),
        jsonl(&[said(SID1, &cwd, "2026-10-02T09:00:00Z", "ALPHA only line")]),
    )
    .unwrap();
    w.agree("the log shrank");
    w.expect_hits("later in the day", 0);
    w.expect_hits("ALPHA only line", 1);

    // Written over in place and longer than before, from the same first line: only what
    // was already read being different tells.
    let first = said(SID1, &cwd, "2026-10-02T09:00:00Z", "ALPHA only line");
    let before = fs::metadata(w.s1()).unwrap().len();
    let other = jsonl(&[
        first,
        said(
            SID1,
            &cwd,
            "2026-10-02T09:10:00Z",
            "REWRITTEN second line, longer than the whole old file",
        ),
        said(SID1, &cwd, "2026-10-02T09:11:00Z", "REWRITTEN third line"),
    ]);
    assert!(other.len() as u64 > before);
    fs::write(w.s1(), other).unwrap();
    w.agree("the log was written over");
    w.expect_hits("REWRITTEN", 2);

    // The same size and the same beginning, a different middle.
    let mut bytes = fs::read(w.s1()).unwrap();
    let at = bytes.windows(9).position(|b| b == b"REWRITTEN").unwrap();
    bytes[at..at + 9].copy_from_slice(b"REPLACING");
    bytes.extend_from_slice(
        jsonl(&[said(SID1, &cwd, "2026-10-02T09:12:00Z", "REPLACING tail")]).as_bytes(),
    );
    fs::write(w.s1(), bytes).unwrap();
    w.agree("the middle changed and the file grew");
    w.expect_hits("REWRITTEN", 1);
    w.expect_hits("REPLACING", 2);

    // A new file in its place (another inode).
    let tmp = w.path("replacement.jsonl");
    fs::write(
        &tmp,
        jsonl(&[
            said(SID1, &cwd, "2026-10-02T09:00:00Z", "ALPHA only line"),
            said(SID1, &cwd, "2026-10-02T09:20:00Z", "REPLACEMENT file"),
        ]),
    )
    .unwrap();
    fs::rename(&tmp, w.s1()).unwrap();
    w.agree("the log was replaced by another file");
    w.expect_hits("REPLACEMENT", 1);
    w.expect_hits("REPLACING", 0);
}

#[test]
fn a_log_that_is_gone_is_gone_from_the_answers_and_the_cache() {
    let w = world();
    w.agree("start");
    let kept = w.cache_files().len();
    fs::remove_file(w.s2()).unwrap();
    w.agree("a log was deleted");
    w.expect_hits("other project", 0);
    assert_eq!(
        w.cache_files().len(),
        kept - 1,
        "its cache was cleared away"
    );
    // A log only touched is the same log.
    let f = fs::File::options().write(true).open(w.s1()).unwrap();
    f.set_modified(std::time::SystemTime::now()).unwrap();
    w.agree("touched");
    w.expect_hits("later in the day", 1);
}

#[test]
fn a_cache_that_is_damaged_or_from_another_reader_is_not_believed() {
    let w = world();
    w.agree("start");
    let files = w.cache_files();
    assert!(!files.is_empty());
    let poison = |f: &Path, from: &str, to: &str| {
        let text = fs::read_to_string(f).unwrap();
        assert!(text.contains(from), "{text}");
        fs::write(f, text.replacen(from, to, 1)).unwrap();
    };
    // Which cache file holds the first Claude session: the one with its words.
    let mine = files
        .iter()
        .find(|f| fs::read_to_string(f).unwrap().contains("later in the day"))
        .unwrap()
        .clone();

    // Made by another reader (or layout): its contents are not used, and are made again.
    for key in ["reader", "format"] {
        let text = fs::read_to_string(&mine).unwrap();
        let header = text.lines().next().unwrap();
        let mut h: Value = serde_json::from_str(header).unwrap();
        h[key] = json!(if key == "reader" {
            json!("0000")
        } else {
            json!(999)
        });
        let rest = &text[header.len()..];
        fs::write(&mine, format!("{h}{rest}")).unwrap();
        poison(&mine, "later in the day", "POISONED text");
        w.expect_hits("POISONED", 0);
        w.expect_hits("later in the day", 1);
        w.agree(&format!("a cache of another {key}"));
    }

    // Cut short in the middle, and not a cache at all.
    let text = fs::read_to_string(&mine).unwrap();
    fs::write(&mine, &text[..text.len() / 2]).unwrap();
    w.agree("a cache cut in half");
    w.expect_hits("later in the day", 1);
    fs::write(&mine, "not a cache\n{\"x\":").unwrap();
    w.agree("a cache of nonsense");
    // Made again, whole.
    let text = fs::read_to_string(&mine).unwrap();
    assert!(text.contains("later in the day"), "{text}");
}

#[test]
fn without_a_place_to_keep_the_cache_the_logs_are_read() {
    let w = world();
    // The cache directory cannot be made: a file stands where it should be.
    let blocked = w.path("blocked");
    fs::write(&blocked, "a file").unwrap();
    for q in w.questions() {
        let args: Vec<&str> = q.iter().map(String::as_str).collect();
        let kept = w.ask_with(&blocked, false, &args);
        let raw = w.ask_with(&blocked, true, &args);
        assert_eq!(kept, raw, "`{}`", q.join(" "));
        assert_eq!(kept.0, 0, "{}", kept.2);
    }
}

#[test]
fn raw_leaves_no_cache_behind_and_the_cache_is_kept_private() {
    let w = world();
    let (code, _, err) = w.ask(true, &["search", ".", "--who", "any"]);
    assert_eq!(code, 0, "{err}");
    assert!(w.cache_files().is_empty(), "--raw wrote a cache");
    let (code, _, err) = w.ask(false, &["search", ".", "--who", "any"]);
    assert_eq!(code, 0, "{err}");
    assert!(!w.cache_files().is_empty());
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let any = &w.cache_files()[0];
    assert_eq!(mode(any), 0o600, "the cache holds what was said");
    assert_eq!(mode(any.parent().unwrap()), 0o700);
    // --raw is for the commands that read talk.
    let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
        .args([
            "repeats",
            "--raw",
            "--root",
            w.path("claude").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--raw"));
}
