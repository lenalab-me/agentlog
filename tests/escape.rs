//! `tool rpatch`: going another way counts as an escape only on the file that failed.
//! Runs the binary on small session logs written here, and reads its output.

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

const NOT_FOUND: &str = "Exit code 1\nerror: edits[0] src/a.rs (input line 2): \"old\" not found\nrpatch: nothing written";

/// A session of tool calls: (tool, input, result, failed).
fn session(path: &Path, calls: &[(&str, Value, &str, bool)]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut out = String::new();
    for (i, (tool, input, result, failed)) in calls.iter().enumerate() {
        let ts = format!("2026-10-05T01:{i:02}:00Z");
        let id = format!("toolu_{i}");
        let use_line = json!({
            "type": "assistant", "timestamp": ts,
            "message": {"content": [{"type": "tool_use", "id": id, "name": tool, "input": input}]}
        });
        let result_line = json!({
            "type": "user", "timestamp": ts,
            "message": {"content": [{"type": "tool_result", "tool_use_id": id, "content": result, "is_error": failed}]}
        });
        out.push_str(&format!("{use_line}\n{result_line}\n"));
    }
    fs::write(path, out).unwrap();
}

fn bash(cmd: &str) -> Value {
    json!({ "command": cmd })
}

fn file(path: &str) -> Value {
    json!({ "file_path": path })
}

/// A failed rpatch call, then the call after it.
fn failed_then(root: &Path, name: &str, rpatch: &str, result: &str, next: (&str, Value)) {
    session(
        &root.join(format!("-p/{name}.jsonl")),
        &[
            ("Bash", bash(rpatch), result, true),
            (next.0, next.1, "", false),
        ],
    );
}

fn run(root: &Path, args: &[&str]) -> String {
    let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
        .args(args)
        .args(["--root", root.to_str().unwrap()])
        .args([
            "--since",
            "2026-10-01T00:00Z",
            "--until",
            "2026-10-20T00:00Z",
        ])
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env("HOME", "/nonexistent-home")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn an_escape_is_another_way_on_the_file_that_failed() {
    let d = tempfile::tempdir().unwrap();
    let r = d.path();
    // the same file, read from each way rpatch is told it and each way of writing it
    failed_then(
        r,
        "same-arg",
        "rpatch src/a.rs <<'EOF'",
        NOT_FOUND,
        ("Edit", file("/w/src/a.rs")),
    );
    failed_then(
        r,
        "same-block",
        "cd /w && rpatch <<'EOF'\n=== src/b.rs\n<<<<<<< OLD\nx\n=======\ny\n>>>>>>> NEW\nEOF",
        NOT_FOUND,
        ("Bash", bash("sed -i '' 's/x/y/' /w/src/b.rs")),
    );
    failed_then(
        r,
        "same-json",
        "rpatch <<'EOF'\n{\"edits\": [{\"file\": \"/w/c.py\", \"old\": \"a\", \"new\": \"b\"}]}\nEOF",
        NOT_FOUND,
        ("Bash", bash("cd /w && perl -pi -e 's/a/b/' c.py")),
    );
    failed_then(
        r,
        "same-error",
        "rpatch < /tmp/d.rpatch",
        "Exit code 1\nerror: edits[0] src/d.rs (input line 2): \"old\" not found",
        (
            "Bash",
            bash(
                "python3 - <<'EOF'\nfrom pathlib import Path\np = Path('src/d.rs')\np.write_text(p.read_text().replace('a', 'b'))\nEOF",
            ),
        ),
    );
    // another file: not an escape
    failed_then(
        r,
        "another-edit",
        "rpatch src/a.rs <<'EOF'",
        NOT_FOUND,
        ("Edit", file("/w/src/other.rs")),
    );
    failed_then(
        r,
        "another-cat",
        "rpatch <<'EOF'\n=== src/e.rs\nEOF",
        NOT_FOUND,
        ("Bash", bash("cat > notes.md <<'EOF'\nnotes\nEOF")),
    );
    failed_then(
        r,
        "another-python",
        "rpatch src/a.rs <<'EOF'",
        NOT_FOUND,
        ("Bash", bash("python3 -c \"open('f.txt', 'w').write('x')\"")),
    );
    // the file is not known on one side: counted as an escape, apart
    failed_then(
        r,
        "unknown-input",
        "rpatch <<'EOF'\nEOF",
        "Exit code 2\nrpatch: no edits in input (see rpatch --help)",
        ("Edit", file("/w/x.rs")),
    );
    failed_then(
        r,
        "unknown-variable",
        "rpatch src/a.rs <<'EOF'",
        NOT_FOUND,
        ("Bash", bash("sed -i '' s/a/b/ \"$f\"")),
    );

    let j: Value = serde_json::from_str(&run(r, &["tool", "rpatch", "--json"])).unwrap();
    assert_eq!(
        j["after_failure"],
        json!({
            "went another way on the same file": 4,
            "another way on another file (not an escape)": 3,
            "went another way, file not known": 2,
        })
    );
    assert_eq!(
        j["went_to"],
        json!({"Edit": 2, "Bash sed": 2, "Bash perl": 1, "Bash python3": 1})
    );
    assert_eq!(
        j["another_file"],
        json!({"Edit": 1, "Bash cat": 1, "Bash python3": 1})
    );

    let text = run(r, &["tool", "rpatch", "--examples", "9"]);
    assert!(
        text.contains("    on another file: Bash cat 1, Bash python3 1, Edit 1\n"),
        "{text}"
    );
    assert!(text.contains("=> Edit: /w/src/a.rs  (same file)"), "{text}");
    assert!(
        text.contains("=> Edit: /w/x.rs  (file not known)"),
        "{text}"
    );
    let another = text
        .split("another way on another file (not an escape)\n")
        .last()
        .unwrap();
    assert!(another.contains("=> Edit: /w/src/other.rs\n"), "{text}");
}

#[test]
fn a_tool_without_files_goes_another_way_as_before() {
    let d = tempfile::tempdir().unwrap();
    failed_then(
        d.path(),
        "s",
        "devsrv stop web",
        "Exit code 1\ndevsrv: nothing named web was started here",
        ("Bash", bash("pkill -f vite")),
    );
    let j: Value = serde_json::from_str(&run(d.path(), &["tool", "devsrv", "--json"])).unwrap();
    assert_eq!(j["after_failure"], json!({"went another way": 1}));
    assert_eq!(j["went_to"], json!({"Bash pkill": 1}));
}
