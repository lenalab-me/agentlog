//! Runs the binary on small session logs written here, and reads its --json output.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

/// One tool call and its result, as Claude Code writes them (two JSONL lines).
struct Ev {
    ts: &'static str,
    tool: &'static str,
    input: Value,
    result: &'static str,
    is_error: bool,
}

fn bash(ts: &'static str, cmd: &str, result: &'static str) -> Ev {
    Ev {
        ts,
        tool: "Bash",
        input: json!({ "command": cmd }),
        is_error: result.starts_with("Exit code") || result.starts_with("This agent"),
        result,
    }
}

fn tool(
    ts: &'static str,
    name: &'static str,
    path: &str,
    result: &'static str,
    is_error: bool,
) -> Ev {
    Ev {
        ts,
        tool: name,
        input: json!({ "file_path": path }),
        result,
        is_error,
    }
}

fn write_log(path: &Path, evs: &[Ev]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut out = String::new();
    // Ids are unique across sessions, as Claude Code's are (a copied history shares them).
    let file: String = path
        .to_string_lossy()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    for (i, e) in evs.iter().enumerate() {
        let id = format!("toolu_{file}_{i}");
        let use_line = json!({
            "type": "assistant", "timestamp": e.ts,
            "message": {"content": [{"type": "tool_use", "id": id, "name": e.tool, "input": e.input}]}
        });
        let result_line = json!({
            "type": "user", "timestamp": e.ts,
            "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": e.result, "is_error": e.is_error}]}
        });
        out.push_str(&format!("{use_line}\n{result_line}\n"));
    }
    fs::write(path, out).unwrap();
}

fn run(root: &Path, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
        .args(args)
        .args(["--root", root.to_str().unwrap()])
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        // No Codex logs of the real machine: its default places live under HOME.
        .env("HOME", "/nonexistent-home")
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn json_of(root: &Path, args: &[&str]) -> Value {
    let mut a = args.to_vec();
    a.extend([
        "--json",
        "--since",
        "2026-10-01T00:00Z",
        "--until",
        "2026-10-20T00:00Z",
    ]);
    let (code, out, err) = run(root, &a);
    assert_eq!(code, 0, "stderr: {err}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
}

const OLD_NOT_FOUND: &str = "Exit code 1\nerror: edits[0] src/a.rs (input line 2): \"old\" not found\nrpatch: nothing written";
const WORKTREE: &str = "This agent is isolated in the worktree /w/agent-a1, but this command is too complex to verify that it stays inside the worktree.";

#[test]
fn the_window_is_each_calls_own_time() {
    let d = tempfile::tempdir().unwrap();
    // A session from April, resumed in October: the file is new, most calls are old.
    write_log(
        &d.path().join("-p-old/s1.jsonl"),
        &[
            bash(
                "2026-04-23T03:00:00Z",
                "kubectl get pods",
                "Exit code 1\nerror: no context",
            ),
            bash(
                "2026-04-23T03:01:00Z",
                "kubectl get pods",
                "Exit code 1\nerror: no context",
            ),
            bash("2026-10-05T03:00:00Z", "ls", "a\nb"),
        ],
    );
    let j = json_of(d.path(), &["repeats"]);
    assert_eq!(j["calls"], 1);
    assert_eq!(j["signatures"], json!([]));
}

#[test]
fn repeats_counts_contexts_and_subagents_apart() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("-p-net");
    write_log(
        &p.join("s1.jsonl"),
        &[
            bash("2026-10-05T01:00:00Z", "cd /w && git status", WORKTREE),
            bash("2026-10-05T01:01:00Z", "rpatch <<'EOF'", WORKTREE),
            bash("2026-10-05T01:02:00Z", "git status", "On branch main"),
            bash(
                "2026-10-05T01:03:00Z",
                "for f in x; do git log $f; done",
                WORKTREE,
            ),
        ],
    );
    write_log(
        &p.join("s1/subagents/agent-a1.jsonl"),
        &[bash("2026-10-06T01:00:00Z", "cd /w && sh x.sh", WORKTREE)],
    );
    // seen once only: left out
    write_log(
        &p.join("s2.jsonl"),
        &[bash(
            "2026-10-06T02:00:00Z",
            "npm test",
            "Exit code 1\nError: one-off",
        )],
    );
    let j = json_of(d.path(), &["repeats"]);
    let sigs = j["signatures"].as_array().unwrap();
    assert_eq!(sigs.len(), 1, "{j}");
    let s = &sigs[0];
    assert!(
        s["signature"]
            .as_str()
            .unwrap()
            .starts_with("Bash | This agent is isolated")
    );
    assert_eq!(
        (s["rep"].clone(), s["ctx"].clone(), s["total"].clone()),
        (json!(1), json!(2), json!(4))
    );
    // the passing `git status` between the 2nd and 3rd breaks the run
    assert_eq!(s["streak"], 2);
}

#[test]
fn what_the_user_refused_is_not_a_failure() {
    let d = tempfile::tempdir().unwrap();
    let no = "The user doesn't want to proceed with this tool use.";
    write_log(
        &d.path().join("-p/s1.jsonl"),
        &[
            tool("2026-10-05T01:00:00Z", "Edit", "/a", no, true),
            tool("2026-10-05T01:01:00Z", "Edit", "/b", no, true),
            tool("2026-10-05T01:02:00Z", "Edit", "/c", "ok", false),
        ],
    );
    assert_eq!(json_of(d.path(), &["repeats"])["signatures"], json!([]));
    let j = json_of(d.path(), &["tool", "Edit"]);
    assert_eq!(
        (j["uses"].clone(), j["failed"].clone()),
        (json!(1), json!(0))
    );
    assert_eq!(j["after_failure"], json!({}));
    let j = json_of(d.path(), &["daily", "x=.", "--failed"]);
    assert_eq!(j["total"]["x"], 0);
    let (_, text, _) = run(d.path(), &["repeats", "--since", "2026-10-01T00:00Z"]);
    assert!(text.contains(": 3 calls, 0 failed,"), "{text}");
}

#[test]
fn rpatch_runs_of_failures_and_what_came_next() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("-p");
    write_log(
        &p.join("s1.jsonl"),
        &[
            // fixed on the next call, with a Read between
            bash("2026-10-05T01:00:00Z", "rpatch <<'EOF'", OLD_NOT_FOUND),
            tool("2026-10-05T01:01:00Z", "Read", "/a", "1 fn a()", false),
            bash(
                "2026-10-05T01:02:00Z",
                "rpatch <<'EOF'",
                "M src/a.rs edits=1",
            ),
            // two in a row, then fixed
            bash("2026-10-05T02:00:00Z", "rpatch <<'EOF'", OLD_NOT_FOUND),
            bash(
                "2026-10-05T02:01:00Z",
                "cd x && rpatch <<'EOF'",
                "Exit code 2\nrpatch: line 3: a FROM part needs `<<<<<<< TO`",
            ),
            bash(
                "2026-10-05T02:02:00Z",
                "rpatch <<'EOF'",
                "M src/a.rs edits=1",
            ),
            // a later command in the chain failed, not rpatch: not a failure of rpatch
            bash(
                "2026-10-05T03:00:00Z",
                "rpatch <<'EOF' && cargo test",
                "Exit code 101\nM src/a.rs edits=1\ntest x ... FAILED",
            ),
            // a new file: neither a use nor counted per day
            tool(
                "2026-10-05T04:00:00Z",
                "Write",
                "/new.rs",
                "File created",
                false,
            ),
            bash("2026-10-05T04:01:00Z", "sed -i '' s/a/b/ f.txt", ""),
            bash("2026-10-05T04:02:00Z", "rg x | sd a b", ""),
        ],
    );
    write_log(
        &p.join("s2.jsonl"),
        &[
            // went another way: rewrote the file
            bash("2026-10-06T01:00:00Z", "rpatch <<'EOF'", OLD_NOT_FOUND),
            tool(
                "2026-10-06T01:01:00Z",
                "Write",
                "/w/src/a.rs",
                "File written",
                false,
            ),
            // a scratch file under /tmp is not going another way; the context ends
            bash("2026-10-06T02:00:00Z", "rpatch <<'EOF'", OLD_NOT_FOUND),
            bash("2026-10-06T02:01:00Z", "cat > /tmp/x.mjs <<'EOF'", ""),
        ],
    );
    let j = json_of(d.path(), &["tool", "rpatch"]);
    assert_eq!(j["uses"], 8);
    assert_eq!(j["failed"], 5);
    assert_eq!(
        j["after_failure"],
        json!({
            "fixed on the next call": 1,
            "fixed after 2 failures in a row": 1,
            "went another way on the same file": 1,
            "context ended after it": 1,
        })
    );
    assert_eq!(j["went_to"], json!({"Write": 1}));
    // sed -i counts; sd in a pipe, Write and cat > do not
    assert_eq!(j["other_ways"]["kinds"], json!({"Bash sed": 1}));
}

#[test]
fn devsrv_other_ways_before_and_after_the_first_use() {
    let d = tempfile::tempdir().unwrap();
    write_log(
        &d.path().join("-p/s1.jsonl"),
        &[
            bash("2026-10-02T01:00:00Z", "lsof -i :8080", "node 123"),
            bash("2026-10-02T01:01:00Z", "pkill -f vite", ""),
            bash("2026-10-02T01:02:00Z", "npx vite build && echo ok", "ok"),
            bash("2026-10-03T01:00:00Z", "python3 -m http.server 8000 &", ""),
            bash(
                "2026-10-07T01:00:00Z",
                "devsrv start web --port auto -- npx vite --port {port}",
                "http://127.0.0.1:5173/",
            ),
            bash(
                "2026-10-07T01:05:00Z",
                "devsrv stop nope",
                "Exit code 1\ndevsrv: nothing named nope was started here",
            ),
            bash("2026-10-07T01:06:00Z", "devsrv stop web", ""),
            bash("2026-10-08T01:00:00Z", "kill $(cat pid)", ""),
        ],
    );
    let j = json_of(d.path(), &["tool", "devsrv"]);
    assert_eq!(j["uses"], 3);
    assert_eq!(j["failed"], 1);
    assert_eq!(j["after_failure"], json!({"fixed on the next call": 1}));
    // `vite build && …` is not a server in the background
    assert_eq!(j["other_ways"]["total"], 4);
    assert_eq!(j["other_ways"]["before_first_use"], 3);
    assert!(j["first_use"].as_str().unwrap().starts_with("2026-10-07"));
}

#[test]
fn any_tool_by_name() {
    let d = tempfile::tempdir().unwrap();
    let rate = "Error POSTing to endpoint: Too many requests";
    write_log(
        &d.path().join("-p/s1.jsonl"),
        &[
            tool("2026-10-02T01:00:00Z", "mcp__docs__search", "q", rate, true),
            tool("2026-10-02T01:00:05Z", "mcp__docs__search", "q", rate, true),
            tool(
                "2026-10-02T01:00:10Z",
                "mcp__docs__search",
                "q",
                "results",
                false,
            ),
        ],
    );
    let j = json_of(d.path(), &["tool", "mcp__docs__search"]);
    assert_eq!(j["uses"], 3);
    assert_eq!(
        j["after_failure"],
        json!({"fixed after 2 failures in a row": 1})
    );
}

#[test]
fn daily_counts_per_local_day() {
    let d = tempfile::tempdir().unwrap();
    write_log(
        &d.path().join("-p/s1.jsonl"),
        &[
            bash(
                "2026-10-02T01:00:00Z",
                "until curl -s x; do sleep 1; done",
                "",
            ),
            bash("2026-10-02T02:00:00Z", "cd w && git status", WORKTREE),
            bash(
                "2026-10-04T01:00:00Z",
                "until curl -s x; do sleep 1; done",
                "",
            ),
        ],
    );
    let j = json_of(
        d.path(),
        &["daily", "poll=until .*sleep", "wt=isolated in the worktree"],
    );
    assert_eq!(j["total"], json!({"poll": 2, "wt": 1}));
    assert_eq!(j["days"].as_array().unwrap().len(), 2);
    let j = json_of(d.path(), &["daily", "poll=until", "--in", "result"]);
    assert_eq!(j["total"], json!({"poll": 0}));
}

#[test]
fn the_running_session_is_left_out_unless_asked() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("-p");
    let same = "Exit code 1\nError: same";
    write_log(
        &p.join("self-1234.jsonl"),
        &[bash("2026-10-05T01:00:00Z", "npm test", same)],
    );
    write_log(
        &p.join("self-1234/subagents/agent-a1.jsonl"),
        &[bash("2026-10-05T01:01:00Z", "npm test", same)],
    );
    write_log(
        &p.join("other.jsonl"),
        &[bash("2026-10-05T01:02:00Z", "npm test", same)],
    );
    let calls = |extra: &[&str]| {
        let mut args = vec![
            "repeats",
            "--json",
            "--since",
            "2026-10-01T00:00Z",
            "--until",
            "2026-10-20T00:00Z",
            "--root",
            d.path().to_str().unwrap(),
        ];
        args.extend(extra);
        let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
            .args(&args)
            .env("CLAUDE_CODE_SESSION_ID", "self-1234")
            .output()
            .unwrap();
        serde_json::from_slice::<Value>(&o.stdout).unwrap()["calls"].clone()
    };
    assert_eq!(calls(&[]), 1);
    assert_eq!(calls(&["--include-self"]), 3);
}

#[test]
fn bad_input_is_exit_2_and_no_log_directory_is_exit_3() {
    let d = tempfile::tempdir().unwrap();
    for args in [
        &["repeats", "--since", "yesterday"][..],
        &["tool"],
        &["daily", "noequals"],
        &["daily", "x=("],
        &["frobnicate"],
        &["repeats", "--bogus"],
    ] {
        let (code, _, err) = run(d.path(), args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.starts_with("agentlog: "), "{err}");
    }
    let (code, _, err) = run(&d.path().join("missing"), &["repeats"]);
    assert_eq!(code, 3, "{err}");
}

// ---------------------------------------------------------------------------------------
// search and show: conversations are written by hand, with made-up words

const CWD: &str = "/Users/tester/project/prod/net/designer";

fn line(sid: &str, ts: &str, kind: &str, message: Value) -> Value {
    json!({"type": kind, "timestamp": ts, "sessionId": sid, "cwd": CWD, "message": message})
}

fn user(sid: &str, ts: &str, text: &str) -> Value {
    line(sid, ts, "user", json!({"role": "user", "content": text}))
}

fn assistant(sid: &str, ts: &str, blocks: Value) -> Value {
    line(
        sid,
        ts,
        "assistant",
        json!({"role": "assistant", "content": blocks}),
    )
}

fn says(sid: &str, ts: &str, text: &str) -> Value {
    assistant(sid, ts, json!([{"type": "text", "text": text}]))
}

fn write_lines(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let out: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(path, out).unwrap();
}

fn hits(root: &Path, args: &[&str]) -> Vec<Value> {
    let mut a = vec!["search"];
    a.extend(args);
    a.push("--json");
    let (code, out, err) = run(root, &a);
    assert_eq!(code, 0, "stderr: {err}");
    out.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

const S1: &str = "aaaaaaaa-1111-4000-8000-000000000001";

#[test]
fn search_reads_only_what_was_said() {
    let d = tempfile::tempdir().unwrap();
    let ts = "2026-10-02T05:00:00Z";
    write_lines(
        &d.path().join("-p-net").join(format!("{S1}.jsonl")),
        &[
            user(S1, ts, "ZEBRA typed by a person"),
            says(S1, "2026-10-02T05:01:00Z", "I will handle the ZEBRA"),
            assistant(
                S1,
                "2026-10-02T05:02:00Z",
                json!([{"type": "thinking", "thinking": "ZEBRA in thought"}]),
            ),
            assistant(
                S1,
                "2026-10-02T05:03:00Z",
                json!([{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "echo ZEBRA"}}]),
            ),
            line(
                S1,
                "2026-10-02T05:03:01Z",
                "user",
                json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ZEBRA output"}]}),
            ),
            json!({"type": "user", "timestamp": "2026-10-02T05:04:00Z", "sessionId": S1, "cwd": CWD,
                   "isMeta": true, "message": {"role": "user", "content": "ZEBRA meta"}}),
            user(
                S1,
                "2026-10-02T05:05:00Z",
                "<system-reminder>ZEBRA rules</system-reminder>\nok go",
            ),
            user(
                S1,
                "2026-10-02T05:06:00Z",
                "<task-notification><summary>ZEBRA done</summary></task-notification>",
            ),
            user(
                S1,
                "2026-10-02T05:07:00Z",
                "<local-command-stdout>ZEBRA</local-command-stdout>",
            ),
            line(
                S1,
                "2026-10-02T05:08:00Z",
                "user",
                json!({"role": "user", "content": [{"type": "text", "text": "second block ZEBRA"}]}),
            ),
        ],
    );
    let all = hits(d.path(), &["ZEBRA"]);
    let words: Vec<&str> = all.iter().map(|h| h["snippet"].as_str().unwrap()).collect();
    assert_eq!(
        words,
        [
            "ZEBRA typed by a person",
            "I will handle the ZEBRA",
            "second block ZEBRA"
        ]
    );
    assert_eq!(hits(d.path(), &["ZEBRA", "--who", "user"]).len(), 2);
    assert_eq!(hits(d.path(), &["ZEBRA", "--who", "assistant"]).len(), 1);
    // What is left of a user message once the reminder is taken off is searchable.
    let ok = hits(d.path(), &["ok go"]);
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0]["snippet"], "ok go");
}

#[test]
fn search_has_no_window_unless_asked_and_shows_local_time_ids_and_projects() {
    let d = tempfile::tempdir().unwrap();
    write_lines(
        &d.path().join("-p-net").join(format!("{S1}.jsonl")),
        &[
            user(S1, "2020-01-02T03:04:05Z", "an old Remark about caching"),
            says(S1, "2026-10-02T05:01:00Z", "a new remark about caching"),
        ],
    );
    // No --since: the 2020 line is found. -i makes the two spellings one.
    let all = hits(d.path(), &["remark", "-i"]);
    assert_eq!(all.len(), 2);
    assert_eq!(all[0]["timestamp"], "2020-01-02T03:04:05Z");
    assert_eq!(all[0]["who"], "user");
    assert_eq!(all[0]["id"], "aaaaaaaa");
    assert_eq!(all[0]["session"], S1);
    assert_eq!(all[0]["project"], "net/designer");
    assert_eq!(hits(d.path(), &["remark"]).len(), 1);
    let since = hits(d.path(), &["remark", "-i", "--since", "2026-10-01T00:00Z"]);
    assert_eq!(since.len(), 1);
    assert_eq!(since[0]["who"], "assistant");
    // The text form leads with the local time to the minute and the id.
    let (code, out, _) = run(d.path(), &["search", "remark", "-i"]);
    assert_eq!(code, 0);
    let first = out.lines().next().unwrap();
    assert!(
        first.contains("user") && first.contains("aaaaaaaa") && first.contains("net/designer"),
        "{first}"
    );
    assert!(
        first.contains("| …") || first.contains("| an old Remark"),
        "{first}"
    );
}

#[test]
fn search_keeps_the_newest_past_the_limit_and_says_so() {
    let d = tempfile::tempdir().unwrap();
    write_lines(
        &d.path().join("-p-net").join(format!("{S1}.jsonl")),
        &[
            user(S1, "2026-10-02T05:00:00Z", "needle one"),
            says(S1, "2026-10-02T05:01:00Z", "needle two"),
            user(S1, "2026-10-02T05:02:00Z", "needle three"),
        ],
    );
    let (code, out, _) = run(d.path(), &["search", "needle", "--limit", "2"]);
    assert_eq!(code, 0);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(lines[0].contains("needle two") && lines[1].contains("needle three"));
    assert!(
        lines[2].starts_with("3 hits") && lines[2].contains("1 older"),
        "{out}"
    );
    // JSON: only the hits on stdout, the note aside.
    let (_, out, err) = run(d.path(), &["search", "needle", "--limit", "2", "--json"]);
    assert_eq!(out.lines().count(), 2);
    assert!(err.contains("3 hits"), "{err}");
    let (code, out, _) = run(d.path(), &["search", "nothing-like-this"]);
    assert_eq!((code, out.trim()), (0, "0 hits"));
}

#[test]
fn a_subagent_is_marked_and_its_prompt_is_not_the_users() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("-p-net");
    write_lines(
        &p.join(format!("{S1}.jsonl")),
        &[user(S1, "2026-10-02T05:00:00Z", "beacon from a person")],
    );
    let mut sub = user(S1, "2026-10-02T05:01:00Z", "beacon written by the parent");
    sub["isSidechain"] = json!(true);
    write_lines(
        &p.join(S1).join("subagents/agent-bbbbbbbbcccc.jsonl"),
        &[
            sub,
            says(S1, "2026-10-02T05:02:00Z", "beacon from the subagent"),
        ],
    );
    let all = hits(d.path(), &["beacon"]);
    let got: Vec<(&str, &str)> = all
        .iter()
        .map(|h| (h["who"].as_str().unwrap(), h["id"].as_str().unwrap()))
        .collect();
    assert_eq!(
        got,
        [
            ("user", "aaaaaaaa"),
            ("task", "aaaaaaaa/bbbbbbbb"),
            ("assistant", "aaaaaaaa/bbbbbbbb")
        ]
    );
    assert_eq!(hits(d.path(), &["beacon", "--who", "user"]).len(), 1);
}

fn long_talk(root: &Path) {
    let mut lines = Vec::new();
    for i in 0..9 {
        let ts = format!("2026-10-02T05:{:02}:00Z", i * 2);
        lines.push(user(S1, &ts, &format!("question {i} marker{i}")));
        let ts = format!("2026-10-02T05:{:02}:30Z", i * 2);
        lines.push(assistant(
            S1,
            &ts,
            json!([
                {"type": "text", "text": format!("answer {i}")},
                {"type": "tool_use", "id": format!("t{i}"), "name": "Bash", "input": {"command": format!("ls dir{i}\nsecond line")}}
            ]),
        ));
        lines.push(line(
            S1,
            &ts,
            "user",
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": format!("t{i}"), "content": format!("RESULT{i}")}]}),
        ));
    }
    write_lines(&root.join("-p-net").join(format!("{S1}.jsonl")), &lines);
}

fn show(root: &Path, args: &[&str]) -> (i32, String, String) {
    let mut a = vec!["show"];
    a.extend(args);
    run(root, &a)
}

#[test]
fn show_prints_the_talk_with_tools_as_one_line_and_no_results() {
    let d = tempfile::tempdir().unwrap();
    long_talk(d.path());
    let (code, out, err) = show(d.path(), &["aaaa"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("question 0 marker0") && out.contains("answer 8"),
        "{out}"
    );
    assert!(out.contains("  [Bash] ls dir3…"), "{out}");
    assert!(
        !out.contains("RESULT") && !out.contains("second line"),
        "{out}"
    );
    assert!(out.starts_with("# aaaaaaaa  claude  net/designer"), "{out}");
}

#[test]
fn show_centres_on_the_first_grep_match_or_the_time() {
    let d = tempfile::tempdir().unwrap();
    long_talk(d.path());
    // marker4 is the 9th utterance (#9); 2 either side is #7..#11.
    let (code, out, err) = show(
        d.path(),
        &["aaaa", "--grep", "marker[4-6]", "--around", "2"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("showing #7..#11 around #9"), "{out}");
    assert!(out.contains(">> [#9 "), "{out}");
    assert!(
        out.contains("answer 3")
            && out.contains("question 5")
            && !out.contains("answer 5")
            && !out.contains("answer 2"),
        "{out}"
    );
    assert!(!out.contains("question 0"), "{out}");
    // The time picks the nearest utterance: 05:08:20Z is closest to the answer at 05:08:30.
    let (code, out, err) = show(
        d.path(),
        &["aaaa", "--at", "2026-10-02T05:08:20Z", "--around", "0"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains(">> [#10 ") && out.contains("answer 4") && !out.contains("question 4"),
        "{out}"
    );
    // To the minute covers the whole minute, the earliest utterance in it wins.
    let (_, out, _) = show(
        d.path(),
        &["aaaa", "--at", "2026-10-02T05:08Z", "--around", "0"],
    );
    assert!(
        out.contains(">> [#9 ") && out.contains("question 4"),
        "{out}"
    );
    // No match is an error, not an empty page.
    let (code, _, err) = show(d.path(), &["aaaa", "--grep", "no-such-word"]);
    assert_eq!(code, 2, "{err}");
    let (code, _, err) = show(
        d.path(),
        &["aaaa", "--grep", "x", "--at", "2026-10-02T05:00Z"],
    );
    assert_eq!(code, 2, "{err}");
}

#[test]
fn show_needs_a_unique_session() {
    let d = tempfile::tempdir().unwrap();
    long_talk(d.path());
    let other = "aaaaaaaa-2222-4000-8000-000000000002";
    write_lines(
        &d.path().join("-p-net").join(format!("{other}.jsonl")),
        &[user(other, "2026-10-03T05:00:00Z", "another talk")],
    );
    let (code, out, err) = show(d.path(), &["aaaaaaaa"]);
    assert_eq!(code, 2, "{out}");
    assert!(err.contains("fits 2 sessions"), "{err}");
    let (code, _, err) = show(d.path(), &["aaaaaaaa-2"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = show(d.path(), &["ffff"]);
    assert_eq!(code, 2);
    assert!(err.contains("no session starts with `ffff`"), "{err}");
}

// ---------------------------------------------------------------------------------------
// Codex logs: written line by line in the order Codex writes the keys (the reader tells a
// line's kind from its first bytes), with made-up words

fn cx_line(ord: u32, ts: &str, kind: &str, payload: &str) -> String {
    format!(r#"{{"timestamp":"{ts}","ordinal":{ord},"type":"{kind}","payload":{payload}}}"#)
}

fn cx_meta(sid: &str, ts: &str, cwd: &str, extra: &str) -> String {
    let payload = format!(r#"{{"id":"{sid}","cwd":"{cwd}","originator":"Codex Desktop"{extra}}}"#);
    cx_line(0, ts, "session_meta", &payload)
}

fn cx_msg(ord: u32, ts: &str, role: &str, blocks: &[&str]) -> String {
    let kind = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    let content: Vec<String> = blocks
        .iter()
        .map(|t| json!({"type": kind, "text": t}).to_string())
        .collect();
    cx_line(
        ord,
        ts,
        "response_item",
        &format!(
            r#"{{"type":"message","role":"{role}","content":[{}]}}"#,
            content.join(",")
        ),
    )
}

/// A payload whose `type` comes first, as Codex writes it, then the fields of `rest`.
fn cx_typed(ord: u32, ts: &str, kind: &str, ptype: &str, rest: &Value) -> String {
    let body = rest.to_string();
    cx_line(
        ord,
        ts,
        kind,
        &format!(r#"{{"type":"{ptype}",{}"#, &body[1..]),
    )
}

fn cx_call(ord: u32, ts: &str, name: &str, args: &Value) -> String {
    let rest = json!({"name": name, "arguments": args.to_string(), "call_id": "c1"});
    cx_typed(ord, ts, "response_item", "function_call", &rest)
}

fn cx_custom(ord: u32, ts: &str, name: &str, input: &str) -> String {
    let rest = json!({"name": name, "input": input});
    cx_typed(ord, ts, "response_item", "custom_tool_call", &rest)
}

fn cx_other(ord: u32, ts: &str, kind: &str, ptype: &str, text: &str) -> String {
    let rest = json!({"output": text, "text": text, "message": text});
    cx_typed(ord, ts, kind, ptype, &rest)
}

fn write_raw(path: &Path, lines: &[String]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let out: String = lines.iter().map(|l| format!("{l}\n")).collect();
    fs::write(path, out).unwrap();
}

fn cx_file(codex: &Path, dir: &str, local: &str, sid: &str) -> std::path::PathBuf {
    codex.join(dir).join(format!("rollout-{local}-{sid}.jsonl"))
}

/// `agentlog <args> --source codex` over the two Codex places under `codex`.
fn cx(codex: &Path, args: &[&str]) -> (i32, String, String) {
    for d in ["sessions", "archived_sessions"] {
        fs::create_dir_all(codex.join(d)).unwrap();
    }
    let (a, b) = (
        codex.join("sessions").to_str().unwrap().to_string(),
        codex
            .join("archived_sessions")
            .to_str()
            .unwrap()
            .to_string(),
    );
    let mut all: Vec<&str> = args.to_vec();
    all.extend(["--source", "codex", "--codex-root", &a, "--codex-root", &b]);
    run(&codex.join("no-claude"), &all)
}

fn cx_hits(codex: &Path, args: &[&str]) -> Vec<Value> {
    let mut a = vec!["search"];
    a.extend(args);
    a.push("--json");
    let (code, out, err) = cx(codex, &a);
    assert_eq!(code, 0, "stderr: {err}");
    out.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

fn snippets(h: &[Value]) -> Vec<&str> {
    h.iter().map(|h| h["snippet"].as_str().unwrap()).collect()
}

const CX1: &str = "019f0000-aaaa-7000-8000-00000000c001";
const CX_CWD: &str = "/Users/tester/project/prod/net/designer";
const CX_DAY: &str = "sessions/2026/10/02";
const CX_LOCAL: &str = "2026-10-02T14-00-00";

#[test]
fn codex_search_reads_only_what_was_said() {
    let d = tempfile::tempdir().unwrap();
    let t = |s: u32| format!("2026-10-02T05:{s:02}:00.123Z");
    let mut lines = vec![
        cx_meta(CX1, &t(0), CX_CWD, ""),
        cx_msg(1, &t(1), "developer", &["ZEBRA developer rules"]),
        cx_msg(
            2,
            &t(2),
            "user",
            &[
                "<in-app-browser-context source=\"ambient\">\nZEBRA tabs\n</in-app-browser-context>",
                "ZEBRA typed by a person",
            ],
        ),
        cx_msg(
            3,
            &t(3),
            "user",
            &[
                "<environment_context>\n  <cwd>ZEBRA</cwd>\n</environment_context>",
                "# AGENTS.md instructions for /x\n\n<INSTRUCTIONS>\nZEBRA rules\n</INSTRUCTIONS>",
                "<recommended_plugins>\nHere is a list of plugins ZEBRA\n</recommended_plugins>",
                "<subagent_notification>\n{\"note\":\"ZEBRA\"}\n</subagent_notification>",
                "<turn_aborted>\nThe user interrupted ZEBRA\n</turn_aborted>",
                "<image name=[Image #1] path=\"/tmp/ZEBRA.png\">",
                "</image>",
            ],
        ),
        cx_msg(
            4,
            &t(4),
            "user",
            &[
                "# Files mentioned by the user:\n\n## a.png: /tmp/a.png\n\n## My request for Codex:\n\nZEBRA with an attachment",
            ],
        ),
        cx_msg(5, &t(5), "assistant", &["I will handle the ZEBRA"]),
        cx_other(6, &t(6), "response_item", "reasoning", "ZEBRA in thought"),
        cx_call(7, &t(7), "exec_command", &json!({"cmd": "echo ZEBRA"})),
        cx_other(
            8,
            &t(8),
            "response_item",
            "function_call_output",
            "ZEBRA output",
        ),
        cx_other(9, &t(9), "event_msg", "user_message", "ZEBRA again"),
    ];
    // A damaged line in the middle must not stop the file, and a half-written last line
    // is only passed over.
    lines.push(r#"{"timestamp":"2026-10-02T05:10:00.000Z","ordinal":10,"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"ZEBRA broken"#.to_string());
    lines.push(cx_msg(11, &t(11), "user", &["ZEBRA after the damage"]));
    let mut raw: String = lines.iter().map(|l| format!("{l}\n")).collect();
    raw.push_str(r#"{"timestamp":"2026-10-02T05:12:00.000Z","ordinal":12,"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"ZEBRA cut"#);
    let path = cx_file(d.path(), CX_DAY, CX_LOCAL, CX1);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, raw).unwrap();

    let all = cx_hits(d.path(), &["ZEBRA"]);
    assert_eq!(
        snippets(&all),
        [
            "ZEBRA typed by a person",
            "ZEBRA with an attachment",
            "I will handle the ZEBRA",
            "ZEBRA after the damage"
        ]
    );
    assert_eq!(all[0]["source"], "codex");
    assert_eq!(all[0]["id"], "0000c001", "the last 8 characters");
    assert_eq!(all[0]["session"], CX1);
    assert_eq!(all[0]["project"], "net/designer");
    assert_eq!(all[0]["timestamp"], "2026-10-02T05:02:00Z");
    assert_eq!(cx_hits(d.path(), &["ZEBRA", "--who", "assistant"]).len(), 1);
    assert_eq!(
        cx_hits(d.path(), &["ZEBRA", "--project", "designer"]).len(),
        4
    );
    assert_eq!(
        cx_hits(d.path(), &["ZEBRA", "--project", "elsewhere"]).len(),
        0
    );
}

#[test]
fn codex_search_finds_what_the_log_writes_differently() {
    // In a log line a newline is `\n`, a quote `\"`, a tab `\t`: a pattern that meets any of
    // them, or counts characters, must not be turned away by the quick look at the raw line.
    let d = tempfile::tempdir().unwrap();
    let t = |s: u32| format!("2026-10-02T05:{s:02}:00.000Z");
    let lines = [
        cx_meta(CX1, &t(0), CX_CWD, ""),
        cx_msg(1, &t(1), "user", &["first line\nsecond line"]),
        cx_msg(2, &t(2), "user", &["she said \"hello there\" twice"]),
        cx_msg(3, &t(3), "user", &["column\tseparated"]),
        cx_msg(4, &t(4), "user", &["Mixed CASE Word"]),
        cx_msg(5, &t(5), "user", &["疎にしすぎると困る"]),
        cx_msg(6, &t(6), "user", &["a\\b path"]),
    ];
    write_raw(&cx_file(d.path(), CX_DAY, CX_LOCAL, CX1), &lines);
    let n = |args: &[&str]| cx_hits(d.path(), args).len();
    assert_eq!(n(&[r"first\s+line\s+second"]), 1);
    assert_eq!(n(&["first line.second"]), 0, "`.` does not match a newline");
    assert_eq!(n(&["said \"hello there\" twice"]), 1);
    assert_eq!(n(&["said .hello"]), 1);
    assert_eq!(n(&[r"column\sseparated"]), 1);
    assert_eq!(n(&["mixed case", "-i"]), 1);
    assert_eq!(n(&["mixed case"]), 0);
    assert_eq!(n(&["(?i)mixed case"]), 1);
    assert_eq!(n(&["疎にしすぎ"]), 1);
    assert_eq!(n(&["nothing-like-this|Word"]), 1);
    assert_eq!(n(&[r"a\\b"]), 1);
    assert_eq!(n(&[r"\d+"]), 0);
    assert_eq!(n(&["second line$"]), 1);
}

#[test]
fn a_codex_session_in_two_places_is_read_once_from_the_larger_file() {
    let t = |s: u32| format!("2026-10-02T05:{s:02}:00.000Z");
    let small = [
        cx_meta(CX1, &t(0), CX_CWD, ""),
        cx_msg(1, &t(1), "user", &["twin: first"]),
    ];
    let large = [
        cx_meta(CX1, &t(0), CX_CWD, ""),
        cx_msg(1, &t(1), "user", &["twin: first"]),
        cx_msg(2, &t(2), "assistant", &["twin: second"]),
    ];
    // The larger one in either place.
    for (in_sessions, in_archive) in [(&small[..], &large[..]), (&large[..], &small[..])] {
        let d = tempfile::tempdir().unwrap();
        write_raw(&cx_file(d.path(), CX_DAY, CX_LOCAL, CX1), in_sessions);
        write_raw(
            &cx_file(d.path(), "archived_sessions", CX_LOCAL, CX1),
            in_archive,
        );
        let got = cx_hits(d.path(), &["twin"]);
        assert_eq!(snippets(&got), ["twin: first", "twin: second"]);
        let (code, out, err) = cx(d.path(), &["show", "0000c001"]);
        assert_eq!(code, 0, "one session, not two: {err}");
        assert!(out.contains("(2 utterances"), "{out}");
    }
}

#[test]
fn a_codex_session_is_named_by_the_end_of_its_id_and_show_takes_any_part() {
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (
        "019f0000-aaaa-7000-8000-0000aaaa1111",
        "019f0000-aaaa-7000-8000-0000bbbb2222",
    );
    for (sid, word) in [(a, "alpha"), (b, "beta")] {
        write_raw(
            &cx_file(d.path(), CX_DAY, CX_LOCAL, sid),
            &[
                cx_meta(sid, "2026-10-02T05:00:00.000Z", CX_CWD, ""),
                cx_msg(
                    1,
                    "2026-10-02T05:01:00.000Z",
                    "user",
                    &[&format!("a {word} talk")],
                ),
            ],
        );
    }
    let got = cx_hits(d.path(), &["talk"]);
    let ids: Vec<&str> = got.iter().map(|h| h["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        ["aaaa1111", "bbbb2222"],
        "the first 8 characters are shared"
    );
    // The end, a part from the middle, and the whole id.
    for spec in ["aaaa1111", "8000-0000aaaa", a] {
        let (code, out, err) = cx(d.path(), &["show", spec]);
        assert_eq!(code, 0, "{spec}: {err}");
        assert!(out.starts_with("# aaaa1111  codex  net/designer"), "{out}");
        assert!(
            out.contains("a alpha talk") && !out.contains("beta"),
            "{out}"
        );
    }
    // The shared start fits both: they are listed, as for Claude.
    let (code, _, err) = cx(d.path(), &["show", "019f0000"]);
    assert_eq!(code, 2);
    assert!(err.contains("fits 2 sessions"), "{err}");
    assert!(
        err.contains("aaaa1111  codex") && err.contains("bbbb2222  codex"),
        "{err}"
    );
}

#[test]
fn codex_show_lists_calls_in_a_line_and_leaves_out_a_copied_history() {
    let d = tempfile::tempdir().unwrap();
    let parent = "019f0000-aaaa-7000-8000-00000000c0aa";
    let t = |s: u32| format!("2026-10-02T05:{s:02}:00.000Z");
    // A forked subagent's file starts with its parent's lines (ordinals below 3).
    let extra = format!(
        r#","parent_thread_id":"{parent}","subagent_history_start_ordinal":3,"source":{{"subagent":{{"thread_spawn":{{"parent_thread_id":"{parent}"}}}}}}"#
    );
    let lines = [
        cx_meta(CX1, &t(0), CX_CWD, &extra),
        cx_msg(1, &t(1), "user", &["copied question from the parent"]),
        cx_msg(2, &t(2), "assistant", &["copied answer from the parent"]),
        cx_msg(3, &t(3), "user", &["own task: check the ports"]),
        cx_call(
            4,
            &t(4),
            "exec_command",
            &json!({"cmd": "ls dir\nsecond line"}),
        ),
        cx_other(
            5,
            &t(5),
            "response_item",
            "function_call_output",
            "RESULT text",
        ),
        cx_custom(
            6,
            &t(6),
            "apply_patch",
            "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** End Patch",
        ),
        cx_custom(
            7,
            &t(7),
            "exec",
            "const r = await tools.exec_command({cmd: \"git status\", workdir: \"/x\"});\ntext(r)",
        ),
        cx_call(
            8,
            &t(8),
            "shell",
            &json!({"command": ["bash", "-lc", "pwd"]}),
        ),
        cx_msg(9, &t(9), "assistant", &["checked"]),
    ];
    write_raw(&cx_file(d.path(), CX_DAY, CX_LOCAL, CX1), &lines);
    let (code, out, err) = cx(d.path(), &["show", "c001"]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("own task: check the ports") && out.contains("checked"),
        "{out}"
    );
    assert!(!out.contains("copied"), "{out}");
    assert!(out.contains("  [exec_command] ls dir…"), "{out}");
    assert!(
        out.contains("  [apply_patch] Update File: src/a.rs"),
        "{out}"
    );
    assert!(out.contains("  [exec_command] git status"), "{out}");
    assert!(out.contains("  [shell] pwd"), "{out}");
    assert!(
        !out.contains("RESULT") && !out.contains("second line"),
        "{out}"
    );
    assert!(out.contains("(2 utterances, 4 tool calls)"), "{out}");
    // A subagent's prompt is the parent's, not the user's.
    let any = cx_hits(d.path(), &["task", "--who", "any"]);
    assert_eq!(any.len(), 1);
    assert_eq!(any[0]["who"], "task");
    assert_eq!(cx_hits(d.path(), &["task", "--who", "user"]).len(), 0);
}

#[test]
fn source_picks_the_logs_and_each_line_says_which_it_came_from() {
    let d = tempfile::tempdir().unwrap();
    let claude = d.path().join("claude");
    write_lines(
        &claude.join("-p-net").join(format!("{S1}.jsonl")),
        &[user(S1, "2026-10-02T05:00:00Z", "pivot from claude")],
    );
    let codex = d.path().join("codex");
    write_raw(
        &cx_file(&codex, CX_DAY, CX_LOCAL, CX1),
        &[
            cx_meta(CX1, "2026-10-02T05:00:00.000Z", CX_CWD, ""),
            cx_msg(1, "2026-10-02T05:01:00.000Z", "user", &["pivot from codex"]),
        ],
    );
    fs::create_dir_all(codex.join("archived_sessions")).unwrap();
    let (a, b) = (
        codex.join("sessions").to_str().unwrap().to_string(),
        codex
            .join("archived_sessions")
            .to_str()
            .unwrap()
            .to_string(),
    );
    let sources = |extra: &[&str]| {
        let mut args = vec![
            "search",
            "pivot",
            "--json",
            "--codex-root",
            &a,
            "--codex-root",
            &b,
        ];
        args.extend(extra);
        let (code, out, err) = run(&claude, &args);
        assert_eq!(code, 0, "{err}");
        out.lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .map(|h| h["source"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(sources(&[]), ["claude", "codex"]);
    assert_eq!(sources(&["--source", "all"]), ["claude", "codex"]);
    assert_eq!(sources(&["--source", "claude"]), ["claude"]);
    assert_eq!(sources(&["--source", "codex"]), ["codex"]);
    // The text form carries the source too.
    let (_, out, _) = run(
        &claude,
        &["search", "pivot", "--codex-root", &a, "--codex-root", &b],
    );
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].contains("claude") && lines[1].contains("codex"),
        "{out}"
    );
    // `show` finds the session in either, and `--source` narrows it.
    let roots = ["--codex-root", &a, "--codex-root", &b];
    let mut args = vec!["show", "0000c001"];
    args.extend(roots);
    let (code, out, _) = run(&claude, &args);
    assert_eq!(code, 0, "{out}");
    assert!(out.starts_with("# 0000c001  codex"), "{out}");
    args.extend(["--source", "claude"]);
    let (code, _, _) = run(&claude, &args);
    assert_eq!(code, 2);
}

#[test]
fn source_and_codex_root_have_clear_failures() {
    let d = tempfile::tempdir().unwrap();
    // Asked for alone with nowhere to read: exit 3 and where to look.
    let (code, _, err) = run(d.path(), &["search", "x", "--source", "codex"]);
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("--codex-root"), "{err}");
    // Beside Claude's logs, no Codex logs on the machine is not an error.
    let (code, _, err) = run(d.path(), &["search", "x"]);
    assert_eq!(code, 0, "{err}");
    let missing = d.path().join("missing");
    let (code, _, err) = run(
        d.path(),
        &["search", "x", "--codex-root", missing.to_str().unwrap()],
    );
    assert_eq!(code, 3, "{err}");
    assert!(err.contains("missing"), "{err}");
    for args in [
        &["search", "x", "--source", "bogus"][..],
        &["repeats", "--source", "codex"],
        &["tool", "rpatch", "--codex-root", "/x"],
    ] {
        let (code, _, err) = run(d.path(), args);
        assert_eq!(code, 2, "{args:?}: {err}");
    }
}

#[test]
fn codex_files_are_chosen_by_the_window() {
    let d = tempfile::tempdir().unwrap();
    let t = |day: u32| format!("2026-10-{day:02}T05:00:00.000Z");
    write_raw(
        &cx_file(d.path(), CX_DAY, CX_LOCAL, CX1),
        &[
            cx_meta(CX1, &t(2), CX_CWD, ""),
            cx_msg(1, &t(2), "user", &["window early"]),
            cx_msg(2, &t(9), "user", &["window late"]),
        ],
    );
    assert_eq!(cx_hits(d.path(), &["window"]).len(), 2);
    let got = cx_hits(d.path(), &["window", "--until", "2026-10-05T00:00Z"]);
    assert_eq!(snippets(&got), ["window early"]);
    let got = cx_hits(d.path(), &["window", "--since", "2026-10-05T00:00Z"]);
    assert_eq!(snippets(&got), ["window late"]);
    // Before the session began: the file is not even opened.
    assert_eq!(
        cx_hits(d.path(), &["window", "--until", "2026-09-01"]).len(),
        0
    );
    // A file last written before the window holds nothing in it.
    assert_eq!(
        cx_hits(d.path(), &["window", "--since", "2099-01-01"]).len(),
        0
    );
}

/// Codex's logs under HOME are left out of search unless asked for; show still finds them.
#[test]
fn search_reads_codex_only_when_asked_and_show_finds_it_anyway() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let claude = d.path().join("claude");
    fs::create_dir_all(&claude).unwrap();
    let codex = home.join(".codex");
    fs::create_dir_all(codex.join("sessions/2026/10/02")).unwrap();
    let t = |s: u32| format!("2026-10-02T05:{s:02}:00.123Z");
    write_raw(
        &cx_file(&codex, CX_DAY, CX_LOCAL, CX1),
        &[
            cx_meta(CX1, &t(0), CX_CWD, ""),
            cx_msg(1, &t(1), "user", &["ZEBRA from codex"]),
        ],
    );
    let run_home = |args: &[&str]| {
        let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
            .args(args)
            .args(["--root", claude.to_str().unwrap()])
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env("HOME", &home)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout).into_owned();
        (o.status.code().unwrap_or(-1), out)
    };
    let (code, out) = run_home(&["search", "ZEBRA"]);
    assert_eq!(code, 0);
    assert!(!out.contains("ZEBRA from codex"), "{out}");
    let (_, out) = run_home(&["search", "ZEBRA", "--source", "all"]);
    assert!(out.contains("ZEBRA from codex"), "{out}");
    let (code, out) = run_home(&["show", &CX1[CX1.len() - 8..]]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("ZEBRA from codex"), "{out}");
}

// timeline: git repositories made here, and sessions written by hand that work in them

/// `git` in `dir` with fixed names, no global config, and the times given.
fn git_in(dir: &Path, author: &str, committer: &str, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=tester",
            "-c",
            "user.email=tester@example.com",
        ])
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_DATE", author)
        .env("GIT_COMMITTER_DATE", committer)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// An empty commit made at `author` and recorded (rebased, say) at `committer`.
fn commit(dir: &Path, author: &str, committer: &str, subject: &str) {
    git_in(
        dir,
        author,
        committer,
        &["commit", "--allow-empty", "-q", "-m", subject],
    );
}

/// A new repository at `dir` (made, with its real path returned: git reports that one).
fn init_repo(dir: &Path) -> std::path::PathBuf {
    fs::create_dir_all(dir).unwrap();
    let dir = fs::canonicalize(dir).unwrap();
    git_in(
        &dir,
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00Z",
        &["init", "-q"],
    );
    dir
}

/// One typed line of a session that was working in `cwd`.
fn said_in(cwd: &Path, sid: &str, ts: &str, who: &str, text: &str) -> Value {
    let mut v = if who == "user" {
        user(sid, ts, text)
    } else {
        says(sid, ts, text)
    };
    v["cwd"] = json!(cwd.to_str().unwrap());
    v
}

fn sid(n: u32) -> String {
    format!("{n:08x}-1111-4000-8000-000000000001")
}

/// `agentlog timeline --repo REPO …` with the clock on UTC and HOME as given.
fn tl(root: &Path, home: &Path, repo: &Path, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
        .args(["timeline", "--repo", repo.to_str().unwrap()])
        .args(["--root", root.to_str().unwrap()])
        .args([
            "--since",
            "2026-10-01T00:00Z",
            "--until",
            "2026-10-20T00:00Z",
        ])
        .args(args)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env("HOME", home)
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

fn items(root: &Path, home: &Path, repo: &Path, args: &[&str]) -> Vec<Value> {
    let mut a = args.to_vec();
    a.push("--json");
    let (code, out, err) = tl(root, home, repo, &a);
    assert_eq!(code, 0, "stderr: {err}");
    out.lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}")))
        .collect()
}

/// `kind:text` of each item, to read an order at a glance.
fn story(v: &[Value]) -> Vec<String> {
    v.iter()
        .map(|i| {
            format!(
                "{}:{}",
                i["kind"].as_str().unwrap(),
                i["snippet"].as_str().unwrap()
            )
        })
        .collect()
}

#[test]
fn timeline_puts_the_talk_and_the_commits_in_one_order_by_time() {
    let d = tempfile::tempdir().unwrap();
    let repo = init_repo(&d.path().join("proj/designer"));
    let t = |h: u32, m: u32| format!("2026-10-05T{h:02}:{m:02}:00Z");
    commit(&repo, &t(10, 0), &t(10, 0), "first change");
    // Rebased the next day: the author time says when it was written, and decides the place.
    commit(&repo, &t(10, 20), "2026-10-06T09:00:00Z", "second change");
    // On another branch, merged with a merge commit that is left out of the list.
    git_in(
        &repo,
        &t(10, 25),
        &t(10, 25),
        &["checkout", "-q", "-b", "side"],
    );
    commit(&repo, &t(10, 40), &t(10, 40), "side change");
    git_in(&repo, &t(10, 45), &t(10, 45), &["checkout", "-q", "main"]);
    git_in(
        &repo,
        &t(10, 50),
        &t(10, 50),
        &["merge", "--no-ff", "-q", "-m", "Merge side", "side"],
    );
    commit(
        &repo,
        "2026-10-07T08:00:00Z",
        "2026-10-07T08:00:00Z",
        "after the window",
    );

    let root = d.path().join("claude");
    let s = sid(1);
    write_lines(
        &root.join(format!("-p/{s}.jsonl")),
        &[
            said_in(&repo, &s, &t(9, 50), "user", "ask one"),
            said_in(&repo, &s, &t(10, 10), "user", "ask two"),
            said_in(&repo, &s, &t(10, 15), "assistant", "answer two"),
            said_in(&repo, &s, &t(10, 30), "user", "ask three"),
            said_in(&repo, &s, &t(10, 55), "user", "ask four"),
        ],
    );
    let home = d.path().join("home");
    let v = items(&root, &home, &repo, &["--until", "2026-10-06T00:00Z"]);
    assert_eq!(
        story(&v),
        [
            "user:ask one",
            "commit:first change",
            "user:ask two",
            "commit:second change",
            "user:ask three",
            "commit:side change",
            "user:ask four",
        ]
    );
    assert_eq!(v[1]["hash"].as_str().unwrap().len(), 40);
    assert_eq!(v[0]["id"], "00000001");
    assert_eq!(v[0]["session"], s.as_str());
    // The assistant's text comes with --who any, in its place.
    let all = items(
        &root,
        &home,
        &repo,
        &["--until", "2026-10-06T00:00Z", "--who", "any"],
    );
    let at = story(&all)
        .iter()
        .position(|x| x == "assistant:answer two")
        .unwrap();
    assert_eq!(story(&all)[at - 1], "user:ask two");
    // The window applies to commits by author time too.
    let wide = items(&root, &home, &repo, &[]);
    assert!(story(&wide).contains(&"commit:after the window".to_string()));
    assert!(
        !story(&items(
            &root,
            &home,
            &repo,
            &["--until", "2026-10-07T00:00Z"]
        ))
        .contains(&"commit:after the window".to_string())
    );

    // The text: a line per item with the local time, a rule where the day changes, and the
    // session id only where the session changes.
    let (code, out, err) = tl(&root, &home, &repo, &["--until", "2026-10-06T00:00Z"]);
    assert_eq!(code, 0, "{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "--- 2026-10-05 ---");
    assert!(
        lines[1].starts_with("2026-10-05 09:50  user       00000001  | ask one"),
        "{out}"
    );
    assert!(
        lines[2].starts_with("2026-10-05 10:00  commit     "),
        "{out}"
    );
    assert!(
        lines[3].starts_with("2026-10-05 10:10  user       00000001  | ask two"),
        "{out}"
    );
    let (_, out, _) = tl(
        &root,
        &home,
        &repo,
        &["--until", "2026-10-06T00:00Z", "--who", "any"],
    );
    let l: Vec<&str> = out.lines().collect();
    let i = l.iter().position(|x| x.contains("| ask two")).unwrap();
    assert!(
        l[i + 1].contains("assistant") && !l[i + 1].contains("00000001"),
        "{out}"
    );
}

#[test]
fn timeline_takes_the_sessions_that_worked_in_the_repository_or_a_worktree_of_it() {
    let d = tempfile::tempdir().unwrap();
    let repo = init_repo(&d.path().join("proj/designer"));
    commit(
        &repo,
        "2026-10-05T10:00:00Z",
        "2026-10-05T10:00:00Z",
        "a change",
    );
    let parent = repo.parent().unwrap().to_path_buf();
    let other = parent.join("other");
    let lookalike = parent.join("designer-two");
    // A worktree git still lists, and one T3 Code made and deleted since.
    let live = d.path().join("elsewhere/live");
    git_in(
        &repo,
        "2026-10-05T10:01:00Z",
        "2026-10-05T10:01:00Z",
        &["worktree", "add", "-q", "-b", "wt", live.to_str().unwrap()],
    );
    let live = fs::canonicalize(&live).unwrap();
    let home = d.path().join("home");
    let t3 = home.join(".t3/worktrees/designer/feature");
    let t3_of_another = home.join(".t3/worktrees/other-app/feature");

    let root = d.path().join("claude");
    let mut n = 0;
    let mut session = |label: &str, lines: Vec<(&Path, u32)>| {
        n += 1;
        let s = sid(n);
        let rows: Vec<Value> = lines
            .iter()
            .enumerate()
            .flat_map(|(k, (cwd, count))| {
                (0..*count).map(move |j| (k, *cwd, j)).collect::<Vec<_>>()
            })
            .enumerate()
            .map(|(i, (_, cwd, _))| {
                let ts = format!("2026-10-05T11:{i:02}:00Z");
                // The first line is the one that is looked for; the rest is only where it was.
                let who = if i == 0 { "user" } else { "assistant" };
                said_in(cwd, &s, &ts, who, &format!("{label} {i}"))
            })
            .collect();
        write_lines(&root.join(format!("-p/{s}.jsonl")), &rows);
        s
    };
    let inside = session("inside", vec![(repo.as_path(), 2)]);
    let sub = session("subfolder", vec![(repo.join("backend").as_path(), 2)]);
    let in_live = session("live", vec![(live.as_path(), 2)]);
    let in_t3 = session("t3", vec![(t3.as_path(), 2)]);
    // Opened in the parent folder, then mostly worked in the repository.
    let from_parent = session(
        "from parent",
        vec![(parent.as_path(), 6), (repo.as_path(), 3)],
    );
    let _parent_only = session("parent only", vec![(parent.as_path(), 8)]);
    let _sibling = session("sibling", vec![(other.as_path(), 3)]);
    let _lookalike = session("lookalike", vec![(lookalike.as_path(), 3)]);
    let _t3_other = session("t3 other", vec![(t3_of_another.as_path(), 3)]);
    // Opened in the parent folder, but worked more in the sibling.
    let _mostly_sibling = session(
        "mostly sibling",
        vec![
            (parent.as_path(), 6),
            (repo.as_path(), 2),
            (other.as_path(), 4),
        ],
    );
    // Half here, half there: kept.
    let half = session("half", vec![(repo.as_path(), 3), (other.as_path(), 3)]);

    let picked = |args: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = items(&root, &home, &repo, args)
            .iter()
            .filter(|i| i["kind"] == "user")
            .map(|i| i["session"].as_str().unwrap().to_string())
            .collect();
        v.sort();
        v
    };
    let mut want = vec![inside, sub, in_live, in_t3, from_parent, half.clone()];
    want.sort();
    assert_eq!(picked(&[]), want);

    // --project names the sessions by the directory they started in, not by the repository.
    assert_eq!(picked(&["--project", "proj/other"]), [sid(7)]);
}

#[test]
fn timeline_needs_a_git_repository_and_says_so() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("claude");
    fs::create_dir_all(&root).unwrap();
    let plain = d.path().join("not-a-repo");
    fs::create_dir_all(&plain).unwrap();
    let (code, out, err) = tl(&root, &home, &plain, &[]);
    assert_eq!((code, out.as_str()), (2, ""), "{err}");
    assert!(
        err.contains("--repo") && err.contains("not a git repository"),
        "{err}"
    );
    let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
        .args(["timeline", "--root", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("timeline needs --repo DIR"));
    let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
        .args(["search", "x", "--repo", "."])
        .args(["--root", root.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--repo is for timeline"));
    // An empty window is said, with where sessions were looked for; exit 0.
    let repo = init_repo(&d.path().join("repo"));
    let (code, out, _) = tl(&root, &home, &repo, &[]);
    assert_eq!(code, 0);
    assert!(out.starts_with("0 items"), "{out}");
    assert!(out.contains(repo.to_str().unwrap()), "{out}");
}

fn agentlog(args: &[&str]) -> (Option<i32>, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
        .args(args)
        .output()
        .unwrap();
    (
        o.status.code(),
        String::from_utf8(o.stdout).unwrap(),
        String::from_utf8(o.stderr).unwrap(),
    )
}

const HELP_TXT: &str = include_str!("../src/help.txt");

/// The headings of help.txt: lines not indented, after the summary.
fn help_headings() -> Vec<&'static str> {
    let lines: Vec<&str> = HELP_TXT.lines().collect();
    let exit = lines
        .iter()
        .position(|l| l.starts_with("Exit status:"))
        .unwrap();
    let blank = exit + lines[exit..].iter().position(|l| l.is_empty()).unwrap();
    let mut out = Vec::new();
    for i in blank + 1..lines.len() {
        if !lines[i].is_empty() && !lines[i].starts_with(' ') {
            assert!(
                lines[i - 1].is_empty(),
                "help.txt: `{}` is not indented but follows a line; a heading needs a blank line before it",
                lines[i]
            );
            out.push(lines[i]);
        }
    }
    out
}

/// Agents read the help with `head -40`: `--help` is the summary (usage, examples, output and exit
/// status) and a pointer to the rest, and all of it has to fit in those lines.
#[test]
fn the_summary_of_the_help_fits_in_forty_lines() {
    for args in [&["--help"][..], &["-h"], &["help"], &[]] {
        let (code, out, _) = agentlog(args);
        assert_eq!(code, Some(0));
        let n = out.lines().count();
        assert!(
            n <= 40,
            "`agentlog {args:?}` prints {n} lines; `head -40` cuts off:\n{out}"
        );
        assert!(
            out.contains("Exit status:") && out.contains("agentlog help all"),
            "{out}"
        );
    }
}

#[test]
fn help_for_a_command_has_its_section_and_not_the_others() {
    let (code, out, _) = agentlog(&["help", "rules"]);
    assert_eq!(code, Some(0));
    assert!(out.contains("agentlog rules [--repo DIR]"), "{out}");
    assert!(out.contains("\nrules: count the prohibitions"), "{out}");
    assert!(!out.contains("search: one line per utterance"), "{out}");
    assert!(!out.contains("Examples:"), "{out}");
    // repeats, tool and daily share the section that says what is counted.
    let (_, out, _) = agentlog(&["help", "tool"]);
    assert!(
        out.contains("agentlog tool NAME") && out.contains("[--escape-tool T,T]"),
        "{out}"
    );
    assert!(
        out.contains("What it counts") && out.contains("Presets: rpatch"),
        "{out}"
    );
    assert!(!out.contains("agentlog daily"), "{out}");
    // search points to the Codex and cache sections; they are asked for by name.
    let (_, out, _) = agentlog(&["help", "search"]);
    assert!(
        out.contains("agentlog help codex") && out.contains("agentlog help cache"),
        "{out}"
    );
    let (_, out, _) = agentlog(&["help", "codex"]);
    assert!(out.starts_with("Codex logs"), "{out}");
}

#[test]
fn command_dash_dash_help_is_help_for_the_command() {
    for name in [
        "repeats", "tool", "daily", "search", "show", "timeline", "memory", "rules",
    ] {
        let (code, out, _) = agentlog(&["help", name]);
        assert_eq!(code, Some(0));
        assert_eq!(
            agentlog(&[name, "--help"]),
            (Some(0), out.clone(), String::new()),
            "{name}"
        );
        assert_eq!(
            agentlog(&[name, "-h"]),
            (Some(0), out, String::new()),
            "{name}"
        );
    }
}

#[test]
fn help_all_is_the_whole_of_help_txt() {
    let (code, out, _) = agentlog(&["help", "all"]);
    assert_eq!(code, Some(0));
    assert_eq!(out, HELP_TXT);
}

#[test]
fn help_for_an_unknown_name_says_so_and_lists_the_names() {
    let (code, out, err) = agentlog(&["help", "foo"]);
    assert_eq!(code, Some(2));
    assert!(out.is_empty());
    let first = err.lines().next().unwrap();
    assert!(
        first.contains("`foo`") && first.contains("rules") && first.contains("cache"),
        "{err}"
    );
    assert_eq!(agentlog(&["help", "a", "b"]).0, Some(2));
}

/// A section of help.txt that no name asks for would be written but never read.
#[test]
fn every_section_of_help_txt_is_asked_for_by_some_name() {
    let (_, _, err) = agentlog(&["help", "foo"]);
    let list = err.lines().next().unwrap().split("try: ").nth(1).unwrap();
    let names: Vec<&str> = list.split(", ").filter(|n| *n != "all").collect();
    assert!(names.len() >= 8, "{names:?}");
    let mut shown = String::new();
    for name in names {
        let (code, out, _) = agentlog(&["help", name]);
        assert_eq!(code, Some(0), "{name}");
        shown.push_str(&out);
    }
    let headings = help_headings();
    assert!(headings.len() >= 8, "{headings:?}");
    for h in headings {
        assert!(
            shown.lines().any(|l| l == h),
            "no name of `agentlog help` prints the section `{h}`"
        );
    }
}

/// A tool call and its failed result, as two lines; `extra` is merged into both.
fn failed_call(id: &str, ts: &str, result: &str, extra: Value) -> String {
    let mut use_line = json!({
        "type": "assistant", "timestamp": ts,
        "message": {"content": [{"type": "tool_use", "id": id, "name": "Bash", "input": {"command": "cargo build"}}]}
    });
    let mut result_line = json!({
        "type": "user", "timestamp": ts,
        "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": result, "is_error": true}]}
    });
    for line in [&mut use_line, &mut result_line] {
        for (k, v) in extra.as_object().unwrap() {
            line[k] = v.clone();
        }
    }
    format!("{use_line}\n{result_line}\n")
}

const BOOM: &str = "Exit code 101\nerror: boom";

#[test]
fn a_copied_history_is_counted_once() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("-p-copy");
    let first = failed_call("toolu_a", "2026-10-05T01:00:00Z", BOOM, json!({}));
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("s1.jsonl"), &first).unwrap();
    // A later session that starts with a copy of s1's history, then fails again itself.
    let own = failed_call("toolu_b", "2026-10-05T02:00:00Z", BOOM, json!({}));
    fs::write(p.join("s2.jsonl"), format!("{first}{own}")).unwrap();
    let j = json_of(d.path(), &["repeats", "--all"]);
    assert_eq!(j["calls"], 2);
    let s = &j["signatures"][0];
    assert_eq!(
        (s["total"].clone(), s["ctx"].clone(), s["rep"].clone()),
        (json!(2), json!(2), json!(0))
    );
}

#[test]
fn a_result_cut_inside_an_emoji_still_counts_as_a_failure() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("-p-emoji");
    fs::create_dir_all(&p).unwrap();
    // JSON.stringify writes half of a surrogate pair as `\ud83d`; serde_json refuses it.
    let cut = failed_call(
        "toolu_c",
        "2026-10-05T01:00:00Z",
        "Exit code 101\nerror: boom CUT",
        json!({}),
    )
    .replace("CUT", "\\ud83d");
    let whole = failed_call("toolu_d", "2026-10-05T01:01:00Z", BOOM, json!({}));
    fs::write(p.join("s1.jsonl"), format!("{cut}{whole}")).unwrap();
    let j = json_of(d.path(), &["daily", "x=cargo", "--failed"]);
    assert_eq!(j["total"]["x"], 2);
}

#[test]
fn a_last_line_cut_inside_a_character_leaves_the_rest_of_the_file() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("-p-cut");
    fs::create_dir_all(&p).unwrap();
    let mut bytes = failed_call("toolu_e", "2026-10-05T01:00:00Z", BOOM, json!({})).into_bytes();
    // the line being written stops after two of the three bytes of `あ`
    bytes.extend_from_slice(
        b"{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"\xe3\x81",
    );
    fs::write(p.join("s1.jsonl"), bytes).unwrap();
    let j = json_of(d.path(), &["repeats", "--all"]);
    assert_eq!(j["calls"], 1);
    assert_eq!(j["signatures"][0]["total"], 1);
}

#[test]
fn a_subagent_inside_its_parents_file_is_its_own_context() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("-p-side");
    fs::create_dir_all(&p).unwrap();
    let parent = failed_call("toolu_f", "2026-10-05T01:00:00Z", BOOM, json!({}));
    let mut side = json!({ "agentId": "a1b2c3" });
    side["isSidechain"] = json!(true);
    let sub = failed_call("toolu_g", "2026-10-05T01:01:00Z", BOOM, side);
    fs::write(p.join("s1.jsonl"), format!("{parent}{sub}")).unwrap();
    let j = json_of(d.path(), &["repeats", "--all", "--examples", "5"]);
    assert_eq!(j["contexts"], 2);
    let s = &j["signatures"][0];
    assert_eq!((s["ctx"].clone(), s["rep"].clone()), (json!(2), json!(0)));
    let ctxs: Vec<&str> = s["examples"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["context"].as_str().unwrap())
        .collect();
    assert!(
        ctxs.iter().any(|c| c.ends_with("/s1/agent-a1b2c3")),
        "{ctxs:?}"
    );
}

#[test]
fn search_shows_a_copied_history_once_and_reads_a_cut_emoji() {
    let d = tempfile::tempdir().unwrap();
    let s2 = "bbbbbbbb-2222-4000-8000-000000000002";
    let p = d.path().join("-p-net");
    let said = |sid| user(sid, "2026-10-02T05:00:00Z", "ZEBRA once");
    write_lines(&p.join(format!("{S1}.jsonl")), &[said(S1)]);
    // s2 starts with a copy of s1's history, then goes on
    let own = user(s2, "2026-10-02T06:00:00Z", "ZEBRA again CUT")
        .to_string()
        .replace("CUT", "\\ud83d");
    fs::write(
        p.join(format!("{s2}.jsonl")),
        format!("{}\n{own}\n", said(s2)),
    )
    .unwrap();
    let got: Vec<(String, String)> = hits(d.path(), &["ZEBRA"])
        .iter()
        .map(|h| {
            (
                h["id"].as_str().unwrap().into(),
                h["snippet"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("aaaaaaaa".into(), "ZEBRA once".into()),
            ("bbbbbbbb".into(), "ZEBRA again \u{fffd}".into())
        ]
    );
}

#[test]
fn a_codex_fork_by_the_user_does_not_say_its_parents_words_again() {
    let d = tempfile::tempdir().unwrap();
    let parent = "019f0000-aaaa-7000-8000-00000000f001";
    let fork = "019f0000-aaaa-7000-8000-00000000f002";
    let based = "019f0000-aaaa-7000-8000-00000000f003";
    write_raw(
        &cx_file(d.path(), CX_DAY, CX_LOCAL, parent),
        &[
            cx_meta(parent, "2026-10-02T05:00:00.000Z", CX_CWD, ""),
            cx_msg(
                1,
                "2026-10-02T05:00:05.000Z",
                "user",
                &["ZEBRA rename the widget"],
            ),
        ],
    );
    // The fork writes a copy of its parent's lines as it is made, all stamped then.
    let from = format!(r#","forked_from_id":"{parent}""#);
    write_raw(
        &cx_file(d.path(), CX_DAY, "2026-10-02T15-00-00", fork),
        &[
            cx_meta(fork, "2026-10-02T06:00:00.000Z", CX_CWD, &from),
            cx_msg(
                1,
                "2026-10-02T06:00:00.400Z",
                "user",
                &["ZEBRA rename the widget"],
            ),
            cx_msg(
                2,
                "2026-10-02T06:01:00.000Z",
                "user",
                &["ZEBRA now the gadget"],
            ),
        ],
    );
    // A fork that points at its parent's history holds only its own lines.
    let base = format!(r#","forked_from_id":"{parent}","history_base":{{"thread_id":"{parent}"}}"#);
    write_raw(
        &cx_file(d.path(), CX_DAY, "2026-10-02T16-00-00", based),
        &[
            cx_meta(based, "2026-10-02T07:00:00.000Z", CX_CWD, &base),
            cx_msg(
                1,
                "2026-10-02T07:00:01.000Z",
                "user",
                &["ZEBRA said at once"],
            ),
        ],
    );
    let h = cx_hits(d.path(), &["ZEBRA"]);
    let got: Vec<(&str, &str)> = h
        .iter()
        .map(|h| (h["id"].as_str().unwrap(), h["snippet"].as_str().unwrap()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("0000f001", "ZEBRA rename the widget"),
            ("0000f002", "ZEBRA now the gadget"),
            ("0000f003", "ZEBRA said at once"),
        ]
    );
}

#[test]
fn a_local_time_near_a_change_of_daylight_saving_time_is_that_time() {
    let d = tempfile::tempdir().unwrap();
    for (when, local) in [
        ("2026-03-08T03:30", "2026-03-08 03:30"),
        ("2026-03-08T12:00", "2026-03-08 12:00"),
        ("2026-11-01T05:00", "2026-11-01 05:00"),
    ] {
        let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
            .args(["daily", "x=.", "--json", "--since", when])
            .args(["--root", d.path().to_str().unwrap()])
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env("HOME", "/nonexistent-home")
            .env("TZ", "America/New_York")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let j: Value = serde_json::from_slice(&o.stdout).unwrap();
        assert_eq!(j["since"], local, "{when}");
    }
}

#[test]
fn project_takes_the_name_search_shows_in_every_report() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("-Users-tester-work-shop-web");
    fs::create_dir_all(&p).unwrap();
    let cwd = json!({"cwd": "/Users/tester/work/shop/web"});
    let call = failed_call("toolu_h", "2026-10-05T01:00:00Z", BOOM, cwd);
    fs::write(p.join("s1.jsonl"), call).unwrap();
    for (name, n) in [("work/shop/web", 1), ("shop-web", 1), ("shop/api", 0)] {
        let j = json_of(d.path(), &["repeats", "--project", name]);
        assert_eq!(j["calls"], n, "{name}");
        let j = json_of(d.path(), &["daily", "x=.", "--project", name]);
        assert_eq!(j["total"]["x"], n, "{name}");
    }
}

/// The binary with only these variables for where things are (no --root).
fn run_env(env: &[(&str, &Path)], args: &[&str]) -> (i32, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_agentlog"));
    c.args(args)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env("HOME", "/nonexistent-home");
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn the_logs_are_where_claude_config_dir_and_codex_home_say() {
    let d = tempfile::tempdir().unwrap();
    let cfg = d.path().join("claude-config");
    let p = cfg.join("projects/-p");
    fs::create_dir_all(&p).unwrap();
    fs::write(
        p.join("s1.jsonl"),
        failed_call("toolu_i", "2026-10-05T01:00:00Z", BOOM, json!({})),
    )
    .unwrap();
    let window = [
        "--since",
        "2026-10-01T00:00Z",
        "--until",
        "2026-10-20T00:00Z",
        "--json",
    ];
    let mut a = vec!["repeats", "--all"];
    a.extend(window);
    let (code, out, err) = run_env(&[("CLAUDE_CONFIG_DIR", &cfg)], &a);
    assert_eq!(code, 0, "{err}");
    assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["calls"], 1);

    let codex = d.path().join("codex-home");
    write_raw(
        &cx_file(&codex, CX_DAY, CX_LOCAL, CX1),
        &[
            cx_meta(CX1, "2026-10-02T05:00:00.000Z", CX_CWD, ""),
            cx_msg(
                1,
                "2026-10-02T05:00:05.000Z",
                "user",
                &["ZEBRA in codex home"],
            ),
        ],
    );
    let (code, out, err) = run_env(
        &[("CODEX_HOME", &codex)],
        &["search", "ZEBRA", "--source", "codex"],
    );
    assert_eq!((code, out.lines().count()), (0, 1), "{err}");
    // show reads both by default: a machine with Codex's logs only is not an error
    let (code, out, err) = run_env(&[("CODEX_HOME", &codex)], &["show", "0000c001"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("ZEBRA in codex home"), "{out}");
    // a copy with one of its two places: the other is named and left out
    let missing = codex.join("archived_sessions");
    let sessions = codex.join("sessions");
    let (code, out, err) = run_env(
        &[],
        &[
            "search",
            "ZEBRA",
            "--source",
            "codex",
            "--codex-root",
            sessions.to_str().unwrap(),
            "--codex-root",
            missing.to_str().unwrap(),
        ],
    );
    assert_eq!((code, out.lines().count()), (0, 1), "{err}");
    assert!(
        err.contains("archived_sessions: no such directory"),
        "{err}"
    );
}

#[test]
fn an_option_of_another_command_is_refused() {
    let d = tempfile::tempdir().unwrap();
    for args in [
        &["repeats", "--who", "user"][..],
        &["daily", "x=a", "-v"],
        &["tool", "rpatch", "--in", "input"],
        &["search", "x", "--top", "1"],
        &["show", "abc", "--limit", "3"],
        &["unused", "-i"],
        &["timeline", "--repo", ".", "--full"],
    ] {
        let (code, _, err) = run(d.path(), args);
        assert_eq!(code, 2, "{args:?}: {err}");
        let first = err.lines().next().unwrap();
        assert!(first.contains("does not take it"), "{args:?}: {first}");
    }
    // the options a command reads still pass
    let (code, _, err) = run(
        d.path(),
        &["repeats", "--top", "3", "--all", "-v", "--tool", "Bash"],
    );
    assert_eq!(code, 0, "{err}");
}

/// An unclosed code fence turns the rest of a document into code.
#[test]
fn every_code_fence_in_the_docs_is_closed() {
    for doc in ["README.md", "docs/design.md", "docs/recipes.md"] {
        let text = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(doc)).unwrap();
        let fences = text
            .lines()
            .filter(|l| l.trim_start().starts_with("```"))
            .count();
        assert_eq!(fences % 2, 0, "{doc} has {fences} fences");
    }
}

#[test]
fn memory_reads_no_window_so_its_order_is_not_checked() {
    let d = tempfile::tempdir().unwrap();
    let args = [
        "memory",
        "x",
        "--since",
        "2026-10-05",
        "--until",
        "2026-10-01",
    ];
    let (code, _, err) = run(d.path(), &args);
    assert_eq!(code, 0, "{err}");
    let (code, _, _) = run(
        d.path(),
        &["repeats", "--since", "2026-10-05", "--until", "2026-10-01"],
    );
    assert_eq!(code, 2);
}
