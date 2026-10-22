//! `memory` and `rules` on memory files written here, with made-up words. HOME is a folder
//! made here, so the memories of the real machine are never read.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

/// A home folder with Claude Code's projects `net` (holding the memory), `net/alpha` (its
/// `memory` is a link to net's) and `beta`, a repository `work/repo` with its own memory,
/// and Codex's memories and AGENTS.md.
struct World {
    _dir: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A session log of one line, which is where a project's name is read from.
fn session(projects: &Path, folder: &str, cwd: &str) {
    let line = json!({"type": "user", "cwd": cwd, "sessionId": "s", "message": {"content": "hi"}});
    write(&projects.join(folder).join("s.jsonl"), &format!("{line}\n"));
}

fn folder_of(dir: &str) -> String {
    dir.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(dir.path()).unwrap();
    let projects = home.join(".claude/projects");

    session(
        &projects,
        "-Users-t-project-prod-net",
        "/Users/t/project/prod/net",
    );
    session(
        &projects,
        "-Users-t-project-prod-net-alpha",
        "/Users/t/project/prod/net/alpha",
    );
    session(
        &projects,
        "-Users-t-project-prod-beta",
        "/Users/t/project/prod/beta",
    );
    let net = projects.join("-Users-t-project-prod-net/memory");
    write(
        &net.join("MEMORY.md"),
        "- [A](feedback-a.md) — INDEXWORD は使わない\n",
    );
    write(
        &net.join("feedback-a.md"),
        "---\nname: feedback-a\ndescription: DESCWORD を守る\nmetadata:\n  node_type: memory\n  type: feedback\n  originSessionId: FRONTONLY しない\n---\n\nZEBRA を探す。\nふつうの文。\nこれはしない。\n```sh\nCODEWORD はしない\n```\nもう一つ使わない。\n",
    );
    write(
        &net.join("project-b.md"),
        "---\nname: project-b\ndescription: old form\ntype: project\n---\nZEBRA again\nNever hand-edit it.\n",
    );
    symlink(
        "../-Users-t-project-prod-net/memory",
        projects.join("-Users-t-project-prod-net-alpha/memory"),
    )
    .unwrap();
    write(
        &projects.join("-Users-t-project-prod-beta/memory/user-c.md"),
        "---\nname: user-c\ndescription: c\nmetadata:\n  type: user\n---\nZEBRA is here. Do not touch.\n",
    );

    // a repository: its AGENTS.md, CLAUDE.md as a link to it, and its own memory
    let repo = home.join("work/repo");
    write(
        &repo.join("AGENTS.md"),
        "# Rules\n- 勝手に消しません\n- fine\n",
    );
    symlink("AGENTS.md", repo.join("CLAUDE.md")).unwrap();
    session(
        &projects,
        &folder_of(repo.to_str().unwrap()),
        repo.to_str().unwrap(),
    );
    write(
        &projects
            .join(folder_of(repo.to_str().unwrap()))
            .join("memory/repo-only.md"),
        "---\nname: repo-only\ndescription: d\nmetadata:\n  type: feedback\n---\nZEBRA ここだけ。使わない。\n",
    );
    write(
        &home.join(".claude/CLAUDE.md"),
        "- 日本語以外では返答しない\n- 説明\n",
    );

    write(
        &home.join(".codex/memories/raw_memories.md"),
        "ZEBRA codex\n- avoid this\n",
    );
    write(&home.join(".codex/AGENTS.md"), "- Never push\n");
    World {
        _dir: dir,
        home,
        repo,
    }
}

fn agentlog(w: &World, args: &[&str]) -> (i32, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_agentlog"))
        .args(args)
        .args(["--root", w.home.join(".claude/projects").to_str().unwrap()])
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("CODEX_HOME")
        .env("HOME", &w.home)
        .output()
        .unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

fn objects(w: &World, args: &[&str]) -> Vec<Value> {
    let mut a = args.to_vec();
    a.push("--json");
    let (code, out, err) = agentlog(w, &a);
    assert_eq!(code, 0, "{err}");
    out.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn files(v: &[Value]) -> Vec<String> {
    let mut f: Vec<String> = v
        .iter()
        .map(|o| {
            format!(
                "{}/{}",
                o["project"].as_str().unwrap(),
                o["file"].as_str().unwrap()
            )
        })
        .collect();
    f.sort();
    f
}

#[test]
fn a_file_seen_through_a_link_is_one_hit_shown_under_the_folder_it_lives_in() {
    let w = world();
    let v = objects(&w, &["memory", "ZEBRA"]);
    let got = files(&v);
    assert_eq!(got.len(), 4, "{got:?}");
    assert_eq!(
        &got[..3],
        ["beta/user-c.md", "net/feedback-a.md", "net/project-b.md"]
    );
    assert!(got[3].ends_with("/repo-only.md"), "{got:?}");
    let a = v.iter().find(|o| o["file"] == "feedback-a.md").unwrap();
    assert_eq!(a["also_in"], json!(["net/alpha"]));
    assert_eq!(
        a["matches"],
        json!([{"line": 10, "text": "ZEBRA を探す。"}])
    );
    assert_eq!(a["type"], "feedback");
    assert_eq!(a["description"], "DESCWORD を守る");
    // by the name of a project the link is seen from
    let v = objects(&w, &["memory", "ZEBRA", "--project", "alpha"]);
    assert_eq!(files(&v), ["net/feedback-a.md", "net/project-b.md"]);
    let v = objects(
        &w,
        &["memory", "ZEBRA", "--exclude", "beta", "--exclude", "alpha"],
    );
    assert_eq!(v.len(), 1);
}

#[test]
fn the_index_is_left_out_unless_asked_for() {
    let w = world();
    assert!(objects(&w, &["memory", "INDEXWORD"]).is_empty());
    let v = objects(&w, &["memory", "INDEXWORD", "--index"]);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0]["file"], "MEMORY.md");
}

#[test]
fn name_and_description_are_searched_but_the_rest_of_the_frontmatter_is_not() {
    let w = world();
    let v = objects(&w, &["memory", "DESCWORD"]);
    assert_eq!(v[0]["matches"][0]["line"], 3);
    assert!(objects(&w, &["memory", "FRONTONLY"]).is_empty());
    let v = objects(&w, &["memory", "FEEDBACK-A", "-i"]);
    assert_eq!(v[0]["matches"][0]["line"], 2);
    // code is searched: only `rules` leaves it out
    assert_eq!(objects(&w, &["memory", "CODEWORD"]).len(), 1);
}

#[test]
fn type_narrows_and_codex_memories_have_none() {
    let w = world();
    let v = objects(&w, &["memory", "ZEBRA", "--type", "feedback"]);
    assert!(v.iter().all(|o| o["type"] == "feedback"));
    assert_eq!(v.len(), 2);
    let all = objects(&w, &["memory", "ZEBRA", "--source", "all"]);
    assert_eq!(all.len(), 5);
    assert!(all.iter().any(|o| o["source"] == "codex"));
    let typed = objects(
        &w,
        &["memory", "ZEBRA", "--source", "all", "--type", "feedback"],
    );
    assert!(typed.iter().all(|o| o["source"] == "claude"));
    // old files keep the type at the top
    let v = objects(&w, &["memory", "ZEBRA", "--type", "project"]);
    assert_eq!(v[0]["file"], "project-b.md");
    let only = objects(&w, &["memory", "ZEBRA", "--source", "codex"]);
    assert_eq!(files(&only), ["codex/raw_memories.md"]);
}

#[test]
fn text_output_lists_the_file_then_its_lines_and_ends_with_a_count() {
    let w = world();
    let (code, out, _) = agentlog(&w, &["memory", "DESCWORD"]);
    assert_eq!(code, 0);
    assert!(
        out.contains(
            "net  feedback-a.md  feedback  DESCWORD を守る\n  3: description: DESCWORD を守る\n"
        ),
        "{out}"
    );
    assert!(out.ends_with("1 lines in 1 files\n"), "{out}");
}

fn rule_lines(v: &[Value]) -> Vec<(String, u64)> {
    let mut r: Vec<(String, u64)> = v
        .iter()
        .map(|o| {
            (
                o["file"].as_str().unwrap().to_string(),
                o["line"].as_u64().unwrap(),
            )
        })
        .collect();
    r.sort();
    r
}

#[test]
fn rules_do_not_count_the_frontmatter_the_code_or_the_index() {
    let w = world();
    let v = objects(&w, &["rules"]);
    let got = rule_lines(&v);
    // feedback-a: the sentences at 12 and 16; the fenced 14 and the frontmatter 7 are not
    assert!(got.contains(&("net/feedback-a.md".into(), 12)), "{got:?}");
    assert!(got.contains(&("net/feedback-a.md".into(), 16)), "{got:?}");
    assert!(
        !got.iter()
            .any(|(f, n)| f == "net/feedback-a.md" && (*n == 7 || *n == 14)),
        "{got:?}"
    );
    assert!(
        !got.iter().any(|(f, _)| f.ends_with("MEMORY.md")),
        "{got:?}"
    );
    assert!(got.iter().any(|(f, _)| f == "net/project-b.md"));
    // the linked memory is read once, and no instruction file or Codex file is without being asked
    assert_eq!(
        got.iter().filter(|(f, _)| f == "net/feedback-a.md").count(),
        2
    );
    assert!(
        got.iter()
            .all(|(f, _)| f.starts_with("net/") || f.starts_with("beta/") || f.contains("repo")),
        "{got:?}"
    );
    assert!(
        !got.iter()
            .any(|(f, _)| f.contains("codex") || f.contains("AGENTS"))
    );
}

#[test]
fn rules_count_prints_the_table_and_the_total_only() {
    let w = world();
    let (code, out, _) = agentlog(&w, &["rules", "--count"]);
    assert_eq!(code, 0);
    assert!(out.starts_with("count  place         file\n"), "{out}");
    let first = out.lines().nth(1).unwrap();
    assert!(
        first
            .trim_start()
            .starts_with("2  memory        net/feedback-a.md"),
        "{out}"
    );
    assert!(
        out.contains("total (4 files with any, 4 files read)"),
        "{out}"
    );
    assert!(!out.contains("net/feedback-a.md:"), "{out}");
    // the same count as JSON ends with the total
    let v = objects(&w, &["rules", "--count"]);
    assert_eq!(v.last().unwrap()["total"], 5);
}

#[test]
fn rules_for_a_repository_are_what_a_session_started_there_reads() {
    let w = world();
    let repo = w.repo.to_str().unwrap();
    let v = objects(&w, &["rules", "--repo", repo]);
    let got: Vec<String> = rule_lines(&v).into_iter().map(|(f, _)| f).collect();
    // the global CLAUDE.md, the repository's AGENTS.md (CLAUDE.md is the same file), its memory
    assert!(
        got.iter()
            .any(|f| f.ends_with("/.claude/CLAUDE.md") || f == "~/.claude/CLAUDE.md"),
        "{got:?}"
    );
    assert_eq!(
        got.iter()
            .filter(|f| f.ends_with("work/repo/AGENTS.md"))
            .count(),
        1,
        "{got:?}"
    );
    assert!(
        !got.iter().any(|f| f.ends_with("work/repo/CLAUDE.md")),
        "{got:?}"
    );
    assert!(got.iter().any(|f| f.ends_with("repo-only.md")), "{got:?}");
    assert!(
        !got.iter()
            .any(|f| f.starts_with("net/") || f.starts_with("beta/")),
        "{got:?}"
    );
    // Codex's are added by --source
    let v = objects(&w, &["rules", "--repo", repo, "--source", "all"]);
    let got: Vec<String> = rule_lines(&v).into_iter().map(|(f, _)| f).collect();
    assert!(
        got.iter().any(|f| f.ends_with(".codex/AGENTS.md")),
        "{got:?}"
    );
    assert!(got.iter().any(|f| f.starts_with("codex/")), "{got:?}");
    // a directory that is not there
    let (code, _, err) = agentlog(&w, &["rules", "--repo", "/nonexistent/dir"]);
    assert_eq!(code, 2, "{err}");
}

#[test]
fn the_pattern_can_be_replaced() {
    let w = world();
    let v = objects(&w, &["rules", "--pattern", "ZEBRA"]);
    assert_eq!(v.len(), 4);
    let (code, _, err) = agentlog(&w, &["rules", "--pattern", "("]);
    assert_eq!(code, 2, "{err}");
}

#[test]
fn flags_that_belong_to_another_command_are_refused() {
    let w = world();
    for args in [
        &["rules", "--type", "user"][..],
        &["memory", "x", "--count"],
        &["memory", "x", "--pattern", "y"],
        &["repeats", "--index"],
        &["memory", "x", "--repo", "."],
    ] {
        let (code, _, err) = agentlog(&w, args);
        assert_eq!(code, 2, "{args:?}: {err}");
    }
    let (code, _, err) = agentlog(
        &w,
        &[
            "memory",
            "x",
            "--source",
            "codex",
            "--codex-memories",
            "/nonexistent",
        ],
    );
    assert_eq!(code, 3, "{err}");
}

fn places(v: &[Value]) -> Vec<(String, String)> {
    let mut p: Vec<(String, String)> = v
        .iter()
        .map(|o| {
            (
                o["file"].as_str().unwrap().to_string(),
                o["place"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    p.sort();
    p.dedup();
    p
}

#[test]
fn every_rule_line_says_where_its_file_lives() {
    let w = world();
    let repo = w.repo.to_str().unwrap();
    let v = objects(&w, &["rules", "--repo", repo, "--source", "all"]);
    let got = places(&v);
    let place_of = |end: &str| -> String {
        let hits: Vec<&(String, String)> = got.iter().filter(|(f, _)| f.ends_with(end)).collect();
        assert_eq!(hits.len(), 1, "{end}: {got:?}");
        hits[0].1.clone()
    };
    assert_eq!(place_of(".claude/CLAUDE.md"), "global");
    assert_eq!(place_of(".codex/AGENTS.md"), "global");
    assert_eq!(place_of("work/repo/AGENTS.md"), "repo");
    assert_eq!(place_of("repo-only.md"), "memory");
    assert_eq!(place_of("raw_memories.md"), "codex-memory");
    // the table and the printed lines show it too
    let (_, out, _) = agentlog(&w, &["rules", "--repo", repo, "--source", "all", "--count"]);
    assert!(out.starts_with("count  place         file\n"), "{out}");
    assert!(
        out.lines()
            .any(|l| l.contains("codex-memory  codex/raw_memories.md")),
        "{out}"
    );
    let (_, out, _) = agentlog(&w, &["rules", "--repo", repo, "--source", "all"]);
    assert!(out.contains("[global]  - Never push"), "{out}");
}

fn also_in(v: &[Value], file: &str) -> Vec<String> {
    let hits: Vec<&Value> = v
        .iter()
        .filter(|o| o["file"].as_str().unwrap().ends_with(file))
        .collect();
    assert!(!hits.is_empty(), "{file}: {v:?}");
    let first = hits[0]["also_in"].as_array().unwrap();
    for h in &hits {
        assert_eq!(h["also_in"].as_array().unwrap(), first);
    }
    first
        .iter()
        .map(|p| p.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_memory_of_the_same_name_in_another_project_is_marked_but_a_link_to_one_file_is_not() {
    let w = world();
    let projects = w.home.join(".claude/projects");
    // beta holds a file of the same name as net's feedback-a.md
    write(
        &projects.join("-Users-t-project-prod-beta/memory/feedback-a.md"),
        "---\nname: feedback-a\ndescription: b's own\nmetadata:\n  type: feedback\n---\nこれはしない。\n",
    );
    // gamma only links to net's project-b.md: one file, so no mark
    session(
        &projects,
        "-Users-t-project-prod-gamma",
        "/Users/t/project/prod/gamma",
    );
    let gamma = projects.join("-Users-t-project-prod-gamma/memory");
    fs::create_dir_all(&gamma).unwrap();
    symlink(
        projects.join("-Users-t-project-prod-net/memory/project-b.md"),
        gamma.join("project-b.md"),
    )
    .unwrap();

    let v = objects(&w, &["rules"]);
    assert_eq!(also_in(&v, "net/feedback-a.md"), ["beta"]);
    assert_eq!(also_in(&v, "beta/feedback-a.md"), ["net"]);
    assert!(also_in(&v, "net/project-b.md").is_empty());
    assert!(also_in(&v, "beta/user-c.md").is_empty());
    assert!(
        v.iter()
            .all(|o| !o["file"].as_str().unwrap().contains("gamma")),
        "{v:?}"
    );
    // the text shows the mark on the table and on the line
    let (_, out, _) = agentlog(&w, &["rules", "--count"]);
    assert!(
        out.lines()
            .any(|l| l.contains("net/feedback-a.md  (also in: beta)")),
        "{out}"
    );
    let (_, out, _) = agentlog(&w, &["rules"]);
    assert!(out.contains("[memory; also in: net]  "), "{out}");

    // narrowing to a repository still looks at every project for the other copies
    let repo = w.repo.to_str().unwrap();
    write(
        &projects.join("-Users-t-project-prod-beta/memory/repo-only.md"),
        "---\nname: repo-only\ndescription: d\nmetadata:\n  type: feedback\n---\n使わない。\n",
    );
    let v = objects(&w, &["rules", "--repo", repo]);
    assert_eq!(also_in(&v, "repo-only.md"), ["beta"]);
    // and --project narrows what is read but not what is compared with
    let v = objects(&w, &["rules", "--project", "net"]);
    assert_eq!(also_in(&v, "net/feedback-a.md"), ["beta"]);
}
