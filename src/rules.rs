//! `rules`: take stock of the prohibitions in the memory and instruction files, so that how
//! many there are is known and they can be read side by side. Whether two of them collide is
//! a judgement of meaning; this only finds the lines that forbid something and lists them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use regex_lite::Regex;
use serde_json::{Value, json};

use crate::memory::{Doc, Scope, Zone, collect, folder_name, instruction_file};
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

/// The files to read: `~/.claude/CLAUDE.md` and the repository's own files when a
/// repository is given, then the memories, then Codex's. Each real file once.
fn documents(o: &RulesOpts) -> Result<Vec<Doc>, String> {
    let sc = &o.scope;
    let home = Path::new(&o.home);
    let mut docs: Vec<Doc> = Vec::new();
    let mut files: Vec<(PathBuf, Source)> = Vec::new();
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
            files.push((home.join(".claude/CLAUDE.md"), Source::Claude));
        }
        if sc.source.codex() {
            files.push((home.join(".codex/AGENTS.md"), Source::Codex));
        }
        files.push((real.join("AGENTS.md"), Source::Claude));
        files.push((real.join("CLAUDE.md"), Source::Claude));
        let mut folders = vec![folder_name(&real.to_string_lossy())];
        folders.push(folder_name(&repo.to_string_lossy()));
        scope.folders = Some(folders);
    } else if sc.source.codex() {
        files.push((home.join(".codex/AGENTS.md"), Source::Codex));
    }
    for (p, s) in files {
        docs.extend(instruction_file(&p, s, &o.home));
    }
    docs.extend(collect(&scope)?);
    let mut seen = HashSet::new();
    docs.retain(|d| seen.insert(d.path.clone()));
    Ok(docs)
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

/// The text to print and the JSON lines. An error is a message for exit 3 (or 2).
pub fn run(o: &RulesOpts) -> Result<(String, Vec<Value>), String> {
    let docs = documents(o)?;
    let mut per_file: Vec<(&Doc, Vec<(usize, &str)>)> = docs
        .iter()
        .map(|d| (d, prohibitions(d, &o.pattern)))
        .filter(|(_, v)| !v.is_empty())
        .collect();
    per_file.sort_by(|a, b| {
        b.1.len()
            .cmp(&a.1.len())
            .then(a.0.shown().cmp(&b.0.shown()))
    });
    let total: usize = per_file.iter().map(|(_, v)| v.len()).sum();

    let mut out = String::new();
    let width = per_file
        .iter()
        .map(|(_, v)| v.len().to_string().len())
        .max()
        .unwrap_or(1)
        .max("count".len());
    out.push_str(&format!("{:>width$}  file\n", "count"));
    for (d, v) in &per_file {
        out.push_str(&format!("{:>width$}  {}\n", v.len(), d.shown()));
    }
    out.push_str(&format!(
        "{:>width$}  total ({} files with any, {} files read)\n",
        total,
        per_file.len(),
        docs.len()
    ));

    let mut objects = Vec::new();
    if o.count {
        for (d, v) in &per_file {
            objects.push(
                json!({"file": d.shown(), "path": d.path.to_string_lossy(), "count": v.len()}),
            );
        }
        objects.push(
            json!({"total": total, "files_with_any": per_file.len(), "files_read": docs.len()}),
        );
        return Ok((out, objects));
    }
    out.push('\n');
    for (d, v) in &per_file {
        for (no, text) in v {
            out.push_str(&format!("{}:{no}  {text}\n", d.shown()));
            objects.push(json!({
                "source": d.source.name(),
                "file": d.shown(),
                "path": d.path.to_string_lossy(),
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
            "実データを渡さない（顧客の）",
            "後回しの事項は提案しない",
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
