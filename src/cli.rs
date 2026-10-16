//! The command line. Options may come anywhere after the subcommand; a bad one is exit 2
//! with the usage line.

use std::path::PathBuf;

use regex_lite::{Regex, RegexBuilder};

use crate::codex;
use crate::commands::{DailyOpts, Field, PRESETS, RepeatsOpts, Spec, ToolOpts, preset};
use crate::needle::Prefilter;
use crate::recall::{SearchOpts, ShowOpts, WhoFilter};
use crate::time;

pub const HELP: &str = include_str!("help.txt");

pub enum Cmd {
    Help,
    Version,
    Repeats(RepeatsOpts),
    Tool(Spec, ToolOpts),
    Daily(DailyOpts),
    Search(SearchOpts),
    /// The session (an id or its start) and what to show of it.
    Show(String, ShowOpts),
}

/// `--source`: which agents' logs `search` and `show` read.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SourceSel {
    Claude,
    Codex,
    All,
}

impl SourceSel {
    pub fn claude(self) -> bool {
        self != SourceSel::Codex
    }
    pub fn codex(self) -> bool {
        self != SourceSel::Claude
    }
}

pub struct Common {
    pub since: i64,
    pub until: i64,
    pub projects: Vec<String>,
    pub excludes: Vec<String>,
    pub root: PathBuf,
    pub json: bool,
    pub source: SourceSel,
    /// Where Codex's logs are. Unless given, the default places that exist.
    pub codex_roots: Vec<PathBuf>,
    pub codex_roots_given: bool,
}

pub struct Parsed {
    pub cmd: Cmd,
    pub common: Common,
}

const USAGE: &str = "usage: agentlog repeats|tool NAME|daily NAME=REGEX...|search REGEX|show SESSION [--since WHEN] [--until WHEN] [--project S] [--exclude S] [--json]  (agentlog --help)";

fn regex(flag: &str, s: &str) -> Result<Regex, String> {
    Regex::new(s).map_err(|e| format!("{flag}: not a regex: {e}"))
}

fn number(flag: &str, s: &str) -> Result<usize, String> {
    s.parse()
        .map_err(|_| format!("{flag}: expected a number, got `{s}`"))
}

/// `--at`: a local time (or one with a zone). A time given to the minute means that
/// minute, a date alone the whole day.
fn at_range(v: &str) -> Option<(i64, i64)> {
    let t = time::parse_iso(v)?;
    let clock = v.trim().split(['T', ' ']).nth(1);
    let span = match clock {
        None => 86399,
        Some(c) => match c
            .split(['Z', '+', '-'])
            .next()
            .unwrap_or("")
            .matches(':')
            .count()
        {
            0 | 1 => 59,
            _ => 0,
        },
    };
    Some((t, t + span))
}

pub fn parse(args: Vec<String>) -> Result<Parsed, String> {
    let now = time::now();
    let home = std::env::var("HOME").unwrap_or_default();
    let mut common = Common {
        since: now - 14 * 86400,
        until: i64::MAX,
        projects: Vec::new(),
        excludes: Vec::new(),
        root: PathBuf::from(format!("{home}/.claude/projects")),
        json: false,
        source: SourceSel::All,
        codex_roots: Vec::new(),
        codex_roots_given: false,
    };
    let mut source_given = false;
    let mut pos: Vec<String> = Vec::new();
    let mut top = 30;
    let mut examples = 0;
    let mut all = false;
    let mut tool_filter: Option<String> = None;
    let mut field = Field::Any;
    let mut failed_only = false;
    let mut include_self = false;
    let mut since_given = false;
    let mut ignore_case = false;
    let mut who = WhoFilter::Any;
    let mut limit = 50;
    let mut grep: Option<String> = None;
    let mut at: Option<(i64, i64)> = None;
    let mut around = 5;
    let mut full = false;
    let (mut uses, mut fail, mut alt, mut escape, mut ignore) = (None, None, None, None, None);
    let mut alt_tools: Option<Vec<String>> = None;
    let mut escape_tools: Option<Vec<String>> = None;
    let list = |s: String| -> Vec<String> {
        s.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        let value = |it: &mut std::vec::IntoIter<String>| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next())
                .ok_or_else(|| format!("{flag} needs a value\n{USAGE}"))
        };
        match flag.as_str() {
            "-h" | "--help" | "help" if pos.is_empty() => {
                return Ok(Parsed {
                    cmd: Cmd::Help,
                    common,
                });
            }
            "-V" | "--version" => {
                return Ok(Parsed {
                    cmd: Cmd::Version,
                    common,
                });
            }
            "--json" => common.json = true,
            "--since" => {
                since_given = true;
                let v = value(&mut it)?;
                common.since = time::parse_when(&v, now).ok_or_else(|| {
                    format!("--since: expected 2026-10-07, 2026-10-07T08:57, …Z, or 14d; got `{v}`")
                })?;
            }
            "--until" => {
                let v = value(&mut it)?;
                common.until = time::parse_when(&v, now).ok_or_else(|| {
                    format!("--until: expected 2026-10-07, 2026-10-07T08:57, …Z, or 14d; got `{v}`")
                })?;
            }
            "--project" => common.projects.push(value(&mut it)?),
            "--exclude" => common.excludes.push(value(&mut it)?),
            "--include-self" => include_self = true,
            "--root" => common.root = PathBuf::from(value(&mut it)?),
            "--codex-root" => {
                common.codex_roots.push(PathBuf::from(value(&mut it)?));
                common.codex_roots_given = true;
            }
            "--source" => {
                source_given = true;
                common.source = match value(&mut it)?.as_str() {
                    "claude" => SourceSel::Claude,
                    "codex" => SourceSel::Codex,
                    "all" => SourceSel::All,
                    v => {
                        return Err(format!(
                            "--source: expected claude, codex or all; got `{v}`"
                        ));
                    }
                }
            }
            "--top" => top = number("--top", &value(&mut it)?)?,
            "--examples" => examples = number("--examples", &value(&mut it)?)?,
            "-v" => examples = examples.max(3),
            "--all" => all = true,
            "--tool" => tool_filter = Some(value(&mut it)?),
            "--in" => {
                field = match value(&mut it)?.as_str() {
                    "input" | "cmd" | "command" => Field::Input,
                    "result" | "output" => Field::Result,
                    "any" => Field::Any,
                    v => return Err(format!("--in: expected input, result or any; got `{v}`")),
                }
            }
            "--failed" => failed_only = true,
            "-i" => ignore_case = true,
            "--full" => full = true,
            "--who" => {
                who = match value(&mut it)?.as_str() {
                    "user" => WhoFilter::User,
                    "assistant" => WhoFilter::Assistant,
                    "any" => WhoFilter::Any,
                    v => return Err(format!("--who: expected user, assistant or any; got `{v}`")),
                }
            }
            "--limit" => limit = number("--limit", &value(&mut it)?)?,
            "--around" => around = number("--around", &value(&mut it)?)?,
            "--grep" => grep = Some(value(&mut it)?),
            "--at" => {
                let v = value(&mut it)?;
                at = Some(at_range(&v).ok_or_else(|| {
                    format!(
                        "--at: expected 2026-10-07T08:57 (local) or 2026-10-07 08:57; got `{v}`"
                    )
                })?);
            }
            "--" => {
                pos.extend(it.by_ref());
                break;
            }
            "--uses" => uses = Some(regex("--uses", &value(&mut it)?)?),
            "--fail" => fail = Some(regex("--fail", &value(&mut it)?)?),
            "--alt" => alt = Some(regex("--alt", &value(&mut it)?)?),
            "--escape" => escape = Some(regex("--escape", &value(&mut it)?)?),
            "--ignore" => ignore = Some(regex("--ignore", &value(&mut it)?)?),
            "--alt-tool" => alt_tools = Some(list(value(&mut it)?)),
            "--escape-tool" => escape_tools = Some(list(value(&mut it)?)),
            f if f.starts_with('-') && f.len() > 1 && !f.contains('=') => {
                return Err(format!("unknown option `{f}`\n{USAGE}"));
            }
            _ => pos.push(a),
        }
    }
    let Some(sub) = pos.first().cloned() else {
        return Ok(Parsed {
            cmd: Cmd::Help,
            common,
        });
    };
    // Looking back for a conversation may reach far; the other reports want recent calls.
    if (sub == "search" || sub == "show") && !since_given {
        common.since = 0;
    }
    // Run from a Claude Code session, leave that session out: its own calls (this
    // analysis) are not what is being measured. `show` names its session, so it is not
    // left out there.
    if !include_self
        && sub != "show"
        && let Ok(id) = std::env::var("CLAUDE_CODE_SESSION_ID")
        && !id.is_empty()
    {
        common.excludes.push(id);
    }
    if common.since >= common.until {
        return Err("--since must be before --until".into());
    }
    if (source_given || common.codex_roots_given) && sub != "search" && sub != "show" {
        return Err(format!(
            "--source and --codex-root are for search and show; {sub} reads Claude Code's logs only"
        ));
    }
    if !common.codex_roots_given {
        common.codex_roots = codex::default_roots()
            .into_iter()
            .filter(|p| p.is_dir())
            .collect();
    }

    let rest = &pos[1..];
    let cmd = match sub.as_str() {
        "repeats" => {
            if !rest.is_empty() {
                return Err(format!(
                    "repeats takes no arguments, got `{}`\n{USAGE}",
                    rest.join(" ")
                ));
            }
            Cmd::Repeats(RepeatsOpts {
                top,
                examples,
                tool: tool_filter,
                all,
            })
        }
        "tool" => {
            let [name] = rest else {
                return Err(format!(
                    "tool takes one NAME (a preset: {}, or a tool name, or any name with --uses)\n{USAGE}",
                    PRESETS.join(", ")
                ));
            };
            let mut spec = preset(name).unwrap_or(Spec {
                name: name.clone(),
                uses: None,
                fail: None,
                alt: None,
                escape: None,
                alt_tools: Vec::new(),
                escape_tools: Vec::new(),
                ignore: None,
            });
            if uses.is_some() {
                spec.uses = uses;
            }
            if fail.is_some() {
                spec.fail = fail;
            }
            if alt.is_some() {
                spec.alt = alt;
            }
            if escape.is_some() {
                spec.escape = escape;
            }
            if ignore.is_some() {
                spec.ignore = ignore;
            }
            if let Some(t) = alt_tools {
                spec.alt_tools = t;
            }
            if let Some(t) = escape_tools {
                spec.escape_tools = t;
            }
            Cmd::Tool(spec, ToolOpts { examples })
        }
        "daily" => {
            if rest.is_empty() {
                return Err(format!("daily needs one or more NAME=REGEX\n{USAGE}"));
            }
            let mut patterns = Vec::new();
            for p in rest {
                let Some((n, r)) = p.split_once('=') else {
                    return Err(format!("daily: expected NAME=REGEX, got `{p}`"));
                };
                patterns.push((n.to_string(), regex(n, r)?));
            }
            Cmd::Daily(DailyOpts {
                patterns,
                field,
                tool: tool_filter,
                failed_only,
            })
        }
        "search" => {
            let [pattern] = rest else {
                return Err(format!(
                    "search takes one REGEX (put `--` before one that starts with `-`)\n{USAGE}"
                ));
            };
            let re = RegexBuilder::new(pattern)
                .case_insensitive(ignore_case)
                .build()
                .map_err(|e| format!("search: not a regex: {e}"))?;
            Cmd::Search(SearchOpts {
                re,
                who,
                limit,
                prefilter: Prefilter::new(pattern, ignore_case),
            })
        }
        "show" => {
            let [session] = rest else {
                return Err(format!(
                    "show takes one SESSION (an id or its first characters, as search prints it)\n{USAGE}"
                ));
            };
            if grep.is_some() && at.is_some() {
                return Err("show: use --grep or --at, not both".into());
            }
            let grep = grep
                .map(|g| {
                    RegexBuilder::new(&g)
                        .case_insensitive(ignore_case)
                        .build()
                        .map_err(|e| format!("--grep: not a regex: {e}"))
                })
                .transpose()?;
            Cmd::Show(
                session.clone(),
                ShowOpts {
                    grep,
                    at,
                    around,
                    full,
                },
            )
        }
        other => return Err(format!("unknown command `{other}`\n{USAGE}")),
    };
    Ok(Parsed { cmd, common })
}
