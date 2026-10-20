//! `memory`: search the memory files agents keep. Claude Code's are
//! `~/.claude/projects/<folder>/memory/*.md` (a frontmatter with `name`, `description` and
//! `type`, then the text); Codex's are any `.md` under `~/.codex/memories/`. The same
//! reading (`Doc`, `collect`) serves `rules`, which counts the prohibitions in them.
//!
//! These are not session logs: nothing here goes through the cache.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use regex_lite::Regex;
use serde_json::{Value, json};

use crate::cli::SourceSel;
use crate::load::short_project;
use crate::talk::{Source, project_name};

/// What a line of a file is, for the commands that read it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zone {
    /// Inside the frontmatter, other than `name` and `description`.
    Front,
    /// The frontmatter's `name` and `description` (and the lines a block value goes on to).
    Meta,
    Body,
    /// Inside a fenced code block.
    Code,
    /// The line that opens or closes a fence.
    Fence,
}

pub struct Line {
    /// 1-based, as in the file.
    pub no: usize,
    pub text: String,
    pub zone: Zone,
}

pub struct Doc {
    pub source: Source,
    /// Where it is shown from: the project of a Claude memory, `codex`, or `-` for a file
    /// that is not a memory (`CLAUDE.md`, `AGENTS.md`).
    pub project: String,
    /// The file as shown: its path under the memory folder; for a file that is not a memory,
    /// its path with the home directory written `~`.
    pub file: String,
    /// The real file (symlinks resolved): what two names for one file are told apart by.
    pub path: PathBuf,
    /// Every project folder the file was found under (a linked `memory` folder shows the
    /// same files under several), with the project name of each.
    pub seen: Vec<(String, String)>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub mtype: Option<String>,
    /// `MEMORY.md`, the list of the other files.
    pub index: bool,
    /// A memory file, as against `CLAUDE.md` and the like.
    pub memory: bool,
    pub lines: Vec<Line>,
}

impl Doc {
    /// What a table calls the file: unique across projects.
    pub fn shown(&self) -> String {
        if self.memory {
            format!("{}/{}", self.project, self.file)
        } else {
            self.file.clone()
        }
    }
}

// ---------------------------------------------------------------------------------------
// Reading one file

fn unquote(v: &str) -> String {
    let v = v.trim();
    for q in ['"', '\''] {
        if let Some(inner) = v.strip_prefix(q).and_then(|s| s.strip_suffix(q)) {
            return inner.to_string();
        }
    }
    v.to_string()
}

/// How far a line is indented.
fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

struct Front {
    /// The number of lines it takes, both `---` lines included; 0 when the file has none.
    len: usize,
    name: Option<String>,
    description: Option<String>,
    mtype: Option<String>,
    /// Indexes of the lines of `name` and `description`.
    meta: Vec<usize>,
}

/// The frontmatter at the head of `lines`. `type` is under `metadata:` in the current form
/// and at the top in older files; either is read.
fn frontmatter(lines: &[&str]) -> Front {
    let none = Front {
        len: 0,
        name: None,
        description: None,
        mtype: None,
        meta: Vec::new(),
    };
    if lines.first().map(|l| l.trim_end()) != Some("---") {
        return none;
    }
    let Some(end) = lines[1..]
        .iter()
        .position(|l| matches!(l.trim_end(), "---" | "..."))
        .map(|i| i + 1)
    else {
        return none;
    };
    let mut f = Front {
        len: end + 1,
        ..none
    };
    let mut i = 1;
    while i < end {
        let l = lines[i];
        let t = l.trim();
        if let Some(v) = t.strip_prefix("type:") {
            f.mtype = Some(unquote(v).to_lowercase()).filter(|s| !s.is_empty());
        }
        let key = if indent(l) == 0 {
            ["name", "description"]
                .into_iter()
                .find(|k| t.strip_prefix(k).is_some_and(|r| r.starts_with(':')))
        } else {
            None
        };
        if let Some(k) = key {
            let v = t[k.len() + 1..].trim();
            f.meta.push(i);
            let mut value = unquote(v);
            // `description: >` and `|`: the value is the more indented lines that follow.
            if matches!(v, ">" | ">-" | "|" | "|-") {
                let parts: Vec<&str> = lines[i + 1..end]
                    .iter()
                    .take_while(|n| indent(n) > 0 || n.trim().is_empty())
                    .copied()
                    .collect();
                f.meta.extend((i + 1)..(i + 1 + parts.len()));
                i += parts.len();
                value = parts
                    .iter()
                    .map(|p| p.trim())
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            let slot = if k == "name" {
                &mut f.name
            } else {
                &mut f.description
            };
            *slot = Some(value).filter(|s| !s.is_empty());
        }
        i += 1;
    }
    f
}

/// The lines of a file with what each is. A fence opens with three backticks or tildes and
/// closes with the same kind; text inside is code.
fn zones(text: &str, with_front: bool) -> (Front, Vec<Line>) {
    let raw: Vec<&str> = text.lines().collect();
    let front = if with_front {
        frontmatter(&raw)
    } else {
        frontmatter(&[])
    };
    let mut out = Vec::with_capacity(raw.len());
    let mut fence: Option<char> = None;
    for (i, l) in raw.iter().enumerate() {
        let zone = if i < front.len {
            if front.meta.contains(&i) {
                Zone::Meta
            } else {
                Zone::Front
            }
        } else {
            let t = l.trim_start();
            let mark = ["```", "~~~"]
                .into_iter()
                .find(|m| t.starts_with(m))
                .and_then(|m| m.chars().next());
            match (fence, mark) {
                (None, Some(c)) => {
                    fence = Some(c);
                    Zone::Fence
                }
                (Some(open), Some(c)) if open == c => {
                    fence = None;
                    Zone::Fence
                }
                (Some(_), _) => Zone::Code,
                (None, None) => Zone::Body,
            }
        };
        out.push(Line {
            no: i + 1,
            text: l.to_string(),
            zone,
        });
    }
    (front, out)
}

fn read_text(p: &Path) -> Option<String> {
    fs::read(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// A file that is not a memory (`CLAUDE.md`, `AGENTS.md`): no frontmatter is read from it.
pub fn instruction_file(path: &Path, source: Source, home: &str) -> Option<Doc> {
    let text = read_text(path)?;
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (_, lines) = zones(&text, false);
    let shown = path.to_string_lossy().into_owned();
    let file = match shown.strip_prefix(home) {
        Some(rest) if !home.is_empty() && rest.starts_with('/') => format!("~{rest}"),
        _ => shown,
    };
    Some(Doc {
        source,
        project: "-".into(),
        file,
        path: real,
        seen: Vec::new(),
        name: None,
        description: None,
        mtype: None,
        index: false,
        memory: false,
        lines,
    })
}

fn memory_file(path: &Path, rel: String, source: Source, project: &str) -> Option<Doc> {
    let text = read_text(path)?;
    let real = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let (front, lines) = zones(&text, true);
    Some(Doc {
        source,
        project: project.to_string(),
        index: path.file_name().is_some_and(|n| n == "MEMORY.md"),
        file: rel,
        path: real,
        seen: Vec::new(),
        name: front.name,
        description: front.description,
        mtype: front.mtype,
        memory: true,
        lines,
    })
}

// ---------------------------------------------------------------------------------------
// Finding the files

/// Every `.md` under `dir` (links followed), in name order.
fn md_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for p in paths {
        let Ok(md) = fs::metadata(&p) else { continue };
        if md.is_dir() {
            md_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "md") {
            out.push(p);
        }
    }
}

/// The folder Claude Code keeps a directory's sessions in: everything but letters and
/// digits becomes `-`.
pub fn folder_name(dir: &str) -> String {
    dir.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The project a folder of `root` is shown as: the working directory its sessions were
/// opened in, as `search` shows it. It is found in the first lines of a few sessions, and
/// only a directory that folders back to the same name counts; otherwise the folder's own
/// name stands.
fn project_of(root: &Path, folder: &str) -> String {
    let mut files: Vec<PathBuf> = fs::read_dir(root.join(folder))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    for f in files.iter().take(5) {
        let Ok(file) = fs::File::open(f) else {
            continue;
        };
        for line in BufReader::new(file).lines().take(200).map_while(Result::ok) {
            if !line.contains("\"cwd\"") {
                continue;
            }
            let cwd = serde_json::from_str::<Value>(&line)
                .ok()
                .and_then(|v| v.get("cwd").and_then(Value::as_str).map(str::to_string));
            if let Some(cwd) = cwd.filter(|c| folder_name(c) == folder) {
                return project_name(&cwd);
            }
        }
    }
    short_project(folder)
}

pub struct Scope {
    pub root: PathBuf,
    pub codex_memories: PathBuf,
    pub source: SourceSel,
    pub projects: Vec<String>,
    pub excludes: Vec<String>,
    pub mtype: Option<String>,
    /// `MEMORY.md` files too.
    pub index: bool,
    /// Only the memories of these project folders (`rules --repo`).
    pub folders: Option<Vec<String>>,
}

/// Claude Code's memory files, each once however many folders show it.
fn claude_memories(sc: &Scope) -> Result<Vec<Doc>, String> {
    let entries =
        fs::read_dir(&sc.root).map_err(|e| format!("cannot read {}: {e}", sc.root.display()))?;
    let mut folders: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|f| sc.folders.as_ref().is_none_or(|only| only.contains(f)))
        .collect();
    folders.sort();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut docs: Vec<Doc> = Vec::new();
    let mut at: HashMap<PathBuf, usize> = HashMap::new();
    let root_real = fs::canonicalize(&sc.root).unwrap_or_else(|_| sc.root.clone());
    // The folder a file really lives in is the one whose project is shown for it, also when
    // only a folder that links to it was asked for.
    let mut name_of = |folder: &str| -> String {
        names
            .entry(folder.to_string())
            .or_insert_with(|| project_of(&sc.root, folder))
            .clone()
    };
    for folder in &folders {
        let memory = sc.root.join(folder).join("memory");
        let mut files = Vec::new();
        md_files(&memory, &mut files);
        if files.is_empty() {
            continue;
        }
        let project = name_of(folder);
        for f in files {
            let real = fs::canonicalize(&f).unwrap_or_else(|_| f.clone());
            let i = match at.get(&real) {
                Some(&i) => i,
                None => {
                    let rel = f
                        .strip_prefix(&memory)
                        .unwrap_or(&f)
                        .to_string_lossy()
                        .into_owned();
                    let home = real
                        .strip_prefix(&root_real)
                        .ok()
                        .and_then(|r| r.components().next())
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .filter(|h| root_real.join(h).join("memory").is_dir());
                    let shown = home
                        .as_deref()
                        .map_or_else(|| project.clone(), &mut name_of);
                    let Some(d) = memory_file(&f, rel, Source::Claude, &shown) else {
                        continue;
                    };
                    docs.push(d);
                    at.insert(real.clone(), docs.len() - 1);
                    docs.len() - 1
                }
            };
            docs[i].seen.push((folder.clone(), project.clone()));
        }
    }
    Ok(docs)
}

/// Codex's memory files: every `.md` under the folder. A folder that is not there is no
/// memories, which the caller may find an error.
fn codex_memories(dir: &Path) -> Vec<Doc> {
    let mut files = Vec::new();
    md_files(dir, &mut files);
    let mut seen: HashMap<PathBuf, ()> = HashMap::new();
    let mut docs = Vec::new();
    for f in files {
        let rel = f
            .strip_prefix(dir)
            .unwrap_or(&f)
            .to_string_lossy()
            .into_owned();
        if let Some(d) = memory_file(&f, rel, Source::Codex, "codex")
            && seen.insert(d.path.clone(), ()).is_none()
        {
            docs.push(d);
        }
    }
    docs
}

fn selected(d: &Doc, sc: &Scope) -> bool {
    if d.index && !sc.index {
        return false;
    }
    if let Some(t) = &sc.mtype
        && d.mtype.as_deref() != Some(t.as_str())
    {
        return false;
    }
    // A name any of the folders showing the file goes by counts.
    let names = || {
        let mut v: Vec<&str> = vec![d.project.as_str()];
        for (f, p) in &d.seen {
            v.push(f);
            v.push(p);
        }
        v
    };
    if !sc.projects.is_empty()
        && !sc
            .projects
            .iter()
            .any(|p| names().iter().any(|n| n.contains(p.as_str())))
    {
        return false;
    }
    let path = d.path.to_string_lossy();
    !sc.excludes
        .iter()
        .any(|x| path.contains(x.as_str()) || names().iter().any(|n| n.contains(x.as_str())))
}

/// The memory files `sc` picks, Claude's first. An error is a message for exit 3.
pub fn collect(sc: &Scope) -> Result<Vec<Doc>, String> {
    let mut docs = Vec::new();
    if sc.source.claude() {
        docs.extend(claude_memories(sc)?);
    }
    if sc.source.codex() {
        if !sc.codex_memories.is_dir() && sc.source == SourceSel::Codex {
            return Err(format!(
                "no Codex memories: {} does not exist (--codex-memories DIR names where they are)",
                sc.codex_memories.display()
            ));
        }
        docs.extend(codex_memories(&sc.codex_memories));
    }
    docs.retain(|d| selected(d, sc));
    Ok(docs)
}

// ---------------------------------------------------------------------------------------
// memory

pub struct MemoryOpts {
    pub re: Regex,
}

/// Longest line shown, in characters.
const SHOWN: usize = 200;

fn clip(s: &str, n: usize) -> String {
    let t = s.trim();
    let mut out: String = t.chars().take(n).collect();
    if t.chars().count() > n {
        out.push('…');
    }
    out
}

/// The files with a line that matches, the text to print, one JSON object per file, and the
/// number of lines. Name, description and the text are searched; the rest of the
/// frontmatter and the fences are not.
pub fn search(docs: &[Doc], o: &MemoryOpts) -> (String, Vec<Value>, usize) {
    let mut out = String::new();
    let mut objects = Vec::new();
    let (mut files, mut total) = (0, 0);
    for d in docs {
        let hits: Vec<&Line> = d
            .lines
            .iter()
            .filter(|l| {
                matches!(l.zone, Zone::Meta | Zone::Body | Zone::Code) && o.re.is_match(&l.text)
            })
            .collect();
        if hits.is_empty() {
            continue;
        }
        files += 1;
        total += hits.len();
        out.push_str(&format!(
            "{}  {}  {}  {}\n",
            d.project,
            d.file,
            d.mtype.as_deref().unwrap_or("-"),
            clip(d.description.as_deref().unwrap_or("-"), 100)
        ));
        for l in &hits {
            out.push_str(&format!("  {}: {}\n", l.no, clip(&l.text, SHOWN)));
        }
        objects.push(json!({
            "source": d.source.name(),
            "project": d.project,
            "file": d.file,
            "path": d.path.to_string_lossy(),
            "name": d.name,
            "description": d.description,
            "type": d.mtype,
            "also_in": d.seen.iter().map(|(_, p)| p).filter(|p| **p != d.project).collect::<Vec<_>>(),
            "matches": hits.iter().map(|l| json!({"line": l.no, "text": l.text.trim()})).collect::<Vec<_>>(),
        }));
    }
    out.push_str(&format!("{total} lines in {files} files\n"));
    (out, objects, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str, front: bool) -> Vec<(usize, &'static str)> {
        zones(text, front)
            .1
            .iter()
            .map(|l| {
                (
                    l.no,
                    match l.zone {
                        Zone::Front => "front",
                        Zone::Meta => "meta",
                        Zone::Body => "body",
                        Zone::Code => "code",
                        Zone::Fence => "fence",
                    },
                )
            })
            .collect()
    }

    #[test]
    fn the_frontmatter_gives_name_description_and_a_type_under_metadata_or_at_the_top() {
        let new = "---\nname: a-b\ndescription: \"one: two\"\nmetadata:\n  node_type: memory\n  type: Feedback\n---\nbody\n";
        let (f, _) = zones(new, true);
        assert_eq!(f.name.as_deref(), Some("a-b"));
        assert_eq!(f.description.as_deref(), Some("one: two"));
        assert_eq!(f.mtype.as_deref(), Some("feedback"));
        assert_eq!(f.len, 7);
        let old = "---\nname: x\ntype: project\n---\n";
        assert_eq!(zones(old, true).0.mtype.as_deref(), Some("project"));
        let none = zones("# title\ntype: user\n", true).0;
        assert!(none.mtype.is_none() && none.len == 0);
    }

    #[test]
    fn a_block_description_is_the_indented_lines_after_it() {
        let t =
            "---\nname: x\ndescription: >\n  first\n  second\nmetadata:\n  type: user\n---\nb\n";
        let (f, ls) = zones(t, true);
        assert_eq!(f.description.as_deref(), Some("first second"));
        assert_eq!(f.mtype.as_deref(), Some("user"));
        assert_eq!(ls[3].zone, Zone::Meta);
        assert_eq!(ls[5].zone, Zone::Front);
    }

    #[test]
    fn frontmatter_and_code_are_told_from_the_text() {
        let t = "---\nname: x\nnode: y\n---\ntext\n```sh\ncode\n```\nafter\n~~~\nmore\n~~~\n";
        assert_eq!(
            lines(t, true),
            vec![
                (1, "front"),
                (2, "meta"),
                (3, "front"),
                (4, "front"),
                (5, "body"),
                (6, "fence"),
                (7, "code"),
                (8, "fence"),
                (9, "body"),
                (10, "fence"),
                (11, "code"),
                (12, "fence"),
            ]
        );
        // a file that is not a memory has no frontmatter
        assert_eq!(lines("---\nx\n---\n", false)[1].1, "body");
        // a fence of the other kind does not close
        assert_eq!(lines("```\n~~~\nx\n```\n", true)[2].1, "code");
    }

    #[test]
    fn a_directory_becomes_the_folder_claude_code_keeps_its_sessions_in() {
        assert_eq!(
            folder_name("/Users/me/.t3/scratch/x_y"),
            "-Users-me--t3-scratch-x-y"
        );
    }
}
