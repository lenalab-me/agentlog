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
