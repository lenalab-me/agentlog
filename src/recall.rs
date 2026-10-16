//! `search` and `show`: find what was said, then read the conversation around it.
//! Each returns the text to print and the same data as JSON (`search`: one object per hit).

use std::fmt::Write as _;

use regex_lite::Regex;
use serde_json::{Value, json};

use crate::talk::{Event, Session, Who};
use crate::time;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WhoFilter {
    User,
    Assistant,
    Any,
}

pub struct SearchOpts {
    pub re: Regex,
    /// A cheap test on a raw log line: lines it turns away cannot match `re`.
    pub prefilter: Option<crate::needle::Prefilter>,
    pub who: WhoFilter,
    pub limit: usize,
}

pub struct ShowOpts {
    pub grep: Option<Regex>,
    /// Local time given to the minute covers the whole minute.
    pub at: Option<(i64, i64)>,
    pub around: usize,
    pub full: bool,
}

/// Characters kept on each side of a match, and of the match itself.
const SIDE: usize = 40;
const MATCH: usize = 120;
/// Longest utterance `show` prints without `--full`.
const SHOW_CAP: usize = 2000;

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The match with about `SIDE` characters either side, on one line.
fn snippet(text: &str, start: usize, end: usize) -> String {
    let before = &text[..start];
    let after = &text[end..];
    let b: String = {
        let mut v: Vec<char> = before.chars().rev().take(SIDE).collect();
        v.reverse();
        v.into_iter().collect()
    };
    let m: String = text[start..end].chars().take(MATCH).collect();
    let a: String = after.chars().take(SIDE).collect();
    let mut out = String::new();
    if before.chars().count() > SIDE {
        out.push('…');
    }
    out.push_str(&b);
    out.push_str(&m);
    out.push_str(&a);
    if after.chars().count() > SIDE || text[start..end].chars().count() > MATCH {
        out.push('…');
    }
    one_line(&out)
}

struct Hit<'a> {
    ts: i64,
    who: Who,
    session: &'a Session,
    snippet: String,
}

/// The hit lines, one JSON object per hit, and a closing note if the list is cut or empty.
pub fn search(sessions: &[Session], o: &SearchOpts) -> (String, Vec<Value>, Option<String>) {
    let mut hits: Vec<Hit> = Vec::new();
    for s in sessions {
        for e in &s.events {
            let Some(who) = e.who else { continue };
            let wanted = match o.who {
                WhoFilter::Any => true,
                WhoFilter::User => who == Who::User,
                WhoFilter::Assistant => who == Who::Assistant,
            };
            if !wanted {
                continue;
            }
            if let Some(m) = o.re.find(&e.text) {
                hits.push(Hit {
                    ts: e.ts,
                    who,
                    session: s,
                    snippet: snippet(&e.text, m.start(), m.end()),
                });
            }
        }
    }
    hits.sort_by_key(|h| h.ts);
    let total = hits.len();
    // Past the limit the newest are kept: they are the ones a review is usually after.
    let skipped = total.saturating_sub(o.limit);
    let shown = &hits[skipped..];

    let idw = shown
        .iter()
        .map(|h| h.session.id().len())
        .max()
        .unwrap_or(8);
    let mut out = String::new();
    let mut objects = Vec::new();
    for h in shown {
        let id = h.session.id();
        let _ = writeln!(
            out,
            "{}  {:<9}  {:<6}  {:<idw$}  {}  | {}",
            time::minute(h.ts),
            h.who.name(),
            h.session.source.name(),
            id,
            h.session.project,
            h.snippet
        );
        objects.push(json!({
            "time": time::minute(h.ts),
            "timestamp": time::iso_utc(h.ts),
            "who": h.who.name(),
            "source": h.session.source.name(),
            "id": id,
            "session": h.session.sid,
            "agent": h.session.agent,
            "project": h.session.project,
            "snippet": h.snippet,
        }));
    }
    let note = if total == 0 {
        Some("0 hits".to_string())
    } else if skipped > 0 {
        Some(format!(
            "{total} hits; the newest {} are shown ({skipped} older left out). Narrow with --since, --project or --who, or raise --limit.",
            o.limit
        ))
    } else {
        None
    };
    (out, objects, note)
}

fn is_say(e: &Event) -> bool {
    e.who.is_some()
}

/// How far `ts` is from the wanted range, in seconds; 0 inside it.
fn distance(ts: i64, (lo, hi): (i64, i64)) -> i64 {
    if ts < lo {
        lo - ts
    } else if ts > hi {
        ts - hi
    } else {
        0
    }
}

fn clipped(text: &str, full: bool) -> String {
    let n = text.chars().count();
    if full || n <= SHOW_CAP {
        return text.to_string();
    }
    let head: String = text.chars().take(SHOW_CAP).collect();
    format!("{head}… (+{} characters; --full prints all)", n - SHOW_CAP)
}

pub fn show(s: &Session, o: &ShowOpts) -> Result<(String, Value), String> {
    let says: Vec<usize> = (0..s.events.len())
        .filter(|&i| is_say(&s.events[i]))
        .collect();
    let center: Option<usize> = if let Some(re) = &o.grep {
        // The first utterance that matches.
        let hit = says.iter().position(|&i| re.is_match(&s.events[i].text));
        Some(hit.ok_or_else(|| format!("no utterance in {} matches `{}`", s.id(), re.as_str()))?)
    } else if let Some(at) = o.at {
        // The closest in time; the earliest if several are as close.
        let mut best: Option<(i64, usize)> = None;
        for (k, &i) in says.iter().enumerate() {
            let d = distance(s.events[i].ts, at);
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, k));
            }
        }
        best.map(|(_, k)| k)
    } else {
        None
    };

    // The utterances shown, as positions among the utterances: `k_lo..=k_hi`.
    let (k_lo, k_hi) = match center {
        Some(c) => (
            c.saturating_sub(o.around),
            (c + o.around).min(says.len() - 1),
        ),
        None => (0, says.len().saturating_sub(1)),
    };
    let (from, to) = match center {
        // Tool calls after the last utterance shown belong to it, up to the next one.
        Some(_) => (
            says[k_lo],
            says.get(k_hi + 1).copied().unwrap_or(s.events.len()),
        ),
        None => (0, s.events.len()),
    };

    let mut out = String::new();
    let (t0, t1) = (
        s.events.first().map_or(0, |e| e.ts),
        s.events.last().map_or(0, |e| e.ts),
    );
    let tools = s.events.len() - says.len();
    let _ = write!(
        out,
        "# {}  {}  {}  {} .. {}  ({} utterances, {tools} tool calls)",
        s.id(),
        s.source.name(),
        s.project,
        time::minute(t0),
        time::minute(t1),
        says.len()
    );
    if let Some(c) = center {
        let _ = write!(
            out,
            "; showing #{}..#{} around #{}",
            k_lo + 1,
            k_hi + 1,
            c + 1
        );
    }
    out.push_str("\n\n");

    let mut events = Vec::new();
    let mut n = k_lo;
    for e in &s.events[from..to] {
        match e.who {
            Some(who) => {
                n += 1;
                let mark = if center == Some(n - 1) { ">> " } else { "" };
                let _ = writeln!(
                    out,
                    "{mark}[#{n} {}] {}\n{}\n",
                    time::minute(e.ts),
                    who.name(),
                    clipped(&e.text, o.full)
                );
                events.push(json!({"n": n, "time": time::minute(e.ts), "who": who.name(), "text": clipped(&e.text, o.full)}));
            }
            None => {
                let _ = writeln!(out, "  {}", e.text);
                events.push(json!({"time": time::minute(e.ts), "who": "tool", "text": e.text}));
            }
        }
    }
    let json = json!({
        "source": s.source.name(),
        "id": s.id(),
        "session": s.sid,
        "agent": s.agent,
        "project": s.project,
        "center": center.map(|c| c + 1),
        "events": events,
    });
    Ok((out, json))
}
