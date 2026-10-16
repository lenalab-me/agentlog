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
    for (i, e) in evs.iter().enumerate() {
        let id = format!("toolu_{i}");
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
fn what_the_user_refused_is_not_a_repeat() {
    let d = tempfile::tempdir().unwrap();
    let no = "The user doesn't want to proceed with this tool use.";
    write_log(
        &d.path().join("-p/s1.jsonl"),
        &[
            tool("2026-10-05T01:00:00Z", "Edit", "/a", no, true),
            tool("2026-10-05T01:01:00Z", "Edit", "/b", no, true),
        ],
    );
    assert_eq!(json_of(d.path(), &["repeats"])["signatures"], json!([]));
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
                "/a.rs",
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
            "went another way": 1,
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
    assert!(out.starts_with("# aaaaaaaa  net/designer"), "{out}");
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
