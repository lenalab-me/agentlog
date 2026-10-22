//! `unused` on settings and logs written here, with made-up names. HOME is a folder made here
//! and PATH one folder of made-up programs, so nothing of the real machine is read.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

struct World {
    _dir: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn program(bin: &Path, name: &str) {
    let p = bin.join(name);
    write(&p, "#!/bin/sh\n");
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

fn tool_use(ts: &str, name: &str, input: Value) -> String {
    // every call has its own id, as in Claude Code's logs (a copied history shares them)
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let id = format!(
        "toolu_{}",
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    json!({
        "type": "assistant", "timestamp": ts,
        "message": {"content": [{"type": "tool_use", "id": id, "name": name, "input": input}]}
    })
    .to_string()
}

fn bash(ts: &str, cmd: &str) -> String {
    tool_use(ts, "Bash", json!({ "command": cmd }))
}

fn typed(ts: &str, text: &str) -> String {
    json!({"type": "user", "timestamp": ts, "message": {"role": "user", "content": text}})
        .to_string()
}

const CLAUDE_MD: &str = "# Language\n\n- `zzlang` is not in the section\n\n\
# CLI tools\n\n\
- Search: `zzgrep` (over grep), `zzjson` (JSON)\n\
- Files: `zzyaml` in a pipe; `zzghost` when it is there\n\
- Heavy jobs (`zzheavy run`, tests), when asked: `zzsrv start NAME`; then `zzsrv logs`, `ls`.\n  \
  Use it instead of `zzkill` loops.\n\n\
# git\n\n- `zzgit` is in another section\n";

/// Installed: MCP servers `alpha-api` (global), `beta` (one project), `claude.ai Fake Drive`
/// (a claude.ai connector), `plugin:kit:vault` (a plugin); skills `draw`, `plan-it` and
/// `kit:kit-env`; programs zzgrep, zzjson, zzyaml, zzsrv (and zzkill, zzheavy, zzlang, zzgit, ls,
/// which the section does not offer). A plugin turned off (`old`) is left out.
fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(dir.path()).unwrap();
    let bin = home.join("bin");
    for p in [
        "zzgrep", "zzjson", "zzyaml", "zzsrv", "zzkill", "zzheavy", "zzlang", "zzgit", "ls",
    ] {
        program(&bin, p);
    }
    let kit = home.join(".claude/plugins/cache/mk/kit/1.0");
    let old = home.join(".claude/plugins/cache/mk/old/1.0");
    write(
        &home.join(".claude.json"),
        &json!({
            "mcpServers": {"alpha-api": {"command": "x"}},
            "projects": {
                "/Users/t/work/x": {"mcpServers": {"beta": {}}},
                "/Users/t/work/y": {"mcpServers": {}},
            },
            "claudeAiMcpEverConnected": ["claude.ai Fake Drive"],
        })
        .to_string(),
    );
    write(
        &home.join(".claude/settings.json"),
        &json!({"enabledPlugins": {"kit@mk": true, "old@mk": false}}).to_string(),
    );
    write(
        &home.join(".claude/plugins/installed_plugins.json"),
        &json!({"version": 2, "plugins": {
            "kit@mk": [{"scope": "user", "installPath": kit}],
            "old@mk": [{"scope": "user", "installPath": old}],
        }})
        .to_string(),
    );
    write(&kit.join(".mcp.json"), r#"{"mcpServers": {"vault": {}}}"#);
    write(
        &kit.join("skills/kit-env/SKILL.md"),
        "---\nname: kit-env\ndescription: x\n---\n",
    );
    write(
        &old.join("skills/stale/SKILL.md"),
        "---\nname: stale\n---\n",
    );
    write(&old.join(".mcp.json"), r#"{"mcpServers": {"gone": {}}}"#);
    let skills = home.join(".claude/skills");
    write(
        &skills.join("draw-folder/SKILL.md"),
        "---\nname: draw\ndescription: x\n---\n",
    );
    write(&skills.join("plan-it/SKILL.md"), "no frontmatter\n");
    write(&skills.join("notes/README.md"), "not a skill\n");
    write(
        &skills.join(".trash/old/SKILL.md"),
        "---\nname: trashed\n---\n",
    );
    write(&home.join(".claude/CLAUDE.md"), CLAUDE_MD);

    let project = home.join(".claude/projects/-Users-t-work-x");
    let lines = [
        tool_use("2026-10-02T10:00:00Z", "mcp__alpha-api__search", json!({})),
        tool_use("2026-10-03T10:00:00Z", "mcp__alpha-api__fetch", json!({})),
        tool_use(
            "2026-10-03T11:00:00Z",
            "mcp__claude_ai_Fake_Drive__read",
            json!({}),
        ),
        tool_use("2026-10-04T10:00:00Z", "Skill", json!({"skill": "draw"})),
        tool_use(
            "2026-10-04T10:30:00Z",
            "Skill",
            json!({"skill": "draw-folder"}),
        ),
        typed(
            "2026-10-04T11:00:00Z",
            "<command-name>/plan-it</command-name>\n<command-args></command-args>",
        ),
        bash(
            "2026-10-05T10:00:00Z",
            "zzgrep foo | zzjson . && zzgrep bar",
        ),
        bash("2026-10-06T10:00:00Z", "cd x && zzsrv start web"),
        bash("2026-10-08T10:00:00Z", "timeout 5 zzgrep -l x"),
        bash(
            "2026-10-06T11:00:00Z",
            "rpatch <<'EOF'\nzzyaml and zzkill are only in the body\nEOF",
        ),
        bash("2026-10-06T12:00:00Z", "echo 'zzyaml' \"zzyaml\" ./zzyaml"),
        // Before the window: not counted, though the file is new.
        bash("2026-09-01T10:00:00Z", "zzyaml . a.yaml"),
        tool_use(
            "2026-09-01T10:00:00Z",
            "mcp__plugin_kit_vault__get",
            json!({}),
        ),
        tool_use(
            "2026-09-01T10:00:00Z",
            "Skill",
            json!({"skill": "kit:kit-env"}),
        ),
    ];
    write(&project.join("s1.jsonl"), &(lines.join("\n") + "\n"));
    write(
        &project.join("s1/subagents/agent-a.jsonl"),
        &(tool_use("2026-10-07T10:00:00Z", "mcp__beta__x", json!({})) + "\n"),
    );
    World {
        _dir: dir,
        home,
        bin,
    }
}

const WINDOW: [&str; 4] = ["--since", "2026-10-01", "--until", "2026-10-10"];

fn agentlog(w: &World, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
        .args(args)
        .args(["--root", w.home.join(".claude/projects").to_str().unwrap()])
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env("HOME", &w.home)
        .env("PATH", &w.bin)
        .env("TZ", "UTC")
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn report(w: &World) -> Value {
    let mut a = vec!["unused", "--json"];
    a.extend(WINDOW);
    let (code, out, err) = agentlog(w, &a);
    assert_eq!(code, 0, "{err}");
    serde_json::from_str(&out).unwrap()
}

/// (kind, name, uses, last) in the order printed.
fn rows(v: &Value) -> Vec<(String, String, u64, Option<String>)> {
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["kind"].as_str().unwrap().to_string(),
                i["name"].as_str().unwrap().to_string(),
                i["uses"].as_u64().unwrap(),
                i["last"].as_str().map(String::from),
            )
        })
        .collect()
}

fn uses(v: &Value, kind: &str, name: &str) -> u64 {
    rows(v)
        .into_iter()
        .find(|r| r.0 == kind && r.1 == name)
        .unwrap_or_else(|| panic!("no {kind} `{name}` in {v:#}"))
        .2
}

#[test]
fn every_installed_thing_is_listed_with_its_uses_the_unused_first() {
    let v = report(&world());
    let r = rows(&v);
    let names: Vec<(&str, &str)> = r.iter().map(|x| (x.0.as_str(), x.1.as_str())).collect();
    assert_eq!(
        names,
        [
            ("mcp", "plugin:kit:vault"),
            ("skill", "kit:kit-env"),
            ("cli", "zzyaml"),
            ("mcp", "beta"),
            ("mcp", "claude.ai Fake Drive"),
            ("skill", "plan-it"),
            ("cli", "zzjson"),
            ("cli", "zzsrv"),
            ("mcp", "alpha-api"),
            ("skill", "draw"),
            ("cli", "zzgrep"),
        ],
        "{v:#}"
    );
    let last: Vec<Option<&str>> = r.iter().map(|x| x.3.as_deref()).collect();
    assert_eq!(last[..3], [None, None, None]);
    let alpha = r.iter().find(|x| x.1 == "alpha-api").unwrap();
    assert_eq!(alpha.3.as_deref(), Some("2026-10-03"));
}

#[test]
fn mcp_servers_are_counted_by_their_tools_under_every_spelling() {
    let v = report(&world());
    // a global server, a claude.ai connector (spaces and dots become `_`), one project's
    // server used in a subagent, a plugin's server used only before the window
    assert_eq!(uses(&v, "mcp", "alpha-api"), 2);
    assert_eq!(uses(&v, "mcp", "claude.ai Fake Drive"), 1);
    assert_eq!(uses(&v, "mcp", "beta"), 1);
    assert_eq!(uses(&v, "mcp", "plugin:kit:vault"), 0);
    let from = |name: &str| -> Value {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["name"] == name)
            .unwrap()["from"]
            .clone()
    };
    assert_eq!(
        from("beta"),
        json!(["~/.claude.json project /Users/t/work/x"])
    );
    assert_eq!(from("plugin:kit:vault"), json!(["plugin kit@mk"]));
}

#[test]
fn skills_are_counted_by_skill_calls_and_by_what_the_user_typed() {
    let v = report(&world());
    // by its frontmatter name, and by its folder's (draw-folder)
    assert_eq!(uses(&v, "skill", "draw"), 2);
    // no frontmatter: the folder's name; run by the user as /plan-it
    assert_eq!(uses(&v, "skill", "plan-it"), 1);
    assert_eq!(uses(&v, "skill", "kit:kit-env"), 0);
    let all = rows(&v);
    for gone in ["stale", "old:stale", "trashed", "notes", "plugin:old:gone"] {
        assert!(
            !all.iter().any(|r| r.1 == gone),
            "`{gone}` is not installed"
        );
    }
}

#[test]
fn a_cli_is_a_word_of_a_bash_command_but_not_in_a_heredoc_or_in_quotes() {
    let v = report(&world());
    // once per call, though named twice in one
    assert_eq!(uses(&v, "cli", "zzgrep"), 2);
    assert_eq!(uses(&v, "cli", "zzjson"), 1);
    assert_eq!(uses(&v, "cli", "zzsrv"), 1);
    assert_eq!(uses(&v, "cli", "zzyaml"), 0);
}

#[test]
fn the_cli_tools_section_names_only_the_commands_it_offers() {
    let v = report(&world());
    let clis: Vec<String> = rows(&v)
        .into_iter()
        .filter(|r| r.0 == "cli")
        .map(|r| r.1)
        .collect();
    let mut sorted = clis.clone();
    sorted.sort();
    // not zzlang or zzgit (other sections), zzheavy (an example in a label), zzkill (what not to
    // use), ls (a subcommand), zzghost (not on PATH)
    assert_eq!(sorted, ["zzgrep", "zzjson", "zzsrv", "zzyaml"]);
    let cli_md = v["read"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["place"] == "~/.claude/CLAUDE.md")
        .unwrap();
    assert!(
        cli_md["found"]
            .as_str()
            .unwrap()
            .contains("left out: zzghost"),
        "{cli_md}"
    );
}

#[test]
fn the_text_says_where_it_read_and_what_it_could_not() {
    let w = world();
    fs::remove_file(w.home.join(".claude/settings.json")).unwrap();
    write(
        &w.home.join(".claude/plugins/installed_plugins.json"),
        "{oops",
    );
    let mut a = vec!["unused"];
    a.extend(WINDOW);
    let (code, out, err) = agentlog(&w, &a);
    assert_eq!(code, 0, "{err}");
    let read = out.split("not read (skipped):").next().unwrap();
    for place in ["~/.claude.json", "~/.claude/skills", "~/.claude/CLAUDE.md"] {
        assert!(read.contains(place), "`{place}` not under read:\n{out}");
    }
    let skipped = out.split("not read (skipped):").nth(1).expect(&out);
    assert!(
        skipped.contains("~/.claude/settings.json") && skipped.contains("not JSON"),
        "{out}"
    );
    // the header, then the table, unused first
    assert!(
        out.starts_with("2026-10-01 00:00 .. 2026-10-10 00:00 (local): "),
        "{out}"
    );
    let table: Vec<&str> = out.lines().skip_while(|l| !l.starts_with("kind")).collect();
    // the plugins could not be read, so the first unused is the command
    assert!(table[1].starts_with("cli    zzyaml"), "{out}");
    assert!(table.last().unwrap().starts_with("cli    zzgrep"), "{out}");
}

#[test]
fn nothing_installed_still_reports_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let w = World {
        home: dir.path().to_path_buf(),
        bin: dir.path().join("bin"),
        _dir: dir,
    };
    fs::create_dir_all(w.home.join(".claude/projects")).unwrap();
    let v = report(&w);
    assert_eq!(v["items"], json!([]));
    let errors = v["read"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p.get("error").is_some())
        .count();
    assert_eq!(errors, 5, "{v:#}");
}

#[test]
fn unreadable_logs_and_flags_of_other_commands_are_refused() {
    let w = world();
    let (code, _, err) = Command::new(env!("CARGO_BIN_EXE_agentlog"))
        .args(["unused", "--root", "/nonexistent-root"])
        .env("HOME", &w.home)
        .output()
        .map(|o| {
            (
                o.status.code().unwrap(),
                String::new(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            )
        })
        .unwrap();
    assert_eq!(code, 3, "{err}");
    for args in [
        &["unused", "x"][..],
        &["unused", "--source", "codex"],
        &["unused", "--raw"],
        &["unused", "--repo", "."],
    ] {
        let (code, _, err) = agentlog(&w, args);
        assert_eq!(code, 2, "{args:?}: {err}");
    }
}

#[test]
fn a_copied_history_counts_once_and_a_cut_line_does_not_hide_a_file() {
    let w = world();
    let before = rows(&report(&w));
    let project = w.home.join(".claude/projects/-Users-t-work-x");
    // a session that starts with a copy of s1 (the same ids, times and uuids), then goes on
    let mut typed_line: Value = serde_json::from_str(&typed(
        "2026-10-04T12:00:00Z",
        "<command-name>/draw</command-name>",
    ))
    .unwrap();
    typed_line["uuid"] = json!("u-1");
    fs::write(
        project.join("s1.jsonl"),
        fs::read_to_string(project.join("s1.jsonl")).unwrap() + &format!("{typed_line}\n"),
    )
    .unwrap();
    let copy = fs::read_to_string(project.join("s1.jsonl")).unwrap();
    let cut = bash("2026-10-05T12:00:00Z", "zzjson CUT").replace("CUT", "\\ud83d");
    let mut s2 = format!("{copy}{cut}\n").into_bytes();
    s2.extend_from_slice(
        b"{\"type\":\"assistant\",\"timestamp\":\"2026-10-05T12:01:00Z\",\"x\":\"\xe3\x81",
    );
    fs::write(project.join("s2.jsonl"), s2).unwrap();
    let after = report(&w);
    for (kind, name, n, _) in before {
        let more = match (kind.as_str(), name.as_str()) {
            ("cli", "zzjson") | ("skill", "draw") => 1,
            _ => 0,
        };
        assert_eq!(uses(&after, &kind, &name), n + more, "{kind} {name}");
    }
}

#[test]
fn project_keeps_the_sessions_of_that_project() {
    let w = world();
    let mut a = vec!["unused", "--json", "--project", "work-x"];
    a.extend(WINDOW);
    let (code, out, err) = agentlog(&w, &a);
    assert_eq!(code, 0, "{err}");
    let kept: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(rows(&kept), rows(&report(&w)));
    let mut a = vec!["unused", "--json", "--project", "elsewhere"];
    a.extend(WINDOW);
    let (_, out, _) = agentlog(&w, &a);
    let none: Value = serde_json::from_str(&out).unwrap();
    assert!(rows(&none).iter().all(|r| r.2 == 0), "{none:#}");
}
