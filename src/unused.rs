//! `unused`: what is installed (MCP servers, skills, the CLIs `~/.claude/CLAUDE.md` points to)
//! beside how often the logs used it, the least used first. It lists; whether to remove one is
//! for a person to judge.
//!
//! The settings are spread over several files, so every place read is reported, and a place
//! that cannot be read is skipped and named.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::load::{self, Filter};
use crate::{jsonl, lines, places, time};

pub struct UnusedOpts {
    pub home: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Kind {
    Mcp,
    Skill,
    Cli,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Mcp => "mcp",
            Kind::Skill => "skill",
            Kind::Cli => "cli",
        }
    }
}

struct Item {
    kind: Kind,
    name: String,
    /// The names the logs call it by: an MCP server's part of `mcp__SERVER__tool`, a skill's
    /// name (and its folder's), a command's name.
    keys: Vec<String>,
    /// Where it is installed, as printed.
    from: Vec<String>,
}

/// One place read: what was found there, or why it was not read.
struct Place {
    path: String,
    found: Result<String, String>,
}

#[derive(Default)]
struct Installed {
    items: Vec<Item>,
    places: Vec<Place>,
}

impl Installed {
    fn add(&mut self, kind: Kind, name: &str, keys: Vec<String>, from: &str) {
        if let Some(it) = self
            .items
            .iter_mut()
            .find(|it| it.kind == kind && it.name == name)
        {
            if !it.from.iter().any(|f| f == from) {
                it.from.push(from.to_string());
            }
            return;
        }
        self.items.push(Item {
            kind,
            name: name.to_string(),
            keys,
            from: vec![from.to_string()],
        });
    }
}

/// Claude Code names an MCP server's tools `mcp__SERVER__tool`, SERVER spelled with every
/// character but letters, digits, `_` and `-` turned into `_` (`claude.ai Google Drive` →
/// `claude_ai_Google_Drive`).
fn tool_name(server: &str) -> String {
    server
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn tilde(p: &Path, home: &Path) -> String {
    match p.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

fn read_json(p: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(p).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| format!("not JSON: {e}"))
}

fn keys_of(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// `~/.claude.json`: the servers for every project, those of one project, and the claude.ai
/// connectors this machine has connected to.
fn claude_json(home: &Path, inst: &mut Installed) {
    let p = places::claude_json(home);
    let path = tilde(&p, home);
    let v = match read_json(&p) {
        Ok(v) => v,
        Err(e) => {
            inst.places.push(Place {
                path,
                found: Err(e),
            });
            return;
        }
    };
    let global = keys_of(v.get("mcpServers"));
    for s in &global {
        inst.add(Kind::Mcp, s, vec![tool_name(s)], &path);
    }
    let mut in_projects = 0;
    let mut projects = 0;
    if let Some(ps) = v.get("projects").and_then(Value::as_object) {
        for (dir, pv) in ps {
            let servers = keys_of(pv.get("mcpServers"));
            if servers.is_empty() {
                continue;
            }
            projects += 1;
            let from = format!("{path} project {}", tilde(Path::new(dir), home));
            for s in &servers {
                in_projects += 1;
                inst.add(Kind::Mcp, s, vec![tool_name(s)], &from);
            }
        }
    }
    let connectors: Vec<&str> = v
        .get("claudeAiMcpEverConnected")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    for s in &connectors {
        inst.add(Kind::Mcp, s, vec![tool_name(s)], "claude.ai");
    }
    inst.places.push(Place {
        path,
        found: Ok(format!(
            "mcp: {} global, {in_projects} in {}, {} (claudeAiMcpEverConnected)",
            global.len(),
            plural(projects, "project"),
            plural(connectors.len(), "claude.ai connector"),
        )),
    });
}

/// `~/.claude/settings.json`: the plugins turned off there are left out.
fn settings(home: &Path, inst: &mut Installed) -> BTreeSet<String> {
    let p = places::claude_dir(home).join("settings.json");
    let path = tilde(&p, home);
    let v = match read_json(&p) {
        Ok(v) => v,
        Err(e) => {
            inst.places.push(Place {
                path,
                found: Err(e),
            });
            return BTreeSet::new();
        }
    };
    let mut on = Vec::new();
    let mut off = BTreeSet::new();
    if let Some(m) = v.get("enabledPlugins").and_then(Value::as_object) {
        for (k, x) in m {
            if x.as_bool() == Some(false) {
                off.insert(k.clone());
            } else {
                on.push(k.as_str());
            }
        }
    }
    let mut found = format!(
        "enabledPlugins: {}",
        if on.is_empty() {
            "none".to_string()
        } else {
            on.join(", ")
        }
    );
    if !off.is_empty() {
        let off: Vec<&str> = off.iter().map(String::as_str).collect();
        let _ = write!(found, "; off, left out: {}", off.join(", "));
    }
    inst.places.push(Place {
        path,
        found: Ok(found),
    });
    off
}

/// The `name:` of a SKILL.md's frontmatter.
fn skill_name(skill_md: &Path) -> Option<String> {
    let text = fs::read_to_string(skill_md).ok()?;
    let mut lines = text.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }
    for l in lines {
        if l.trim() == "---" {
            break;
        }
        if let Some(v) = l.strip_prefix("name:") {
            let v = v.trim().trim_matches(['"', '\'']);
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// The skills in `dir` (a folder each, holding SKILL.md), as (name, folder name).
fn skills_in(dir: &Path) -> io::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir)?.flatten() {
        let folder = e.file_name().to_string_lossy().into_owned();
        let md = e.path().join("SKILL.md");
        if folder.starts_with('.') || !md.is_file() {
            continue;
        }
        let name = skill_name(&md).unwrap_or_else(|| folder.clone());
        out.push((name, folder));
    }
    out.sort();
    Ok(out)
}

fn add_skill(inst: &mut Installed, name: String, folder: String, from: &str) {
    let mut keys = vec![name.clone()];
    if folder != name {
        keys.push(folder);
    }
    inst.add(Kind::Skill, &name, keys, from);
}

/// `~/.claude/plugins/installed_plugins.json`, and in each plugin's folder its MCP servers
/// (`.mcp.json`, or `mcpServers` in `.claude-plugin/plugin.json`) and skills.
fn plugins(home: &Path, off: &BTreeSet<String>, inst: &mut Installed) {
    let p = places::claude_dir(home).join("plugins/installed_plugins.json");
    let path = tilde(&p, home);
    let v = match read_json(&p) {
        Ok(v) => v,
        Err(e) => {
            inst.places.push(Place {
                path,
                found: Err(e),
            });
            return;
        }
    };
    let (mut n, mut servers, mut skills) = (0, 0, 0);
    let mut missing = Vec::new();
    for (id, entries) in v
        .get("plugins")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        if off.contains(id) {
            continue;
        }
        n += 1;
        let plugin = id.split('@').next().unwrap_or(id);
        let from = format!("plugin {id}");
        let dirs: BTreeSet<&str> = entries
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.get("installPath").and_then(Value::as_str))
            .collect();
        for dir in dirs {
            let dir = Path::new(dir);
            if !dir.is_dir() {
                missing.push(tilde(dir, home));
                continue;
            }
            let mut names = keys_of(
                read_json(&dir.join(".mcp.json"))
                    .ok()
                    .as_ref()
                    .and_then(|v| v.get("mcpServers")),
            );
            names.extend(keys_of(
                read_json(&dir.join(".claude-plugin/plugin.json"))
                    .ok()
                    .as_ref()
                    .and_then(|v| v.get("mcpServers")),
            ));
            for s in names {
                servers += 1;
                // A plugin's server is `mcp__plugin_PLUGIN_SERVER__tool`.
                let key = tool_name(&format!("plugin_{plugin}_{s}"));
                inst.add(Kind::Mcp, &format!("plugin:{plugin}:{s}"), vec![key], &from);
            }
            for (name, folder) in skills_in(&dir.join("skills")).unwrap_or_default() {
                skills += 1;
                add_skill(
                    inst,
                    format!("{plugin}:{name}"),
                    format!("{plugin}:{folder}"),
                    &from,
                );
            }
        }
    }
    let mut found = format!(
        "{}: {} mcp, {}",
        plural(n, "plugin"),
        servers,
        plural(skills, "skill")
    );
    if !missing.is_empty() {
        let _ = write!(found, "; install folder missing: {}", missing.join(", "));
    }
    inst.places.push(Place {
        path,
        found: Ok(found),
    });
}

fn user_skills(home: &Path, inst: &mut Installed) {
    let dir = places::claude_dir(home).join("skills");
    let path = tilde(&dir, home);
    match skills_in(&dir) {
        Ok(list) => {
            let n = list.len();
            for (name, folder) in list {
                add_skill(inst, name, folder, &path);
            }
            inst.places.push(Place {
                path,
                found: Ok(plural(n, "skill")),
            });
        }
        Err(e) => inst.places.push(Place {
            path,
            found: Err(e.to_string()),
        }),
    }
}

/// The text of the section headed `CLI tools` (any level), to the next heading of its level
/// or above. Lines in fenced code are not headings.
fn cli_section(text: &str) -> Option<String> {
    let mut level = None;
    let mut out = String::new();
    let mut fenced = false;
    for l in text.lines() {
        if l.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        let hashes = l.chars().take_while(|&c| c == '#').count();
        let heading = !fenced && hashes > 0 && l[hashes..].starts_with(' ');
        match level {
            None if heading && l[hashes..].trim().eq_ignore_ascii_case("CLI tools") => {
                level = Some(hashes)
            }
            Some(n) if heading && hashes <= n => break,
            Some(_) => {
                out.push_str(l);
                out.push('\n');
            }
            None => {}
        }
    }
    level.map(|_| out)
}

/// The commands a `CLI tools` section names: the first word of each `code span`, with four
/// kinds left out. A span in a bullet's label (before its first `: `) is an example of where
/// the tool is used. A span after "instead of" in the same sentence is what not to use. A
/// span that continues a list right after a span of several words (`devsrv logs`, `ls`,
/// `stop`) is a subcommand. A word that is not a name (`--json`, `~/x`, `<<<`) is no command.
fn named_commands(section: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // A bullet and the lines that continue it are one piece of text.
    let mut items: Vec<String> = Vec::new();
    for l in section.lines() {
        let t = l.trim_start();
        if t.starts_with("- ") || t.starts_with("* ") || items.is_empty() || t.is_empty() {
            items.push(String::new());
        }
        let last = items.last_mut().unwrap();
        last.push(' ');
        last.push_str(t);
    }
    for item in &items {
        let parts: Vec<&str> = item.split('`').collect();
        // The text part where the label ends (a colon outside brackets, in the first
        // sentence); spans before it are in the label. A colon after a sentence has ended
        // (`Chain it (`x && y`): …`) is in the text, not a label.
        let mut depth = 0i32;
        let mut label_end = 0;
        let mut ended = false;
        for (k, p) in parts.iter().enumerate().step_by(2) {
            let mut colon = false;
            for (j, c) in p.char_indices() {
                depth += match c {
                    '(' => 1,
                    ')' => -1,
                    _ => 0,
                };
                ended |= depth <= 0 && c == '.' && p[j + 1..].starts_with(' ');
                if ended {
                    break;
                }
                if c == ':' && depth <= 0 && p[j + 1..].chars().next().is_none_or(|n| n == ' ') {
                    colon = true;
                    break;
                }
            }
            if colon {
                label_end = k;
            }
            if colon || ended {
                break;
            }
        }
        // Odd parts are code spans, even parts the text between them.
        let mut negated = false;
        let mut prev_words = 0;
        let mut prev_option = false;
        let mut continuing = false;
        for (i, part) in parts.iter().enumerate() {
            if i % 2 == 0 {
                if part.contains(". ") {
                    negated = false;
                }
                if part.contains("instead of") {
                    negated = true;
                }
                continue;
            }
            if i + 1 == parts.len() {
                break; // an unclosed span
            }
            // `(or …)` goes on a list too; after an option (`--source codex` (or `all`)) what
            // follows is another value of it, not a command
            let gap = parts[i - 1].trim().trim_start_matches('(').trim();
            let list = i >= 3 && matches!(gap, "," | "and" | ", and" | "or" | ", or");
            continuing = list && (continuing || prev_words > 1 || prev_option);
            prev_words = part.split_whitespace().count();
            prev_option = part.starts_with('-');
            let Some(word) = part.split_whitespace().next() else {
                continue;
            };
            let is_name = word.starts_with(|c: char| c.is_ascii_alphanumeric())
                && word
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c));
            let in_label = i < label_end;
            if in_label || negated || continuing || !is_name || out.iter().any(|w| w == word) {
                continue;
            }
            out.push(word.to_string());
        }
    }
    out
}

/// A program of that name in a folder of `path` (PATH's value).
fn on_path(name: &str, path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(path).any(|d| {
        fs::metadata(d.join(name)).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    })
}

fn cli_tools(home: &Path, path_var: &str, inst: &mut Installed) {
    let p = places::claude_dir(home).join("CLAUDE.md");
    let path = tilde(&p, home);
    let section = match fs::read_to_string(&p) {
        Ok(t) => cli_section(&t),
        Err(e) => {
            inst.places.push(Place {
                path,
                found: Err(e.to_string()),
            });
            return;
        }
    };
    let Some(section) = section else {
        inst.places.push(Place {
            path,
            found: Err("no heading `CLI tools`".into()),
        });
        return;
    };
    let (found, missing): (Vec<String>, Vec<String>) = named_commands(&section)
        .into_iter()
        .partition(|w| on_path(w, path_var));
    for w in &found {
        inst.add(Kind::Cli, w, vec![w.clone()], &path);
    }
    let mut note = format!("CLI tools: {}", plural(found.len(), "command"));
    if !missing.is_empty() {
        let _ = write!(
            note,
            "; not a program on PATH, left out: {}",
            missing.join(", ")
        );
    }
    inst.places.push(Place {
        path,
        found: Ok(note),
    });
}

fn installed(home: &Path, path_var: &str) -> Installed {
    let mut inst = Installed::default();
    claude_json(home, &mut inst);
    let off = settings(home, &mut inst);
    plugins(home, &off, &mut inst);
    user_skills(home, &mut inst);
    cli_tools(home, path_var, &mut inst);
    inst
}

// ---------------------------------------------------------------------------------------
// the logs

/// Uses of each name, by kind: how many, and the last time.
#[derive(Default)]
struct Usage {
    calls: usize,
    used: HashMap<(Kind, String), (usize, i64)>,
}

impl Usage {
    fn hit(&mut self, kind: Kind, key: &str, ts: i64) {
        let e = self.used.entry((kind, key.to_string())).or_default();
        e.0 += 1;
        e.1 = e.1.max(ts);
    }
}

/// A Bash command without the bodies of its heredocs (a patch or a file being written is not
/// a use of the commands it mentions).
fn without_heredocs(cmd: &str) -> String {
    let mut out = String::new();
    let mut end: Option<String> = None;
    for l in cmd.lines() {
        if let Some(word) = &end {
            if l.trim() == word {
                end = None;
            }
            continue;
        }
        out.push_str(l);
        out.push('\n');
        if let Some(i) = l.find("<<") {
            let rest = l[i + 2..].trim_start_matches(['-', '~']).trim_start();
            let word: String = rest
                .trim_start_matches(['\'', '"'])
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !word.is_empty() {
                end = Some(word);
            }
        }
    }
    out
}

/// The words of a Bash command: split at spaces and at `; & | ( ) < > \``.
fn words(cmd: &str) -> impl Iterator<Item = &str> {
    cmd.split(|c: char| c.is_whitespace() || ";&|()<>`".contains(c))
        .filter(|w| !w.is_empty())
}

/// `<command-name>/NAME</command-name>` in what the user typed: a skill run as a command.
fn typed_commands(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("<command-name>") {
        rest = &rest[i + "<command-name>".len()..];
        let Some(j) = rest.find("</command-name>") else {
            break;
        };
        out.push(rest[..j].trim().trim_start_matches('/'));
        rest = &rest[j..];
    }
    out
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|x| x.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// One use found in a file: what was used, when, and what it was found in (the tool_use id,
/// or the line's uuid for a typed `/NAME`), so that a copy of it in another file is not
/// counted again.
struct Hit {
    from: Option<String>,
    kind: Kind,
    name: String,
    ts: i64,
}

/// The tool calls (their ids) and the uses in one log file, and its first working directory.
type FileUse = (Vec<Option<String>>, Vec<Hit>, Option<String>);

/// What one log file used. Read as `load` reads: a line that is not JSON is passed over,
/// and so is a last line still being written.
fn read_file(path: &Path, f: &Filter, clis: &BTreeSet<&str>) -> io::Result<FileUse> {
    let (mut calls, mut hits, mut cwd) = (Vec::new(), Vec::new(), None);
    lines::scan_from(
        path,
        0,
        |_, _| true,
        |_, line| {
            let call = memchr::memmem::find(line, b"\"tool_use\"").is_some();
            let typed = memchr::memmem::find(line, b"<command-name>").is_some();
            if !call && !typed {
                return;
            }
            if let Some(v) = jsonl::parse(line) {
                if cwd.is_none() {
                    cwd = v.get("cwd").and_then(Value::as_str).map(str::to_string);
                }
                read_line(&v, f, clis, typed, &mut calls, &mut hits);
            }
        },
    )?;
    Ok((calls, hits, cwd))
}

fn read_line(
    v: &Value,
    f: &Filter,
    clis: &BTreeSet<&str>,
    typed: bool,
    calls: &mut Vec<Option<String>>,
    hits: &mut Vec<Hit>,
) {
    let Some(ts) = v
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(time::parse_iso)
    else {
        return;
    };
    if ts < f.since || ts >= f.until {
        return;
    }
    let mut hit = |from: Option<String>, kind: Kind, name: &str| {
        hits.push(Hit {
            from,
            kind,
            name: name.to_string(),
            ts,
        })
    };
    let content = v.pointer("/message/content").unwrap_or(&Value::Null);
    if typed && v.get("type").and_then(Value::as_str) == Some("user") {
        // a copied line has the same uuid; without one, the same time and name
        let uuid = v.get("uuid").and_then(Value::as_str);
        for name in typed_commands(&text_of(content)) {
            let from = uuid.map_or_else(|| format!("typed {ts} {name}"), str::to_string);
            hit(Some(from), Kind::Skill, name);
        }
    }
    for b in content.as_array().into_iter().flatten() {
        if b.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }
        let id = b.get("id").and_then(Value::as_str).map(str::to_string);
        calls.push(id.clone());
        let tool = b.get("name").and_then(Value::as_str).unwrap_or("");
        let input = b.get("input").unwrap_or(&Value::Null);
        if let Some(rest) = tool.strip_prefix("mcp__") {
            let server = rest.split("__").next().unwrap_or(rest);
            hit(id.clone(), Kind::Mcp, server);
        } else if tool == "Skill" {
            if let Some(s) = input.get("skill").and_then(Value::as_str) {
                hit(id.clone(), Kind::Skill, s.trim_start_matches('/'));
            }
        } else if tool == "Bash"
            && let Some(cmd) = input.get("command").and_then(Value::as_str)
        {
            let cmd = without_heredocs(cmd);
            let seen: BTreeSet<&str> = words(&cmd).filter(|w| clis.contains(w)).collect();
            for w in seen {
                hit(id.clone(), Kind::Cli, w);
            }
        }
    }
}

fn usage(root: &Path, f: &Filter, clis: &BTreeSet<&str>) -> io::Result<(Usage, usize)> {
    let files = load::select_files(root, &load::any_project(f))?;
    let results = load::read_all(&files, |p| {
        let (calls, hits, cwd) = read_file(p, f, clis).ok()?;
        let keep = !calls.is_empty() && load::project_wanted(root, p, cwd.as_deref(), &f.projects);
        keep.then_some((calls, hits))
    });
    let mut all = Usage::default();
    let mut contexts = 0;
    // A session can start with a copy of another's history: what an earlier file had
    // (the same tool_use id, the same typed line) is not counted again.
    let mut seen: HashSet<String> = HashSet::new();
    for (_, (calls, hits)) in results {
        let new = |from: &Option<String>| from.as_ref().is_none_or(|k| !seen.contains(k));
        let n = calls.iter().filter(|id| new(id)).count();
        for h in hits.iter().filter(|h| new(&h.from)) {
            all.hit(h.kind, &h.name, h.ts);
        }
        all.calls += n;
        contexts += usize::from(n > 0);
        seen.extend(calls.into_iter().flatten());
        seen.extend(hits.into_iter().filter_map(|h| h.from));
    }
    Ok((all, contexts))
}

// ---------------------------------------------------------------------------------------
// the report

pub fn run(root: &Path, f: &Filter, o: &UnusedOpts, path_var: &str) -> io::Result<(String, Value)> {
    let inst = installed(&o.home, path_var);
    let clis: BTreeSet<&str> = inst
        .items
        .iter()
        .filter(|it| it.kind == Kind::Cli)
        .map(|it| it.name.as_str())
        .collect();
    let (u, contexts) = usage(root, f, &clis)?;

    struct Row<'a> {
        it: &'a Item,
        uses: usize,
        last: Option<i64>,
    }
    let mut rows: Vec<Row> = inst
        .items
        .iter()
        .map(|it| {
            let (mut uses, mut last) = (0, None::<i64>);
            for k in &it.keys {
                if let Some(&(n, t)) = u.used.get(&(it.kind, k.clone())) {
                    uses += n;
                    last = Some(last.map_or(t, |l| l.max(t)));
                }
            }
            Row { it, uses, last }
        })
        .collect();
    rows.sort_by(|a, b| (a.uses, a.it.kind, &a.it.name).cmp(&(b.uses, b.it.kind, &b.it.name)));

    let until = f.until.min(time::now());
    let mut out = format!(
        "{} .. {} (local): {} tool calls in {} contexts\n",
        time::minute(f.since),
        time::minute(until),
        u.calls,
        contexts
    );
    let pw = inst.places.iter().map(|p| p.path.len()).max().unwrap_or(0);
    out.push_str("read:\n");
    for p in inst.places.iter().filter(|p| p.found.is_ok()) {
        let _ = writeln!(out, "  {:<pw$}  {}", p.path, p.found.as_ref().unwrap());
    }
    if inst.places.iter().any(|p| p.found.is_err()) {
        out.push_str("not read (skipped):\n");
        for p in inst.places.iter().filter(|p| p.found.is_err()) {
            let _ = writeln!(out, "  {:<pw$}  {}", p.path, p.found.as_ref().unwrap_err());
        }
    }
    out.push('\n');
    let nw = rows
        .iter()
        .map(|r| r.it.name.chars().count())
        .max()
        .unwrap_or(4)
        .max(4);
    let _ = writeln!(
        out,
        "{:<5}  {:<nw$}  {:>5}  {:<10}  from",
        "kind", "name", "uses", "last"
    );
    for r in &rows {
        let _ = writeln!(
            out,
            "{:<5}  {:<nw$}  {:>5}  {:<10}  {}",
            r.it.kind.name(),
            r.it.name,
            r.uses,
            r.last.map_or("-".to_string(), time::day),
            r.it.from.join(", ")
        );
    }
    if rows.is_empty() {
        out.push_str("(nothing installed was found)\n");
    }
    let j = json!({
        "since": time::minute(f.since), "until": time::minute(until),
        "calls": u.calls, "contexts": contexts,
        "read": inst.places.iter().map(|p| match &p.found {
            Ok(s) => json!({"place": p.path, "found": s}),
            Err(e) => json!({"place": p.path, "error": e}),
        }).collect::<Vec<_>>(),
        "items": rows.iter().map(|r| json!({
            "kind": r.it.kind.name(), "name": r.it.name, "uses": r.uses,
            "last": r.last.map(time::day), "from": r.it.from,
        })).collect::<Vec<_>>(),
    });
    Ok((out, j))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_is_named_in_its_tools_with_underscores_for_the_rest() {
        assert_eq!(tool_name("claude.ai Some Drive"), "claude_ai_Some_Drive");
        assert_eq!(tool_name("edge-api"), "edge-api");
    }

    #[test]
    fn commands_named_but_not_what_to_avoid_or_subcommands() {
        let s = "- Search: `aa` (over bb), `cc` (over dd)\n\
                 - Replace: `ee` in a pipe. Always use `ff` (format: `ff --help`; `<<<<<<< OLD` part)\n\
                 - Data: `gg`, `hh`\n\
                 - Servers: `ii start NAME` returns; then `ii logs`, `ls`, `wait URL`, `stop NAME`.\n  \
                   Use them instead of `cmd &`, `sleep` loops, `lsof -i` and `pkill` (`ii --help`).\n\
                 - More: `jj run -- CMD` (`.jj.toml`), `jj ls`, `logs --follow`, `stop`; `--json` (or `all`)\n\
                 - Heavy jobs (`kk run`, tests), when asked: `ll run -- CMD`\n\
                 - Check it with `mm` (format: `mm --help`)\n\
                 - To fix one, use `nn` (format: `nn --help`). Chain it (`nn <<'EOF' && x`): later\n\
                 - Logs with `oo log --source a` (or `b`), `oo show`\n";
        assert_eq!(
            named_commands(s),
            [
                "aa", "cc", "ee", "ff", "gg", "hh", "ii", "jj", "ll", "mm", "nn", "oo"
            ]
        );
    }

    #[test]
    fn the_section_ends_at_the_next_heading_of_its_level() {
        let t = "# A\n- `x`\n# CLI tools\n- `y`\n## Sub\n- `z`\n```\n# not a heading\n```\n# Next\n- `w`\n";
        let s = cli_section(t).unwrap();
        assert!(s.contains("`y`") && s.contains("`z`") && s.contains("# not a heading"));
        assert!(!s.contains("`w`") && !s.contains("`x`"));
        assert!(cli_section("# Other\n").is_none());
    }

    #[test]
    fn heredoc_bodies_are_not_commands() {
        let c = "aa <<'EOF' && bb\nqq inside\nEOF\ncc x";
        let w: Vec<String> = words(&without_heredocs(c)).map(String::from).collect();
        assert!(w.contains(&"aa".into()) && w.contains(&"bb".into()) && w.contains(&"cc".into()));
        assert!(!w.contains(&"qq".into()));
    }
}
