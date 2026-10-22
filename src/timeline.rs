//! `timeline`: what the user said and what was committed, in one list by time. For a
//! retrospective or a handoff, where the talk and `git log` were laid side by side by hand.
//!
//! Utterances come from the same sessions `search` reads (what a person typed, the
//! assistant's text). The commits come from `git log`. Which sessions belong to the
//! repository is decided by where they worked (their `cwd`), see `Repo::owns`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};

use crate::recall::{WhoFilter, one_line};
use crate::talk::{Session, Who};
use crate::time;

pub struct TimelineOpts {
    pub repo: std::path::PathBuf,
    pub who: WhoFilter,
}

/// Characters of an utterance or a commit subject shown on a line.
const HEAD: usize = 100;

pub struct Commit {
    /// The author time: when the work was written, which a rebase or a merge does not move.
    pub ts: i64,
    pub hash: String,
    /// What `git log` abbreviates it to; what `git show` takes.
    pub short: String,
    pub subject: String,
}

/// The directories a repository's sessions work in.
pub struct Repo {
    /// The main checkout, then every linked worktree git still knows, then the place T3 Code
    /// puts worktrees (`~/.t3/worktrees/<repo name>/`), which also covers the ones deleted
    /// since: git no longer lists them, but their sessions are still in the logs.
    pub roots: Vec<String>,
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let first = err.lines().next().unwrap_or("git failed");
        return Err(format!(
            "--repo {}: {first} (git {})",
            dir.display(),
            args.join(" ")
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The paths in `git worktree list --porcelain`, the main checkout first.
fn worktree_paths(porcelain: &str) -> Vec<String> {
    porcelain
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .map(|p| p.trim_end_matches('/').to_string())
        .collect()
}

fn parse_log(out: &str) -> Vec<Commit> {
    out.lines()
        .filter_map(|l| {
            let mut p = l.splitn(4, '\x1f');
            let (hash, short, at, subject) = (p.next()?, p.next()?, p.next()?, p.next()?);
            Some(Commit {
                ts: at.parse().ok()?,
                hash: hash.to_string(),
                short: short.to_string(),
                subject: subject.to_string(),
            })
        })
        .collect()
}

impl Repo {
    pub fn find(dir: &Path, home: &str) -> Result<Repo, String> {
        let listed = git(dir, &["worktree", "list", "--porcelain"])?;
        let mut roots = worktree_paths(&listed);
        if let Some(name) = roots
            .first()
            .and_then(|m| m.rsplit('/').next())
            .filter(|n| !n.is_empty() && !home.is_empty())
        {
            roots.push(format!(
                "{}/.t3/worktrees/{name}",
                home.trim_end_matches('/')
            ));
        }
        if roots.is_empty() {
            return Err(format!("--repo {}: git lists no worktree", dir.display()));
        }
        Ok(Repo { roots })
    }

    /// Whether `s` belongs to the repository, judged by where its lines were written.
    pub fn owns(&self, s: &Session) -> bool {
        belongs(&s.cwds, &self.roots)
    }
}

/// `path` is `root` or inside it.
fn is_under(path: &str, root: &str) -> bool {
    path == root || path.strip_prefix(root).is_some_and(|r| r.starts_with('/'))
}

/// A session belongs to the repository when it worked there at least as much as anywhere
/// else. Lines written in a directory above the repository (the folder the session was
/// opened in, `~/src/shop` for `shop/web`) say where it started, not
/// what it worked on, so they count for neither side.
fn belongs(cwds: &HashMap<String, u32>, roots: &[String]) -> bool {
    let (mut inside, mut elsewhere) = (0u32, 0u32);
    for (cwd, n) in cwds {
        let c = cwd.trim_end_matches('/');
        if roots.iter().any(|r| is_under(c, r)) {
            inside += n;
        } else if c.is_empty() || roots.iter().any(|r| is_under(r, c)) {
            // above the repository: the place it started
        } else {
            elsewhere += n;
        }
    }
    inside > 0 && inside >= elsewhere
}

/// Commits of every local and remote-tracking branch (once each), merges left out, whose
/// author time is in the window. Oldest first.
pub fn commits(dir: &Path, since: i64, until: i64) -> Result<Vec<Commit>, String> {
    let mut args = vec![
        "log",
        "--branches",
        "--remotes",
        "--no-merges",
        "--format=%H%x1f%h%x1f%at%x1f%s",
    ];
    // git's own --since looks at the committer time, which is never before the author time,
    // so it only drops commits that are older either way. --until it cannot do for author times.
    let since_arg = format!("--since={since}");
    if since > 0 {
        args.push(&since_arg);
    }
    let mut v = parse_log(&git(dir, &args)?);
    v.retain(|c| c.ts >= since && c.ts < until);
    v.sort_by_key(|c| c.ts);
    Ok(v)
}

fn head(text: &str) -> String {
    let one = one_line(text);
    let mut s: String = one.chars().take(HEAD).collect();
    if one.chars().count() > HEAD {
        s.push('…');
    }
    s
}

enum Item<'a> {
    Say(&'a Session, Who, &'a str),
    Commit(&'a Commit),
}

/// The lines to print, one JSON object per line, and a note if there is nothing.
pub fn render(
    sessions: &[&Session],
    commits: &[Commit],
    who: WhoFilter,
) -> (String, Vec<Value>, Option<String>) {
    // The talk first, then the commits, so that at the same second a request comes before
    // what it led to; the sort is stable, so the rest keeps the order it was read in.
    let mut items: Vec<(i64, u8, Item)> = Vec::new();
    for s in sessions {
        for e in &s.events {
            let Some(w) = e.who else { continue };
            let wanted = match who {
                WhoFilter::Any => true,
                WhoFilter::User => w == Who::User,
                WhoFilter::Assistant => w == Who::Assistant,
            };
            if wanted {
                items.push((e.ts, 0, Item::Say(s, w, &e.text)));
            }
        }
    }
    for c in commits {
        items.push((c.ts, 1, Item::Commit(c)));
    }
    items.sort_by_key(|(ts, rank, _)| (*ts, *rank));

    let idw = items
        .iter()
        .map(|(_, _, i)| match i {
            Item::Say(s, ..) => s.id().len(),
            Item::Commit(c) => c.short.len(),
        })
        .max()
        .unwrap_or(8);
    let mut out = String::new();
    let mut objects = Vec::new();
    let mut day = String::new();
    // The session of the line just before, when that was an utterance: its id is not repeated.
    let mut last: Option<(&str, &Option<String>)> = None;
    for (ts, _, item) in &items {
        let d = time::day(*ts);
        if d != day {
            let _ = writeln!(out, "--- {d} ---");
            day = d;
            last = None;
        }
        match item {
            Item::Say(s, w, text) => {
                let same = last == Some((s.sid.as_str(), &s.agent));
                let id = if same { String::new() } else { s.id() };
                let _ = writeln!(
                    out,
                    "{}  {:<9}  {:<idw$}  | {}",
                    time::minute(*ts),
                    w.name(),
                    id,
                    head(text)
                );
                last = Some((s.sid.as_str(), &s.agent));
                objects.push(json!({
                    "time": time::minute(*ts),
                    "timestamp": time::iso_utc(*ts),
                    "kind": w.name(),
                    "source": s.source.name(),
                    "id": s.id(),
                    "session": s.sid,
                    "agent": s.agent,
                    "project": s.project,
                    "snippet": head(text),
                }));
            }
            Item::Commit(c) => {
                let _ = writeln!(
                    out,
                    "{}  {:<9}  {:<idw$}  | {}",
                    time::minute(*ts),
                    "commit",
                    c.short,
                    head(&c.subject)
                );
                last = None;
                objects.push(json!({
                    "time": time::minute(*ts),
                    "timestamp": time::iso_utc(*ts),
                    "kind": "commit",
                    "id": c.short,
                    "hash": c.hash,
                    "snippet": head(&c.subject),
                }));
            }
        }
    }
    let note = items.is_empty().then(|| {
        format!(
            "0 items: no utterance and no commit in the window ({} sessions matched the repository, {} commits)",
            sessions.len(),
            commits.len()
        )
    });
    (out, objects, note)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwds(v: &[(&str, u32)]) -> HashMap<String, u32> {
        v.iter().map(|(c, n)| (c.to_string(), *n)).collect()
    }

    fn roots() -> Vec<String> {
        vec![
            "/h/p/shop/web".into(),
            "/h/.t3/worktrees/web".into(),
        ]
    }

    #[test]
    fn a_path_is_under_a_root_only_at_a_directory_boundary() {
        assert!(is_under("/a/b", "/a/b"));
        assert!(is_under("/a/b/c", "/a/b"));
        assert!(!is_under("/a/bc", "/a/b"));
        assert!(!is_under("/a", "/a/b"));
    }

    #[test]
    fn sessions_are_judged_by_where_they_worked() {
        let r = roots();
        // inside the repository, in a subfolder, in a worktree T3 made (deleted since)
        assert!(belongs(&cwds(&[("/h/p/shop/web", 3)]), &r));
        assert!(belongs(&cwds(&[("/h/p/shop/web/src", 3)]), &r));
        assert!(belongs(&cwds(&[("/h/.t3/worktrees/web/api", 3)]), &r));
        // a sibling with the same name prefix is not the repository
        assert!(!belongs(&cwds(&[("/h/p/shop/web-two", 9)]), &r));
        // opened in the parent folder: that alone says nothing, the work decides
        assert!(!belongs(&cwds(&[("/h/p/shop", 50)]), &r));
        assert!(belongs(
            &cwds(&[("/h/p/shop", 50), ("/h/p/shop/web", 2)]),
            &r
        ));
        // worked in the sibling more than here
        assert!(!belongs(
            &cwds(&[
                ("/h/p/shop", 50),
                ("/h/p/shop/web", 2),
                ("/h/p/shop/other", 5)
            ]),
            &r
        ));
        // half is enough
        assert!(belongs(
            &cwds(&[("/h/p/shop/web", 5), ("/h/p/shop/other", 5)]),
            &r
        ));
        assert!(!belongs(&cwds(&[("/", 4), ("/tmp", 1)]), &r));
    }

    #[test]
    fn the_main_checkout_is_the_first_worktree_listed() {
        let out = "worktree /a/repo\nHEAD abc\nbranch refs/heads/main\n\nworktree /b/wt/\nHEAD def\ndetached\n";
        assert_eq!(worktree_paths(out), vec!["/a/repo", "/b/wt"]);
    }

    #[test]
    fn a_log_line_has_four_fields_and_the_subject_may_hold_the_separator() {
        let v = parse_log("aaa\x1faa\x1f100\x1fone\x1ftwo\nbad line\nbbb\x1fbb\x1f200\x1fx\n");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].subject, "one\x1ftwo");
        assert_eq!((v[1].ts, v[1].short.as_str()), (200, "bb"));
    }

    #[test]
    fn a_line_shows_the_first_hundred_characters_on_one_line() {
        assert_eq!(head("a\n  b"), "a b");
        let long = "あ".repeat(150);
        assert_eq!(head(&long).chars().count(), HEAD + 1);
        assert!(head(&long).ends_with('…'));
    }
}
