//! `rules`: take stock of the prohibitions in the memory and instruction files, so that how
//! many there are is known and they can be read side by side. Whether two of them collide is
//! a judgement of meaning; this only finds the lines that forbid something and lists them.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use regex_lite::Regex;
use serde_json::{Value, json};

use crate::cli::SourceSel;
use crate::memory::{Doc, Scope, Zone, collect, folder_name, instruction_file};
use crate::places;
use crate::talk::Source;

/// The lines that count as a prohibition. Loose on purpose: a line that only mentions one
/// is counted too, since this is material to read. `--pattern RE` replaces it.
pub const DEFAULT_PATTERN: &str = concat!(
    "(?i)",
    // Japanese: the plain and polite negatives of what is done, and the words for forbidding
    "しない|しません|使わない|書かない|入れない|渡さない|提案しない|禁止|やめる|避ける",
    "|ならない|してはいけ|いけません|使うな",
    // a sentence ending in ない
    "|ない(こと)?([。．、）)]|$)",
    // English
    r"|\b(never|do not|must not|avoid)\b|\bdon['’]t\b",
);

pub struct RulesOpts {
    pub pattern: Regex,
    /// `--repo DIR`: what a session started in DIR reads, not every memory.
    pub repo: Option<PathBuf>,
    pub count: bool,
    pub scope: Scope,
    pub home: String,
}

/// Where a file lives, which tells whether a rule is in the right place: a rule for one
/// project that sits in `Global`, or one for every project that sits in `Repo` or `Memory`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Place {
    /// `~/.claude/CLAUDE.md`, `~/.codex/AGENTS.md`.
    Global,
    /// The AGENTS.md and CLAUDE.md of the `--repo` directory.
    Repo,
    /// A Claude Code project's memory.
    Memory,
    /// `~/.codex/memories`.
    CodexMemory,
}

impl Place {
    fn name(self) -> &'static str {
        match self {
            Place::Global => "global",
            Place::Repo => "repo",
            Place::Memory => "memory",
            Place::CodexMemory => "codex-memory",
        }
    }
}

/// A file read, with where it lives and the other projects holding a memory of the same name.
struct Entry {
    doc: Doc,
    place: Place,
    /// Only for a Claude memory (not `MEMORY.md`); sorted, without the file's own project.
    also_in: Vec<String>,
}

impl Entry {
    /// `also in: a, b`, or nothing.
    fn mark(&self) -> Option<String> {
        (!self.also_in.is_empty()).then(|| format!("also in: {}", self.also_in.join(", ")))
    }
}

/// The last component of a memory's path: what two projects' copies are told to be the same by.
fn base(d: &Doc) -> String {
    Path::new(&d.file)
        .file_name()
        .map_or_else(|| d.file.clone(), |n| n.to_string_lossy().into_owned())
}

/// Per file name, the projects whose Claude memories hold a file of it. Looked at across all
/// projects, however the files to read were narrowed. A file shown through several folders
/// is one file in one project (`collect` merges them by the real path).
fn projects_by_name(
    o: &RulesOpts,
    read: &[Doc],
) -> Result<HashMap<String, BTreeSet<String>>, String> {
    let sc = &o.scope;
    if !sc.source.claude() {
        return Ok(HashMap::new());
    }
    let narrowed = o.repo.is_some() || !sc.projects.is_empty() || !sc.excludes.is_empty();
    let all;
    let docs: &[Doc] = if narrowed {
        all = collect(&Scope {
            root: sc.root.clone(),
            codex_memories: sc.codex_memories.clone(),
            source: SourceSel::Claude,
            projects: Vec::new(),
            excludes: Vec::new(),
            mtype: None,
            index: false,
            folders: None,
        })?;
        &all
    } else {
        read
    };
    let mut by: HashMap<String, BTreeSet<String>> = HashMap::new();
    for d in docs
        .iter()
        .filter(|d| d.memory && !d.index && d.source == Source::Claude)
    {
        by.entry(base(d)).or_default().insert(d.project.clone());
    }
    Ok(by)
}

/// The files to read: `~/.claude/CLAUDE.md` and the repository's own files when a
/// repository is given, then the memories, then Codex's. Each real file once.
fn documents(o: &RulesOpts) -> Result<Vec<Entry>, String> {
    let sc = &o.scope;
    let home = Path::new(&o.home);
    let mut docs: Vec<(Doc, Place)> = Vec::new();
    let mut files: Vec<(PathBuf, Source, Place)> = Vec::new();
    let mut scope = Scope {
        root: sc.root.clone(),
        codex_memories: sc.codex_memories.clone(),
        source: sc.source,
        projects: sc.projects.clone(),
        excludes: sc.excludes.clone(),
        mtype: None,
        index: false,
        folders: None,
    };
    if let Some(repo) = &o.repo {
        let real =
            std::fs::canonicalize(repo).map_err(|e| format!("--repo {}: {e}", repo.display()))?;
        if !real.is_dir() {
            return Err(format!("--repo {}: not a directory", repo.display()));
        }
        if sc.source.claude() {
            files.push((
                places::claude_dir(home).join("CLAUDE.md"),
                Source::Claude,
                Place::Global,
            ));
        }
        if sc.source.codex() {
            files.push((
                places::codex_dir(home).join("AGENTS.md"),
                Source::Codex,
                Place::Global,
            ));
        }
        files.push((real.join("AGENTS.md"), Source::Claude, Place::Repo));
        files.push((real.join("CLAUDE.md"), Source::Claude, Place::Repo));
        let mut folders = vec![folder_name(&real.to_string_lossy())];
        folders.push(folder_name(&repo.to_string_lossy()));
        scope.folders = Some(folders);
    } else if sc.source.codex() {
        files.push((
            places::codex_dir(home).join("AGENTS.md"),
            Source::Codex,
            Place::Global,
        ));
    }
    for (p, s, place) in files {
        docs.extend(instruction_file(&p, s, &o.home).map(|d| (d, place)));
    }
    let memories = collect(&scope)?;
    let by_name = projects_by_name(o, &memories)?;
    docs.extend(memories.into_iter().map(|d| {
        let place = if d.source == Source::Codex {
            Place::CodexMemory
        } else {
            Place::Memory
        };
        (d, place)
    }));
    let mut seen = HashSet::new();
    docs.retain(|(d, _)| seen.insert(d.path.clone()));
    Ok(docs
        .into_iter()
        .map(|(doc, place)| {
            let also_in = if place == Place::Memory && !doc.index {
                by_name
                    .get(&base(&doc))
                    .into_iter()
                    .flatten()
                    .filter(|p| **p != doc.project)
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
            Entry {
                doc,
                place,
                also_in,
            }
        })
        .collect())
}

/// The lines that forbid, with their numbers: text, not frontmatter, not code, not headings.
fn prohibitions<'a>(d: &'a Doc, re: &Regex) -> Vec<(usize, &'a str)> {
    d.lines
        .iter()
        .filter(|l| l.zone == Zone::Body)
        .filter(|l| {
            let t = l.text.trim();
            !t.is_empty() && !t.starts_with('#') && re.is_match(t)
        })
        .map(|l| (l.no, l.text.trim()))
        .collect()
}

/// The width of the place column: `codex-memory` is the longest.
const PLACE: usize = 12;

/// The text to print and the JSON lines. An error is a message for exit 3 (or 2).
pub fn run(o: &RulesOpts) -> Result<(String, Vec<Value>), String> {
    let entries = documents(o)?;
    let mut per_file: Vec<(&Entry, Vec<(usize, &str)>)> = entries
        .iter()
        .map(|e| (e, prohibitions(&e.doc, &o.pattern)))
        .filter(|(_, v)| !v.is_empty())
        .collect();
    per_file.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then(a.0.doc.shown().cmp(&b.0.doc.shown()))
    });
    let total: usize = per_file.iter().map(|(_, v)| v.len()).sum();

    let mut out = String::new();
    let width = per_file
        .iter()
        .map(|(_, v)| v.len().to_string().len())
        .max()
        .unwrap_or(1)
        .max("count".len());
    out.push_str(&format!("{:>width$}  {:<PLACE$}  file\n", "count", "place"));
    for (e, v) in &per_file {
        let mark = e.mark().map(|m| format!("  ({m})")).unwrap_or_default();
        out.push_str(&format!(
            "{:>width$}  {:<PLACE$}  {}{mark}\n",
            v.len(),
            e.place.name(),
            e.doc.shown()
        ));
    }
    out.push_str(&format!(
        "{:>width$}  {:<PLACE$}  total ({} files with any, {} files read)\n",
        total,
        "",
        per_file.len(),
        entries.len()
    ));

    let mut objects = Vec::new();
    if o.count {
        for (e, v) in &per_file {
            objects.push(json!({
                "file": e.doc.shown(),
                "path": e.doc.path.to_string_lossy(),
                "place": e.place.name(),
                "also_in": e.also_in,
                "count": v.len(),
            }));
        }
        objects.push(json!({
            "total": total,
            "files_with_any": per_file.len(),
            "files_read": entries.len(),
        }));
        return Ok((out, objects));
    }
    out.push('\n');
    for (e, v) in &per_file {
        let tag = match e.mark() {
            Some(m) => format!("{}; {m}", e.place.name()),
            None => e.place.name().to_string(),
        };
        for (no, text) in v {
            out.push_str(&format!("{}:{no}  [{tag}]  {text}\n", e.doc.shown()));
            objects.push(json!({
                "source": e.doc.source.name(),
                "file": e.doc.shown(),
                "path": e.doc.path.to_string_lossy(),
                "place": e.place.name(),
                "also_in": e.also_in,
                "line": no,
                "text": text,
            }));
        }
    }
    Ok((out, objects))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is(s: &str) -> bool {
        Regex::new(DEFAULT_PATTERN).unwrap().is_match(s)
    }

    #[test]
    fn the_default_pattern_catches_japanese_and_english_prohibitions() {
        for s in [
            "force push はしない",
            "勝手に消しません",
            "Agent ツールは使わない",
            "ホストへ依存を入れない。",
            "鍵をログに渡さない",
            "余計な飾りは提案しない",
            "mainへの直接pushは禁止",
            "Never edit it by hand",
            "Don't run it twice",
            "do not touch this",
            "You must not commit",
            "Avoid sed -i",
            "変更を巻き戻さないこと。",
            "何も書かない",
        ] {
            assert!(is(s), "{s}");
        }
        for s in ["push してよい", "テストを通す", "ありがとうございます"] {
            assert!(!is(s), "{s}");
        }
    }
}
