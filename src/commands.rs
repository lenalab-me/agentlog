//! The three reports. Each returns the text to print and the same data as JSON.
//!
//! Commands and results can hold credentials, so the text shows counts and flattened
//! error lines; the command text itself appears only with --examples.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

use regex_lite::Regex;
use serde_json::{Value, json};

use crate::load::{Call, Log};
use crate::sig;
use crate::time;

pub struct Window {
    pub since: i64,
    pub until: i64,
}

fn head(s: &str, n: usize) -> String {
    let one: String = s.replace('\n', " ⏎ ");
    let mut out: String = one.chars().take(n).collect();
    if one.chars().count() > n {
        out.push('…');
    }
    out
}

fn short_time(t: i64) -> String {
    time::minute(t)[5..].to_string()
}

fn header(log: &Log, w: &Window) -> String {
    let failed = log.calls.iter().filter(|c| c.failed).count();
    format!(
        "{} .. {} (local): {} calls, {} failed, in {} contexts (a subagent is its own context)\n",
        time::minute(w.since),
        time::minute(w.until.min(time::now())),
        log.calls.len(),
        failed,
        log.ctxs.len()
    )
}

// ---------------------------------------------------------------------------------------
// repeats

pub struct RepeatsOpts {
    pub top: usize,
    pub examples: usize,
    pub tool: Option<String>,
    pub all: bool,
}

#[derive(Default)]
struct SigStats {
    total: usize,
    per_ctx: HashMap<usize, usize>,
    streak: usize,
    days: BTreeSet<String>,
    last: i64,
    calls: Vec<usize>,
}

impl SigStats {
    fn rep(&self) -> usize {
        self.per_ctx.values().filter(|&&n| n >= 2).count()
    }
}

pub fn repeats(log: &Log, w: &Window, o: &RepeatsOpts) -> (String, Value) {
    let mut stats: HashMap<String, SigStats> = HashMap::new();
    // The last failure per (context, tool), to see the same one again with no pass between.
    let mut last: HashMap<(usize, &str), (String, usize)> = HashMap::new();
    for (i, c) in log.calls.iter().enumerate() {
        if o.tool.as_deref().is_some_and(|t| t != c.tool) {
            continue;
        }
        let key = (c.ctx, c.tool.as_str());
        if c.failed && sig::by_user(c) {
            continue;
        }
        if !c.failed {
            last.remove(&key);
            continue;
        }
        let s = sig::signature(c);
        let run = match last.get(&key) {
            Some((prev, n)) if *prev == s => n + 1,
            _ => 1,
        };
        last.insert(key, (s.clone(), run));
        let st = stats.entry(s).or_default();
        st.total += 1;
        *st.per_ctx.entry(c.ctx).or_default() += 1;
        st.streak = st.streak.max(run);
        st.days.insert(time::day(c.ts));
        st.last = st.last.max(c.ts);
        st.calls.push(i);
    }

    let mut rows: Vec<(String, SigStats)> = stats
        .into_iter()
        .filter(|(_, s)| o.all || s.rep() >= 1 || s.per_ctx.len() >= 2)
        .collect();
    rows.sort_by(|(an, a), (bn, b)| {
        (b.rep(), b.per_ctx.len(), b.total, an).cmp(&(a.rep(), a.per_ctx.len(), a.total, bn))
    });
    let shown = rows.len().min(o.top);

    let mut out = header(log, w);
    out.push_str(
        "The same failure again. rep: contexts that had it twice or more. ctx: contexts that had it.\n\
         streak: longest run of it on one tool with no passing call of that tool between.\n\n",
    );
    let _ = writeln!(
        out,
        "{:>4} {:>4} {:>5} {:>6} {:>4}  {:<11}  signature",
        "rep", "ctx", "total", "streak", "days", "last"
    );
    let mut items = Vec::new();
    for (name, s) in rows.iter().take(shown) {
        let _ = writeln!(
            out,
            "{:>4} {:>4} {:>5} {:>6} {:>4}  {:<11}  {name}",
            s.rep(),
            s.per_ctx.len(),
            s.total,
            s.streak,
            s.days.len(),
            short_time(s.last)
        );
        let mut ex = Vec::new();
        if o.examples > 0 {
            // Prefer calls from contexts that repeated it.
            let mut picks: Vec<usize> = s.calls.clone();
            picks.sort_by_key(|&i| std::cmp::Reverse(s.per_ctx[&log.calls[i].ctx]));
            for &i in picks.iter().take(o.examples) {
                let c = &log.calls[i];
                let _ = writeln!(
                    out,
                    "        {}  {}  {}",
                    short_time(c.ts),
                    log.ctxs[c.ctx].label,
                    head(&c.input, 100)
                );
                ex.push(json!({"time": time::minute(c.ts), "context": log.ctxs[c.ctx].label, "input": c.input}));
            }
        }
        items.push(json!({
            "signature": name, "rep": s.rep(), "ctx": s.per_ctx.len(), "total": s.total,
            "streak": s.streak, "days": s.days.len(), "last": time::minute(s.last), "examples": ex,
        }));
    }
    if rows.len() > shown {
        let _ = writeln!(out, "\n{} more (--top N to see them)", rows.len() - shown);
    }
    if rows.is_empty() {
        out.push_str("(no failure happened twice)\n");
    }
    let j = json!({
        "since": time::minute(w.since), "until": time::minute(w.until.min(time::now())),
        "calls": log.calls.len(), "contexts": log.ctxs.len(),
        "signatures": items, "more": rows.len() - shown,
    });
    (out, j)
}

// ---------------------------------------------------------------------------------------
// tool

pub struct Spec {
    pub name: String,
    /// A Bash command that uses the tool. None: calls whose tool name is `name`.
    pub uses: Option<Regex>,
    /// The tool itself failed (not a later command in the same chain). None: any failure.
    pub fail: Option<Regex>,
    /// A Bash command doing the job some other way.
    pub alt: Option<Regex>,
    /// A Bash command that counts only right after a failure, like escape_tools.
    pub escape: Option<Regex>,
    /// Tool calls doing the job some other way.
    pub alt_tools: Vec<String>,
    /// Tool calls that count only right after a failure: Write makes new files all day,
    /// but rewriting the file the tool just failed on is going another way.
    pub escape_tools: Vec<String>,
    /// An `alt` match that does not count (writing a new scratch file is not an escape).
    pub ignore: Option<Regex>,
}

pub const PRESETS: &[&str] = &["rpatch", "devsrv"];

fn re(s: &str) -> Regex {
    Regex::new(s).expect("preset regex")
}

pub fn preset(name: &str) -> Option<Spec> {
    match name {
        "rpatch" => Some(Spec {
            name: name.into(),
            uses: Some(re(r"(^|[\s;&|(])rpatch(\s|$)")),
            fail: Some(re(r"(?m)^(rpatch: |error: edits\[)")),
            // Changing a file in place, as a command of its own (sd in a pipe edits no file).
            // The python script of a heredoc is on the lines after the command.
            alt: Some(re(
                r#"(?:^|\n|[;&(])\s*(?:python3?\s(?s:.*?)(?:\.replace\(|write_text|open\([^)]*['"][wa]['"])|sd\s|sed\s+-i|perl\s+-\w*i)"#,
            )),
            // Writing a whole file: new files are made this way all day.
            escape: Some(re(r"(^|[\s;&|(])cat\s+>\|?\s*[^\s&>]|>\|")),
            alt_tools: vec!["Edit".into(), "MultiEdit".into()],
            escape_tools: vec!["Write".into()],
            ignore: Some(re(r">\|?\s*/(private/)?tmp/|>\s*/dev/null")),
        }),
        "devsrv" => Some(Spec {
            name: name.into(),
            uses: Some(re(
                r"(^|[\s;&|(])devsrv\s+(start|stop|ls|logs|who|wait|restart)\b",
            )),
            fail: Some(re(r"(?m)^devsrv: ")),
            // `&` alone puts a server in the background; `&&`, `>&` and `&>` do not.
            alt: Some(re(
                r"\blsof\s+-\w*i|\bpkill\b|\bnohup\s|\bkill\s+(-\w+\s+)?(\$\(|\d)|(?:http\.server|npm\s+run\s+dev|(?:^|[\s/])vite(?:\s|$))[^\n]*[^&>]&(?:[^&>]|$)|(?:^|[\s;&(])until\s[^\n]*\bcurl\b",
            )),
            escape: None,
            alt_tools: vec![],
            escape_tools: vec![],
            ignore: None,
        }),
        _ => None,
    }
}

pub struct ToolOpts {
    pub examples: usize,
}

enum Ev<'a> {
    Use {
        call: &'a Call,
        failed: bool,
    },
    /// `counted`: also counted per day as another way (false for escape_tools).
    Alt {
        call: &'a Call,
        label: String,
        counted: bool,
    },
}

fn classify<'a>(spec: &Spec, c: &'a Call) -> Option<Ev<'a>> {
    let used = match &spec.uses {
        Some(r) => c.tool == "Bash" && r.is_match(&c.input),
        None => c.tool == spec.name,
    };
    if used {
        let failed = c.failed && spec.fail.as_ref().is_none_or(|r| r.is_match(&c.result));
        return Some(Ev::Use { call: c, failed });
    }
    let alt_tool = spec.alt_tools.contains(&c.tool);
    if alt_tool || spec.escape_tools.contains(&c.tool) {
        return Some(Ev::Alt {
            call: c,
            label: c.tool.clone(),
            counted: alt_tool,
        });
    }
    if c.tool != "Bash" {
        return None;
    }
    let (m, counted) = match spec.alt.as_ref().and_then(|r| r.find(&c.input)) {
        Some(m) => (Some(m), true),
        None => (spec.escape.as_ref().and_then(|r| r.find(&c.input)), false),
    };
    if let Some(m) = m {
        if spec.ignore.as_ref().is_some_and(|r| r.is_match(&c.input)) {
            return None;
        }
        // The program alone: `kill 123` and `kill $(…)` are one kind.
        let label: String = m
            .as_str()
            .trim_start_matches(|ch: char| " \n;&|(/".contains(ch))
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || "._-".contains(*ch))
            .collect();
        return Some(Ev::Alt {
            call: c,
            label: format!("Bash {label}"),
            counted,
        });
    }
    None
}

/// The line saying why the tool failed: the one `fail` matched, else the usual error line.
fn fail_line(spec: &Spec, c: &Call) -> String {
    let line = spec
        .fail
        .as_ref()
        .and_then(|r| r.find(&c.result))
        .map(|m| {
            let start = c.result[..m.start()].rfind('\n').map_or(0, |i| i + 1);
            let end = c.result[m.start()..]
                .find('\n')
                .map_or(c.result.len(), |i| m.start() + i);
            &c.result[start..end]
        })
        .unwrap_or_else(|| sig::error_line(c));
    sig::normalize(line)
}

pub fn tool(log: &Log, w: &Window, spec: &Spec, o: &ToolOpts) -> (String, Value) {
    #[derive(Default)]
    struct Day {
        uses: usize,
        failed: usize,
        alt: usize,
    }
    let mut days: BTreeMap<String, Day> = BTreeMap::new();
    let mut first_use: Option<i64> = None;
    let mut uses = 0;
    let mut failed = 0;
    let mut alt_total = 0;
    let mut alt_before = 0;
    let mut outcomes: BTreeMap<String, usize> = BTreeMap::new();
    let mut escapes: BTreeMap<String, usize> = BTreeMap::new();
    let mut other_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut escape_ex: Vec<(i64, String, String, String)> = Vec::new();
    let mut streak_ex: Vec<(i64, String, Vec<String>)> = Vec::new();

    let mut by_ctx: Vec<Vec<Ev>> = (0..log.ctxs.len()).map(|_| Vec::new()).collect();
    for c in &log.calls {
        if let Some(ev) = classify(spec, c) {
            by_ctx[c.ctx].push(ev);
        }
    }
    // first use, to split the alternatives into before and after
    for evs in &by_ctx {
        for ev in evs {
            if let Ev::Use { call, .. } = ev {
                first_use = Some(first_use.map_or(call.ts, |t: i64| t.min(call.ts)));
            }
        }
    }
    for (ctx, evs) in by_ctx.iter().enumerate() {
        for ev in evs {
            match ev {
                Ev::Use { call, failed: f } => {
                    let d = days.entry(time::day(call.ts)).or_default();
                    d.uses += 1;
                    uses += 1;
                    if *f {
                        d.failed += 1;
                        failed += 1;
                    }
                }
                Ev::Alt { counted: false, .. } => {}
                Ev::Alt { call, label, .. } => {
                    days.entry(time::day(call.ts)).or_default().alt += 1;
                    *other_kinds.entry(label.clone()).or_default() += 1;
                    alt_total += 1;
                    if first_use.is_none_or(|t| call.ts < t) {
                        alt_before += 1;
                    }
                }
            }
        }
        // runs of failed uses, and what came next
        let mut i = 0;
        while i < evs.len() {
            let Ev::Use { failed: true, .. } = evs[i] else {
                i += 1;
                continue;
            };
            let mut j = i;
            while matches!(evs.get(j + 1), Some(Ev::Use { failed: true, .. })) {
                j += 1;
            }
            let n = j - i + 1;
            let outcome = match evs.get(j + 1) {
                None => "context ended after it".to_string(),
                Some(Ev::Use { .. }) if n == 1 => "fixed on the next call".to_string(),
                Some(Ev::Use { .. }) => format!("fixed after {n} failures in a row"),
                Some(Ev::Alt { call, label, .. }) => {
                    *escapes.entry(label.clone()).or_default() += 1;
                    let Ev::Use { call: f, .. } = &evs[j] else {
                        unreachable!()
                    };
                    escape_ex.push((
                        f.ts,
                        log.ctxs[ctx].label.clone(),
                        fail_line(spec, f),
                        format!("{label}: {}", head(&call.input, 90)),
                    ));
                    if n == 1 {
                        "went another way".to_string()
                    } else {
                        format!("went another way after {n} failures")
                    }
                }
            };
            *outcomes.entry(outcome).or_default() += 1;
            if n >= 2 {
                let lines = evs[i..=j]
                    .iter()
                    .filter_map(|e| match e {
                        Ev::Use { call, .. } => Some(fail_line(spec, call)),
                        _ => None,
                    })
                    .collect();
                if let Ev::Use { call, .. } = &evs[i] {
                    streak_ex.push((call.ts, log.ctxs[ctx].label.clone(), lines));
                }
            }
            i = j + 1;
        }
    }

    let mut out = header(log, w);
    let _ = writeln!(out, "\n{}", spec.name);
    match first_use {
        Some(t) => {
            let _ = writeln!(out, "  first use in the window: {}", time::minute(t));
        }
        None => {
            let _ = writeln!(out, "  not used in the window");
        }
    }
    let pct = if uses > 0 {
        failed as f64 * 100.0 / uses as f64
    } else {
        0.0
    };
    let _ = writeln!(out, "  uses: {uses}, failed: {failed} ({pct:.1}%)");
    let until = w.until.min(time::now());
    if let Some(t) = first_use {
        let before_days = (t - w.since) as f64 / 86400.0;
        let after_days = (until - t) as f64 / 86400.0;
        if before_days >= 1.0 && after_days > 0.0 {
            let _ = writeln!(
                out,
                "  other ways per day: {:.1} before the first use ({before_days:.1} days), {:.1} after ({after_days:.1} days)",
                alt_before as f64 / before_days,
                (alt_total - alt_before) as f64 / after_days
            );
        }
    }
    if !other_kinds.is_empty() {
        let mut kinds: Vec<(&String, &usize)> = other_kinds.iter().collect();
        kinds.sort_by(|a, b| b.1.cmp(a.1));
        let list: Vec<String> = kinds.iter().map(|(k, v)| format!("{k} {v}")).collect();
        let _ = writeln!(out, "  other ways: {}", list.join(", "));
    }
    let runs: usize = outcomes.values().sum();
    let _ = writeln!(
        out,
        "\n  after a failure ({runs} runs of failures; other calls between are skipped)"
    );
    let mut ordered: Vec<(&String, &usize)> = outcomes.iter().collect();
    ordered.sort_by(|a, b| b.1.cmp(a.1));
    for (k, v) in &ordered {
        let _ = writeln!(out, "    {v:>5}  {k}");
    }
    if !escapes.is_empty() {
        let list: Vec<String> = escapes.iter().map(|(k, v)| format!("{k} {v}")).collect();
        let _ = writeln!(out, "    went to: {}", list.join(", "));
    }
    let _ = writeln!(
        out,
        "\n  {:<10}  {:>5}  {:>6}  {:>5}",
        "day", "uses", "failed", "other"
    );
    for (d, x) in &days {
        let mark = if first_use.is_some_and(|t| time::day(t) == *d) {
            "  <- first use"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {d:<10}  {:>5}  {:>6}  {:>5}{mark}",
            x.uses, x.failed, x.alt
        );
    }
    if o.examples > 0 {
        if !streak_ex.is_empty() {
            let _ = writeln!(out, "\n  failures in a row");
            for (t, ctx, lines) in streak_ex.iter().take(o.examples) {
                let _ = writeln!(out, "    {}  {ctx}", short_time(*t));
                for l in lines {
                    let _ = writeln!(out, "        {l}");
                }
            }
        }
        if !escape_ex.is_empty() {
            let _ = writeln!(out, "\n  went another way");
            for (t, ctx, err, next) in escape_ex.iter().take(o.examples) {
                let _ = writeln!(
                    out,
                    "    {}  {ctx}\n        {err}\n        => {next}",
                    short_time(*t)
                );
            }
        }
    }

    let j = json!({
        "tool": spec.name,
        "since": time::minute(w.since), "until": time::minute(until),
        "first_use": first_use.map(time::minute),
        "uses": uses, "failed": failed,
        "other_ways": {"total": alt_total, "before_first_use": alt_before, "kinds": other_kinds},
        "after_failure": outcomes, "went_to": escapes,
        "days": days.iter().map(|(d, x)| json!({"day": d, "uses": x.uses, "failed": x.failed, "other": x.alt})).collect::<Vec<_>>(),
    });
    (out, j)
}

// ---------------------------------------------------------------------------------------
// daily

#[derive(Clone, Copy, PartialEq)]
pub enum Field {
    Input,
    Result,
    Any,
}

pub struct DailyOpts {
    pub patterns: Vec<(String, Regex)>,
    pub field: Field,
    pub tool: Option<String>,
    pub failed_only: bool,
}

pub fn daily(log: &Log, w: &Window, o: &DailyOpts) -> (String, Value) {
    let mut table: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut totals = vec![0usize; o.patterns.len()];
    for c in &log.calls {
        if o.tool.as_deref().is_some_and(|t| t != c.tool) || (o.failed_only && !c.failed) {
            continue;
        }
        for (k, (_, r)) in o.patterns.iter().enumerate() {
            let hit = match o.field {
                Field::Input => r.is_match(&c.input),
                Field::Result => r.is_match(&c.result),
                Field::Any => r.is_match(&c.input) || r.is_match(&c.result),
            };
            if hit {
                table
                    .entry(time::day(c.ts))
                    .or_insert_with(|| vec![0; o.patterns.len()])[k] += 1;
                totals[k] += 1;
            }
        }
    }
    let widths: Vec<usize> = o
        .patterns
        .iter()
        .map(|(n, _)| n.chars().count().max(5))
        .collect();
    let mut out = header(log, w);
    out.push('\n');
    let _ = write!(out, "{:<10}", "day");
    for ((n, _), wd) in o.patterns.iter().zip(&widths) {
        let _ = write!(out, "  {:>wd$}", n, wd = wd);
    }
    out.push('\n');
    for (d, row) in &table {
        let _ = write!(out, "{d:<10}");
        for (v, wd) in row.iter().zip(&widths) {
            let _ = write!(
                out,
                "  {:>wd$}",
                if *v == 0 {
                    ".".to_string()
                } else {
                    v.to_string()
                },
                wd = wd
            );
        }
        out.push('\n');
    }
    let _ = write!(out, "{:<10}", "total");
    for (v, wd) in totals.iter().zip(&widths) {
        let _ = write!(out, "  {v:>wd$}", wd = wd);
    }
    out.push('\n');
    let j = json!({
        "since": time::minute(w.since), "until": time::minute(w.until.min(time::now())),
        "patterns": o.patterns.iter().map(|(n, r)| json!({"name": n, "regex": r.as_str()})).collect::<Vec<_>>(),
        "days": table.iter().map(|(d, row)| {
            let mut m = serde_json::Map::new();
            m.insert("day".into(), json!(d));
            for ((n, _), v) in o.patterns.iter().zip(row) {
                m.insert(n.clone(), json!(v));
            }
            Value::Object(m)
        }).collect::<Vec<_>>(),
        "total": o.patterns.iter().zip(&totals).map(|((n, _), v)| (n.clone(), json!(v))).collect::<serde_json::Map<_, _>>(),
    });
    (out, j)
}
