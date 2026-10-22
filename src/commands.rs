//! The three reports. Each returns the text to print and the same data as JSON.
//!
//! Commands and results can hold credentials, so the text shows counts and flattened
//! error lines; the command text itself appears only with --examples.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::sync::OnceLock;

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
    /// Going another way is an escape only on a file the failed call was editing: the
    /// files of both calls are read (`written`) and compared.
    pub same_file: bool,
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
            same_file: true,
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
            same_file: false,
        }),
        _ => None,
    }
}

// Which files a call edits, to tell going another way on the file that failed from work on
// another file. Only the plain forms are read; a path in a variable is not known.

struct FileRes {
    /// A program at the start of a command: `rpatch`, `sed`, `perl`, `sd`.
    program: Regex,
    /// `cd DIR` at the start: relative paths after it are under DIR.
    cd: Regex,
    /// rpatch: `=== FILE`, `"file": "FILE"`, and the file of `error: edits[N] FILE (…)`.
    block: Regex,
    json: Regex,
    edits: Regex,
    /// `cat > FILE`, `>| FILE`.
    cat: Regex,
    /// python: `open('F', 'w')`, `Path('F').write_text`, and any `Path('F')`.
    open: Regex,
    write_path: Regex,
    path: Regex,
}

fn file_res() -> &'static FileRes {
    static RE: OnceLock<FileRes> = OnceLock::new();
    // A string literal, also inside `python3 -c "…"` where its quotes are escaped.
    const LIT: &str = r#"\\?['"]([^'"\\]+)\\?['"]"#;
    RE.get_or_init(|| FileRes {
        program: re(r"(?m)(?:^|[;&|(])\s*(?:\S*/)?(rpatch|sed|perl|sd)[ \t]"),
        cd: re(r"^\s*cd\s"),
        block: re(r"(?m)^===[ \t]+(.+?)[ \t]*$"),
        json: re(r#""file"\s*:\s*"([^"\\]+)""#),
        edits: re(r"edits\[\d+\] (\S.*?)(?: \(|: )"),
        cat: re(r"(?:(?:^|[\s;&|(])cat\s+>\|?|>\|)\s*"),
        open: re(&format!(
            r#"open\(\s*[rbuf]*{LIT}\s*,\s*[rbuf]*\\?['"][^'"]*[wa]"#
        )),
        write_path: re(&format!(
            r"Path\(\s*[rbuf]*{LIT}\s*\)\s*\.write_(?:text|bytes)"
        )),
        path: re(&format!(r"Path\(\s*[rbuf]*{LIT}\s*\)")),
    })
}

/// The words of one shell command at the start of `s`, up to `&&`, `|`, `;`, a newline, a
/// redirection or `)` outside quotes. An empty quoted word (`''`) is kept.
fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                quoted = true;
                cur.extend(chars.by_ref().take_while(|&c| c != '\''));
            }
            '"' => {
                quoted = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => cur.extend(chars.next()),
                        _ => cur.push(c),
                    }
                }
            }
            // a line continued with `\` goes on
            '\\' => cur.extend(chars.next().filter(|&c| c != '\n')),
            ' ' | '\t' => {
                if !cur.is_empty() || quoted {
                    out.push(std::mem::take(&mut cur));
                    quoted = false;
                }
            }
            '\n' | ';' | '&' | '|' | '<' | '>' | ')' => {
                // the 2 of `2>&1` is not a word
                if "<>".contains(ch) && !quoted && cur.bytes().all(|b| b.is_ascii_digit()) {
                    cur.clear();
                }
                break;
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() || quoted {
        out.push(cur);
    }
    out
}

/// `sed -i [SUFFIX] [-e SCRIPT]... [SCRIPT] FILE...`: the files, or none without -i.
fn sed_files(args: &[String]) -> Vec<String> {
    let (mut in_place, mut script, mut pos) = (false, false, Vec::new());
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        if a == "-i" || a == "--in-place" {
            in_place = true;
            // BSD: `-i ''` or `-i .bak`; GNU's `-i 's/…/'` is the script
            if it
                .peek()
                .is_some_and(|n| n.is_empty() || n.starts_with('.'))
            {
                it.next();
            }
        } else if a.starts_with("-i") || a.starts_with("--in-place=") {
            in_place = true;
        } else if a == "-e" || a == "-f" || a == "--expression" || a == "--file" {
            it.next();
            script = true;
        } else if a.starts_with("--expression=") || a.starts_with("--file=") {
            script = true;
        } else if a.len() > 1 && a.starts_with('-') && !a.starts_with("--") {
            in_place |= a.contains('i');
            if a.ends_with('e') || a.ends_with('f') {
                it.next();
                script = true;
            }
        } else if !a.starts_with("--") {
            pos.push(a.clone());
        }
    }
    if !in_place {
        return Vec::new();
    }
    pos.into_iter().skip(usize::from(!script)).collect()
}

/// `perl -pi -e SCRIPT FILE...`: everything after `i` in a switch is its backup suffix.
fn perl_files(args: &[String]) -> Vec<String> {
    let (mut in_place, mut script, mut pos) = (false, false, Vec::new());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a.len() > 1 && a.starts_with('-') && !a.starts_with("--") {
            let flags = &a[1..];
            let before_i = flags.find('i').map_or(flags, |k| &flags[..k]);
            in_place |= before_i.len() < flags.len();
            if before_i.contains(['e', 'E']) {
                it.next();
                script = true;
            }
        } else if !a.starts_with("--") {
            pos.push(a.clone());
        }
    }
    if !in_place {
        return Vec::new();
    }
    pos.into_iter().skip(usize::from(!script)).collect()
}

/// `sd [OPTIONS] FIND REPLACE FILE...`: in a pipe it has no files; `-p` writes nothing.
fn sd_files(args: &[String]) -> Vec<String> {
    let mut pos = Vec::new();
    let mut it = args.iter();
    let mut options = true;
    while let Some(a) = it.next() {
        if !options || !a.starts_with('-') || a == "-" {
            pos.push(a.clone());
        } else if a == "--" {
            options = false;
        } else if a == "-p" || a == "--preview" {
            return Vec::new();
        } else if ["-f", "--flags", "-n", "--max-replacements"].contains(&a.as_str()) {
            it.next();
        }
    }
    pos.into_iter().skip(2).collect()
}

/// `rpatch [OPTIONS] FILE <<'EOF'`: the file given as an argument.
fn rpatch_files(args: &[String]) -> Vec<String> {
    let mut options = true;
    for a in args {
        if options && a == "--" {
            options = false;
        } else if !(options && a.starts_with('-')) {
            return vec![a.clone()];
        }
    }
    Vec::new()
}

/// `~/` is the home directory, a relative path is under `dir` when it is known, and `.`,
/// `..` and `/private/tmp` (macOS's `/tmp`) are written out.
fn normal_path(p: &str, dir: Option<&str>) -> String {
    let mut s = p.trim().to_string();
    if let Some(rest) = s.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        s = format!("{home}/{rest}");
    }
    if let (false, Some(d)) = (s.starts_with('/'), dir) {
        s = format!("{d}/{s}");
    }
    let abs = s.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|l| *l != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    if abs && matches!(parts.as_slice(), ["private", "tmp" | "var", ..]) {
        parts.remove(0);
    }
    let joined = parts.join("/");
    if abs { format!("/{joined}") } else { joined }
}

/// The files a call writes (`target`: or the files a failed call of the tool was going to
/// write, read from its command and its error). Empty: not known.
fn written(c: &Call, target: bool) -> Vec<String> {
    if matches!(
        c.tool.as_str(),
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit"
    ) {
        return vec![normal_path(&c.input, None)];
    }
    if c.tool != "Bash" {
        return Vec::new();
    }
    let r = file_res();
    let cmd = c.input.as_str();
    let dir =
        r.cd.find(cmd)
            .and_then(|m| words(&cmd[m.end()..]).into_iter().next());
    let dir = dir.map(|d| normal_path(&d, None));
    let mut files: Vec<String> = Vec::new();
    for m in r.program.captures_iter(cmd) {
        let (prog, at) = (&m[1], m.get(0).unwrap().end());
        let args = words(&cmd[at..]);
        files.extend(match (prog, target) {
            ("rpatch", true) => rpatch_files(&args),
            ("sed", false) => sed_files(&args),
            ("perl", false) => perl_files(&args),
            ("sd", false) => sd_files(&args),
            _ => Vec::new(),
        });
    }
    if target {
        files.extend(r.block.captures_iter(cmd).map(|m| m[1].to_string()));
        files.extend(r.json.captures_iter(cmd).map(|m| m[1].to_string()));
        files.extend(r.edits.captures_iter(&c.result).map(|m| m[1].to_string()));
    } else {
        for m in r.cat.find_iter(cmd) {
            files.extend(words(&cmd[m.end()..]).into_iter().next());
        }
        if cmd.contains("python") {
            files.extend(r.open.captures_iter(cmd).map(|m| m[1].to_string()));
            files.extend(r.write_path.captures_iter(cmd).map(|m| m[1].to_string()));
            // `p = Path('F')` … `p.write_text(…)`
            if files.is_empty() && cmd.contains(".write_") {
                files.extend(r.path.captures_iter(cmd).map(|m| m[1].to_string()));
            }
        }
    }
    // a variable or a glob is not a path we can compare
    files.retain(|f| !f.is_empty() && !f.contains(['$', '`', '*']));
    let mut files: Vec<String> = files
        .iter()
        .map(|f| normal_path(f, dir.as_deref()))
        .collect();
    files.sort();
    files.dedup();
    files
}

/// Two spellings of one file: the same path, or a relative path that is the end of the other.
fn same_path(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short == long || (!short.starts_with('/') && long.ends_with(&format!("/{short}")))
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Same,
    Another,
    Unknown,
}

fn target_of(failed: &[&Call], next: &Call) -> Target {
    let aimed: Vec<String> = failed.iter().flat_map(|c| written(c, true)).collect();
    let wrote = written(next, false);
    // `../x` with no `cd` before it is under a directory we do not know: it may be any file
    // named x one level up, so it cannot be told apart from the other path.
    let above = |p: &String| p == ".." || p.starts_with("../");
    if aimed.is_empty() || wrote.is_empty() {
        Target::Unknown
    } else if wrote.iter().any(|w| aimed.iter().any(|a| same_path(a, w))) {
        Target::Same
    } else if aimed.iter().chain(&wrote).any(above) {
        Target::Unknown
    } else {
        Target::Another
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
    let mut another_file: BTreeMap<String, usize> = BTreeMap::new();
    let mut other_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut escape_ex: Vec<(i64, String, String, String)> = Vec::new();
    let mut another_ex: Vec<(i64, String, String, String)> = Vec::new();
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
            // The run: failed uses, and between them work on another file than the ones
            // that failed (not an escape: it is noted and passed over).
            let mut failures: Vec<&Call> = Vec::new();
            let mut k = i;
            loop {
                match evs.get(k) {
                    Some(Ev::Use {
                        call, failed: true, ..
                    }) => failures.push(call),
                    Some(Ev::Alt { call, label, .. })
                        if spec.same_file && target_of(&failures, call) == Target::Another =>
                    {
                        let f = failures[failures.len() - 1];
                        *another_file.entry(label.clone()).or_default() += 1;
                        another_ex.push((
                            f.ts,
                            log.ctxs[ctx].label.clone(),
                            fail_line(spec, f),
                            format!("{label}: {}", head(&call.input, 90)),
                        ));
                    }
                    _ => break,
                }
                k += 1;
            }
            let n = failures.len();
            let outcome = match evs.get(k) {
                None => "context ended after it".to_string(),
                Some(Ev::Use { .. }) if n == 1 => "fixed on the next call".to_string(),
                Some(Ev::Use { .. }) => format!("fixed after {n} failures in a row"),
                Some(Ev::Alt { call, label, .. }) => {
                    let target = spec.same_file.then(|| target_of(&failures, call));
                    let f = failures[n - 1];
                    let after = if n == 1 {
                        String::new()
                    } else {
                        format!(" after {n} failures")
                    };
                    let (tag, outcome) = match target {
                        None => ("", format!("went another way{after}")),
                        Some(Target::Same) => (
                            "  (same file)",
                            format!("went another way on the same file{after}"),
                        ),
                        // another file was passed over above
                        Some(Target::Unknown | Target::Another) => (
                            "  (file not known)",
                            format!("went another way, file not known{after}"),
                        ),
                    };
                    *escapes.entry(label.clone()).or_default() += 1;
                    escape_ex.push((
                        f.ts,
                        log.ctxs[ctx].label.clone(),
                        fail_line(spec, f),
                        format!("{label}: {}{tag}", head(&call.input, 90)),
                    ));
                    outcome
                }
            };
            *outcomes.entry(outcome).or_default() += 1;
            if n >= 2 {
                let lines = failures.iter().map(|c| fail_line(spec, c)).collect();
                streak_ex.push((failures[0].ts, log.ctxs[ctx].label.clone(), lines));
            }
            i = k;
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
    if !another_file.is_empty() {
        let list: Vec<String> = another_file
            .iter()
            .map(|(k, v)| format!("{k} {v}"))
            .collect();
        let _ = writeln!(out, "    on another file: {}", list.join(", "));
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
        for (title, list) in [
            ("went another way", &escape_ex),
            ("another way on another file (not an escape)", &another_ex),
        ] {
            if list.is_empty() {
                continue;
            }
            let _ = writeln!(out, "\n  {title}");
            for (t, ctx, err, next) in list.iter().take(o.examples) {
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
        "after_failure": outcomes, "went_to": escapes, "another_file": another_file,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn bash(cmd: &str, result: &str) -> Call {
        Call {
            ctx: 0,
            ts: 0,
            tool: "Bash".into(),
            input: cmd.into(),
            failed: true,
            result: result.into(),
        }
    }

    #[test]
    fn words_end_with_the_command_and_keep_an_empty_quoted_word() {
        assert_eq!(
            words(" -i '' 's/a b/c/' \"d e.txt\" 2>&1 | head"),
            s(&["-i", "", "s/a b/c/", "d e.txt"])
        );
        assert_eq!(words(" a \\\n  b; c"), s(&["a", "b"]));
    }

    #[test]
    fn sed_perl_and_sd_name_the_files_they_change_in_place() {
        assert_eq!(sed_files(&s(&["-i", "", "s/a/b/", "f"])), s(&["f"]));
        assert_eq!(sed_files(&s(&["-i", "s/a/b/", "f", "g"])), s(&["f", "g"]));
        assert_eq!(sed_files(&s(&["-i.bak", "-e", "s/a/b/", "f"])), s(&["f"]));
        assert_eq!(sed_files(&s(&["-n", "1,5p", "f"])), s(&[]));
        assert_eq!(
            perl_files(&s(&["-0777", "-pi", "-e", "s/a/b/", "f"])),
            s(&["f"])
        );
        assert_eq!(perl_files(&s(&["-pe", "s/a/b/", "f"])), s(&[]));
        assert_eq!(sd_files(&s(&["-F", "a", "b", "f"])), s(&["f"]));
        assert_eq!(sd_files(&s(&["a", "b"])), s(&[]));
        assert_eq!(sd_files(&s(&["-p", "a", "b", "f"])), s(&[]));
        assert_eq!(rpatch_files(&s(&["--diff", "--", "-f.rs"])), s(&["-f.rs"]));
    }

    #[test]
    fn paths_are_compared_after_cd_and_by_the_end_of_a_full_path() {
        let c = bash("cd /w && sed -i '' s/a/b/ ./src/../src/a.rs", "");
        assert_eq!(written(&c, false), s(&["/w/src/a.rs"]));
        assert!(same_path("src/a.rs", "/w/src/a.rs"));
        assert!(!same_path("a.rs", "/w/ba.rs"));
        assert!(!same_path("/v/src/a.rs", "/w/src/a.rs"));
        assert_eq!(normal_path("/private/tmp/x", None), "/tmp/x");
    }

    #[test]
    fn a_failed_rpatch_names_its_files_in_the_command_and_the_error() {
        let c = bash(
            "rpatch <<'EOF'\n=== a.rs\n=== ~/b.rs\nEOF",
            "error: edits[0] c.rs (input line 2): \"old\" not found",
        );
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(
            written(&c, true),
            s(&[&format!("{home}/b.rs"), "a.rs", "c.rs"])
        );
        // a python script that writes through a variable names the Path it was made from
        let p = bash("python3 -c \"open(\\\"o.txt\\\", \\\"w\\\")\"", "");
        assert_eq!(written(&p, false), s(&["o.txt"]));
        assert_eq!(written(&bash("cat > \"$d/x\" <<'EOF'", ""), false), s(&[]));
    }
}
