//! The cache of what `search`, `show` and `timeline` take from a log file (`docs/design.md`,
//! "控え"): per log file, its talk, a one-line note per tool call, and the lines' working
//! directories, with how far into the file that was read. A log only grows, so a file that
//! has grown is read again from where the cache stopped; one that shrank or was replaced is
//! read again from the start. The cache is a copy that can be rebuilt: any trouble with it
//! (no place to write, a damaged file, an old version) means reading the log, never an error.
//!
//! One rule for what is taken from a log lives in `talk::read_lines` and
//! `codex::read_lines`. `--raw` runs them over the whole file; the cache runs them over the
//! part that is new. Everything else here is bookkeeping around that.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::needle::Prefilter;
use crate::talk::{Event, Who};

/// The layout of the cache. Raise it when the layout below changes. When the code that reads
/// logs changes, `build.rs` sees to it: it hashes those sources into `READER`.
const FORMAT: u64 = 1;
const READER: &str = env!("AGENTLOG_READER");

/// Bytes at the start of the file, and just before where reading stopped, that must be the
/// same for the cache to be continued. A file that was cut and written again fails this.
const PRINT: u64 = 4096;

/// What a command needs of a session, beyond its words.
#[derive(Clone, Copy)]
pub struct Need {
    /// A one-line note for each tool call (`show`).
    pub tools: bool,
    /// Where the lines of the session worked (`timeline`).
    pub cwds: bool,
}

/// Everything taken from one log file. Not cut to a window: that is done when a `Session`
/// is made, so that one cache serves every window.
#[derive(Default)]
pub struct Extract {
    /// Claude: the working directory of the first line with words in it. Codex: the
    /// session's, from its first line.
    pub cwd: Option<String>,
    /// Claude: the session id the first line with words in it names.
    pub inner: Option<String>,
    /// Codex: a forked subagent's file starts with copies of its parent's lines, up to this
    /// ordinal; and whether the session is a subagent's.
    pub own_from: u64,
    pub subagent: bool,
    pub events: Vec<Event>,
    /// Claude: for each working directory, the time of each line with words in it.
    pub cwd_lines: BTreeMap<String, Vec<i64>>,
}

/// Where the caches go, or nowhere (`--raw`, or no place to write).
pub struct Cache {
    dir: Option<PathBuf>,
}

impl Cache {
    pub fn off() -> Cache {
        Cache { dir: None }
    }

    /// `$XDG_CACHE_HOME/agentlog`, or `~/.cache/agentlog`.
    pub fn standard() -> Cache {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let base = var("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| var("HOME").map(|h| PathBuf::from(h).join(".cache")));
        Cache {
            dir: base.map(|b| b.join("agentlog")),
        }
    }

    /// The caches of the logs under one root. `None` if there is nowhere to put them.
    pub fn slot(&self, kind: &str, root: &Path) -> Option<Slot> {
        let dir = self.dir.as_ref()?;
        let root = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let dir = dir.join(format!(
            "{kind}-{:016x}",
            fnv(root.to_string_lossy().as_bytes())
        ));
        // What was said is in here: for the user alone.
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .ok()?;
        Some(Slot { dir })
    }
}

pub struct Slot {
    dir: PathBuf,
}

impl Slot {
    fn file_for(&self, log: &Path) -> PathBuf {
        self.dir.join(format!(
            "{:016x}.jsonl",
            fnv(log.to_string_lossy().as_bytes())
        ))
    }

    /// Removes the caches of logs that are gone. `logs` is every log under the root.
    pub fn sweep(&self, logs: &[PathBuf]) {
        let keep: HashSet<PathBuf> = logs.iter().map(|l| self.file_for(l)).collect();
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
            // A `.tmpPID` is a cache being written; one left from a run that died is old.
            let left_over = ext.starts_with("tmp")
                && e.metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age.as_secs() > 3600);
            if (ext == "jsonl" && !keep.contains(&p)) || left_over {
                let _ = fs::remove_file(&p);
            }
        }
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// How a log file looked when it was read.
#[derive(PartialEq, Eq)]
struct Stat {
    size: u64,
    mtime: i64,
    mtime_ns: i64,
    ino: u64,
    dev: u64,
}

impl Stat {
    fn of(path: &Path) -> io::Result<Stat> {
        let m = fs::metadata(path)?;
        Ok(Stat {
            size: m.len(),
            mtime: m.mtime(),
            mtime_ns: m.mtime_nsec(),
            ino: m.ino(),
            dev: m.dev(),
        })
    }
}

/// A hash of up to `PRINT` bytes of the log ending at `end`; and of the first ones.
fn prints(path: &Path, end: u64) -> Option<(u64, u64)> {
    let f = fs::File::open(path).ok()?;
    let part = |at: u64, len: u64| -> Option<u64> {
        let mut b = vec![0u8; len as usize];
        f.read_exact_at(&mut b, at).ok()?;
        Some(fnv(&b))
    };
    let len = end.min(PRINT);
    Some((part(0, len)?, part(end - len, len)?))
}

// ---------------------------------------------------------------------------------------
// Reading and writing a cache file

enum State {
    /// The log is as it was when the cache was written.
    Fresh,
    /// The log has grown: read it again from here.
    Grown(u64),
}

/// The state of the cache a header describes, and what it keeps of the reading so far. `None`
/// for a cache that cannot be used: another layout or reader, another file, a log that
/// shrank or no longer begins as it did.
fn header_of(path: &Path, st: &Stat, line: &[u8]) -> Option<(State, Extract)> {
    let v: Value = serde_json::from_slice(line).ok()?;
    let num = |k: &str| v.get(k).and_then(Value::as_u64);
    let int = |k: &str| v.get(k).and_then(Value::as_i64);
    if num("format")? != FORMAT
        || v.get("reader")?.as_str()? != READER
        || v.get("path")?.as_str()? != path.to_string_lossy()
        || num("ino")? != st.ino
        || num("dev")? != st.dev
    {
        return None;
    }
    let pos = num("pos")?;
    let ex = Extract {
        cwd: v.get("cwd").and_then(Value::as_str).map(str::to_string),
        inner: v.get("inner").and_then(Value::as_str).map(str::to_string),
        own_from: num("own_from")?,
        subagent: v.get("subagent")?.as_bool()?,
        ..Extract::default()
    };
    let same =
        num("size")? == st.size && int("mtime")? == st.mtime && int("mtime_ns")? == st.mtime_ns;
    if same {
        return Some((State::Fresh, ex));
    }
    // Grown (or touched): continue only if the part already read is what it was.
    if pos == 0 || st.size < pos {
        return None;
    }
    let (head, tail) = prints(path, pos)?;
    if num("head")? != head || num("tail")? != tail {
        return None;
    }
    Some((State::Grown(pos), ex))
}

/// What a cache line says. The event lines are written by `event_line` in a fixed shape, so
/// they are read without a general JSON parser.
fn read_body(
    mut r: impl BufRead,
    ex: &mut Extract,
    need: Need,
    pre: Option<&Prefilter>,
) -> Option<()> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        line.clear();
        if r.read_until(b'\n', &mut line).ok()? == 0 {
            return Some(());
        }
        if line.last() != Some(&b'\n') {
            return None; // cut short: not a cache that was finished
        }
        line.pop();
        if let Some(rest) = line.strip_prefix(b"{\"c\":") {
            if need.cwds {
                let v: Value = serde_json::from_slice(&[b"{\"c\":", rest].concat()).ok()?;
                let cwd = v.get("c")?.as_str()?.to_string();
                let times = v.get("t")?.as_array()?;
                let times: Option<Vec<i64>> = times.iter().map(Value::as_i64).collect();
                ex.cwd_lines.insert(cwd, times?);
            }
            continue;
        }
        // {"w":"u","t":1760000000,"x":"text"}
        let rest = line.strip_prefix(b"{\"w\":\"")?;
        let who = match *rest.first()? {
            b'u' => Some(Who::User),
            b'a' => Some(Who::Assistant),
            b't' => Some(Who::Task),
            b'c' => None,
            _ => return None,
        };
        if who.is_none() && !need.tools {
            continue;
        }
        if who.is_some()
            && let Some(p) = pre
            && !p.may_match(&line)
        {
            continue;
        }
        let rest = rest.get(1..)?.strip_prefix(b"\",\"t\":")?;
        let comma = rest.iter().position(|&b| b == b',')?;
        let ts: i64 = std::str::from_utf8(&rest[..comma]).ok()?.parse().ok()?;
        let text = rest[comma..].strip_prefix(b",\"x\":")?;
        let text = text.strip_suffix(b"}")?;
        ex.events.push(Event {
            ts,
            who,
            text: serde_json::from_slice(text).ok()?,
        });
    }
}

fn event_line(out: &mut String, e: &Event) {
    let w = match e.who {
        Some(Who::User) => 'u',
        Some(Who::Assistant) => 'a',
        Some(Who::Task) => 't',
        None => 'c',
    };
    out.push_str(&format!(
        "{{\"w\":\"{w}\",\"t\":{},\"x\":{}}}\n",
        e.ts,
        Value::String(e.text.clone())
    ));
}

fn store(file: &Path, log: &Path, st: &Stat, pos: u64, ex: &Extract) -> io::Result<()> {
    let (head, tail) = prints(log, pos).ok_or_else(|| io::Error::other("log unreadable"))?;
    let header = json!({
        "format": FORMAT,
        "reader": READER,
        "path": log.to_string_lossy(),
        "size": st.size, "mtime": st.mtime, "mtime_ns": st.mtime_ns,
        "ino": st.ino, "dev": st.dev,
        "pos": pos, "head": head, "tail": tail,
        "cwd": ex.cwd, "inner": ex.inner,
        "own_from": ex.own_from, "subagent": ex.subagent,
    });
    let mut out = format!("{header}\n");
    for e in &ex.events {
        event_line(&mut out, e);
    }
    for (cwd, times) in &ex.cwd_lines {
        out.push_str(&format!("{}\n", json!({"c": cwd, "t": times})));
    }
    // Written aside and renamed, so that a reader never sees half a cache and two runs at
    // once leave one whole file.
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(out.as_bytes())?;
    }
    fs::rename(&tmp, file).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Reads a cache file: its state, and what it holds filtered by `need` and `pre`.
fn open(
    file: &Path,
    log: &Path,
    st: &Stat,
    need: Need,
    pre: Option<&Prefilter>,
) -> Option<(State, Extract)> {
    let mut r = BufReader::with_capacity(1 << 18, fs::File::open(file).ok()?);
    let mut first = Vec::new();
    r.read_until(b'\n', &mut first).ok()?;
    let (state, mut ex) = header_of(log, st, &first)?;
    // To continue a cache, all of it is needed, whatever the command wants.
    let all = Need {
        tools: true,
        cwds: true,
    };
    match state {
        State::Fresh => read_body(r, &mut ex, need, pre)?,
        State::Grown(_) => read_body(r, &mut ex, all, None)?,
    }
    Some((state, ex))
}

/// The extract of one log file, from the cache where it is good, bringing the cache up to
/// date. Without a `slot` the log is read whole, and nothing is kept.
///
/// `read(from, extract, tools, pre)` takes what is wanted from the log, from byte `from`
/// on, into the extract. `tools` says whether to take the notes of tool calls; `pre` may
/// let it leave out messages that cannot match. It returns where the next read should start.
pub fn fetch(
    slot: Option<&Slot>,
    log: &Path,
    need: Need,
    pre: Option<&Prefilter>,
    read: impl Fn(u64, &mut Extract, bool, Option<&Prefilter>) -> io::Result<u64>,
) -> io::Result<Extract> {
    let Some(slot) = slot else {
        let mut ex = Extract::default();
        read(0, &mut ex, need.tools, pre)?;
        return Ok(ex);
    };
    let st = Stat::of(log)?;
    let file = slot.file_for(log);
    let (from, mut ex) = match open(&file, log, &st, need, pre) {
        Some((State::Fresh, ex)) => return Ok(ex),
        Some((State::Grown(pos), ex)) => (pos, ex),
        _ => (0, Extract::default()),
    };
    let pos = read(from, &mut ex, true, None)?;
    let _ = store(&file, log, &st, pos, &ex);
    if !need.tools {
        ex.events.retain(|e| e.who.is_some());
    }
    Ok(ex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_line_reads_back_as_written() {
        let mut out = String::new();
        let e = Event {
            ts: 1760000000,
            who: Some(Who::Task),
            text: "a \"quoted\" line\nand,\"x\":{} more".into(),
        };
        event_line(&mut out, &e);
        let mut ex = Extract::default();
        let need = Need {
            tools: true,
            cwds: true,
        };
        read_body(out.as_bytes(), &mut ex, need, None).unwrap();
        assert_eq!(ex.events.len(), 1);
        assert_eq!(ex.events[0].text, e.text);
        assert_eq!(ex.events[0].ts, e.ts);
        assert_eq!(ex.events[0].who, e.who);
    }

    #[test]
    fn a_cache_cut_short_is_not_used() {
        let mut ex = Extract::default();
        let need = Need {
            tools: true,
            cwds: true,
        };
        assert!(
            read_body(
                &b"{\"w\":\"u\",\"t\":1,\"x\":\"a\"}"[..],
                &mut ex,
                need,
                None
            )
            .is_none()
        );
    }
}
