//! The command line. Options may come anywhere after the subcommand; a bad one is exit 2
//! with the usage line.

use std::path::PathBuf;

use regex_lite::Regex;

use crate::commands::{DailyOpts, Field, PRESETS, RepeatsOpts, Spec, ToolOpts, preset};
use crate::time;

pub const HELP: &str = include_str!("help.txt");

pub enum Cmd {
    Help,
    Version,
    Repeats(RepeatsOpts),
    Tool(Spec, ToolOpts),
    Daily(DailyOpts),
}

pub struct Common {
    pub since: i64,
    pub until: i64,
    pub projects: Vec<String>,
    pub excludes: Vec<String>,
    pub root: PathBuf,
    pub json: bool,
}

pub struct Parsed {
    pub cmd: Cmd,
    pub common: Common,
}

const USAGE: &str = "usage: agentlog repeats|tool NAME|daily NAME=REGEX... [--since 14d] [--until WHEN] [--project S] [--exclude S] [--json]  (agentlog --help)";

fn regex(flag: &str, s: &str) -> Result<Regex, String> {
    Regex::new(s).map_err(|e| format!("{flag}: not a regex: {e}"))
}

fn number(flag: &str, s: &str) -> Result<usize, String> {
    s.parse()
        .map_err(|_| format!("{flag}: expected a number, got `{s}`"))
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
    };
    let mut pos: Vec<String> = Vec::new();
    let mut top = 30;
    let mut examples = 0;
    let mut all = false;
    let mut tool_filter: Option<String> = None;
    let mut field = Field::Any;
    let mut failed_only = false;
    let mut include_self = false;
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
    // Run from a Claude Code session, leave that session out: its own calls (this
    // analysis) are not what is being measured.
    if !include_self
        && let Ok(id) = std::env::var("CLAUDE_CODE_SESSION_ID")
        && !id.is_empty()
    {
        common.excludes.push(id);
    }
    if common.since >= common.until {
        return Err("--since must be before --until".into());
    }

    let Some(sub) = pos.first().cloned() else {
        return Ok(Parsed {
            cmd: Cmd::Help,
            common,
        });
    };
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
        other => return Err(format!("unknown command `{other}`\n{USAGE}")),
    };
    Ok(Parsed { cmd, common })
}
