// The parsed command is built once per run; its size does not matter.
#![allow(clippy::large_enum_variant)]

mod cache;
mod cli;
mod codex;
mod commands;
mod help;
mod jsonl;
mod lines;
mod load;
mod memory;
mod needle;
mod places;
mod recall;
mod rules;
mod sig;
mod talk;
mod time;
mod timeline;
mod unused;

use cli::Cmd;
use commands::Window;

fn main() {
    // `agentlog search … | head` closes the pipe early; end quietly like other tools.
    // SAFETY: restoring the default action for SIGPIPE, before any thread starts.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let parsed = match cli::parse(std::env::args().skip(1).collect()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("agentlog: {e}");
            std::process::exit(2);
        }
    };
    let c = &parsed.common;
    match &parsed.cmd {
        Cmd::Help(name) => match help::render(name.as_deref()) {
            Ok(text) => {
                print!("{text}");
                return;
            }
            Err(e) => {
                eprintln!("agentlog: {e}");
                std::process::exit(2);
            }
        },
        Cmd::Version => {
            println!("agentlog {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        _ => {}
    }
    let filter = load::Filter {
        since: c.since,
        until: c.until,
        projects: c.projects.clone(),
        excludes: c.excludes.clone(),
    };
    match &parsed.cmd {
        Cmd::Search(o) => return search(c, &filter, o),
        Cmd::Show(session, o) => return show(c, &filter, session, o),
        Cmd::Timeline(o) => return timeline(c, &filter, o),
        Cmd::Memory(sc, o) => return memory(c, sc, o),
        Cmd::Rules(o) => return rules(c, o),
        Cmd::Unused(o) => return unused(c, &filter, o),
        _ => {}
    }
    let log = match load::load(&c.root, &filter) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("agentlog: cannot read {}: {e}", c.root.display());
            std::process::exit(3);
        }
    };
    let w = Window {
        since: c.since,
        until: c.until,
    };
    let (text, json) = match &parsed.cmd {
        Cmd::Repeats(o) => commands::repeats(&log, &w, o),
        Cmd::Tool(spec, o) => commands::tool(&log, &w, spec, o),
        Cmd::Daily(o) => commands::daily(&log, &w, o),
        Cmd::Help(_)
        | Cmd::Version
        | Cmd::Search(_)
        | Cmd::Show(..)
        | Cmd::Timeline(_)
        | Cmd::Memory(..)
        | Cmd::Rules(_)
        | Cmd::Unused(_) => unreachable!(),
    };
    if c.json {
        println!("{json}");
    } else {
        print!("{text}");
    }
}

fn unreadable(what: impl std::fmt::Display, e: std::io::Error) -> ! {
    eprintln!("agentlog: cannot read {what}: {e}");
    std::process::exit(3);
}

/// Codex's roots to read, or `None` to leave Codex out. Asked for alone, there must be
/// some; beside Claude's, a machine without Codex logs is not an error.
fn codex_roots(c: &cli::Common) -> Option<&[std::path::PathBuf]> {
    if !c.source.codex() {
        return None;
    }
    if c.codex_roots.is_empty() {
        if c.source == cli::SourceSel::Codex {
            let home = std::env::var("HOME").unwrap_or_default();
            let place = places::codex_dir(std::path::Path::new(&home)).join("sessions");
            eprintln!(
                "agentlog: no Codex logs: {} does not exist (--codex-root DIR names where they are)",
                place.display()
            );
            std::process::exit(3);
        }
        return None;
    }
    Some(&c.codex_roots)
}

/// Whether to read Claude Code's logs. Beside Codex's (`--source all`), a machine without
/// them is not an error, unless `--root` named the place.
fn claude_on(c: &cli::Common) -> bool {
    c.source.claude()
        && (c.source == cli::SourceSel::Claude
            || c.root_given
            || c.root.is_dir()
            || c.codex_roots.is_empty())
}

/// The sessions to search, from the logs `--source` picks. `pre` turns away Codex messages
/// that cannot match before they are parsed.
fn conversation(
    c: &cli::Common,
    filter: &load::Filter,
    pre: Option<&needle::Prefilter>,
    need: cache::Need,
) -> Vec<talk::Session> {
    let cache = cache_of(c);
    let mut sessions = Vec::new();
    if claude_on(c) {
        sessions.extend(
            talk::load_sessions(&c.root, filter, need, pre, &cache)
                .unwrap_or_else(|e| unreadable(c.root.display(), e)),
        );
    }
    if let Some(roots) = codex_roots(c) {
        sessions.extend(
            codex::load_sessions(roots, filter, need, pre, &cache)
                .unwrap_or_else(|e| unreadable("the Codex logs", e)),
        );
    }
    sessions
}

/// Where what is read from the logs is kept; nowhere with `--raw`.
fn cache_of(c: &cli::Common) -> cache::Cache {
    if c.raw {
        cache::Cache::off()
    } else {
        cache::Cache::standard()
    }
}

fn search(c: &cli::Common, filter: &load::Filter, o: &recall::SearchOpts) {
    let need = cache::Need {
        tools: false,
        cwds: false,
    };
    let sessions = conversation(c, filter, o.prefilter.as_ref(), need);
    let (text, hits, note) = recall::search(&sessions, o);
    if c.json {
        // One object per hit, so the output reads line by line; the note goes aside.
        for h in &hits {
            println!("{h}");
        }
        if let Some(n) = note {
            eprintln!("agentlog: {n}");
        }
    } else {
        print!("{text}");
        if let Some(n) = note {
            println!("{n}");
        }
    }
}

fn memory(c: &cli::Common, sc: &memory::Scope, o: &memory::MemoryOpts) {
    let docs = memory::collect(sc).unwrap_or_else(|e| {
        eprintln!("agentlog: {e}");
        std::process::exit(3);
    });
    let (text, objects, _) = memory::search(&docs, o);
    if c.json {
        for v in &objects {
            println!("{v}");
        }
    } else {
        print!("{text}");
    }
}

fn rules(c: &cli::Common, o: &rules::RulesOpts) {
    match rules::run(o) {
        Ok((text, objects)) => {
            if c.json {
                for v in &objects {
                    println!("{v}");
                }
            } else {
                print!("{text}");
            }
        }
        Err(e) => {
            eprintln!("agentlog: {e}");
            std::process::exit(if e.starts_with("--repo") { 2 } else { 3 });
        }
    }
}

fn unused(c: &cli::Common, filter: &load::Filter, o: &unused::UnusedOpts) {
    let path = std::env::var("PATH").unwrap_or_default();
    let (text, json) =
        unused::run(&c.root, filter, o, &path).unwrap_or_else(|e| unreadable(c.root.display(), e));
    if c.json {
        println!("{json}");
    } else {
        print!("{text}");
    }
}

fn timeline(c: &cli::Common, filter: &load::Filter, o: &timeline::TimelineOpts) {
    let bad = |e: String| -> ! {
        eprintln!("agentlog: {e}");
        std::process::exit(2);
    };
    let home = std::env::var("HOME").unwrap_or_default();
    let repo = timeline::Repo::find(&o.repo, &home).unwrap_or_else(|e| bad(e));
    let commits = timeline::commits(&o.repo, c.since, c.until).unwrap_or_else(|e| bad(e));
    let need = cache::Need {
        tools: false,
        cwds: true,
    };
    let sessions = conversation(c, filter, None, need);
    // --project S picks sessions by name (already done by the filter); without it they are
    // the ones that worked in the repository or one of its worktrees.
    let mine: Vec<&talk::Session> = sessions
        .iter()
        .filter(|s| !filter.projects.is_empty() || repo.owns(s))
        .collect();
    let (text, items, note) = timeline::render(&mine, &commits, o.who);
    if c.json {
        for i in &items {
            println!("{i}");
        }
        if let Some(n) = note {
            eprintln!("agentlog: {n}");
        }
    } else {
        print!("{text}");
        if let Some(n) = note {
            println!("{n}");
            println!(
                "sessions are matched by where they worked: under {}",
                repo.roots.join(", ")
            );
        }
    }
}

fn show(c: &cli::Common, filter: &load::Filter, spec: &str, o: &recall::ShowOpts) {
    let cache = cache_of(c);
    let mut sessions: Vec<talk::Session> = Vec::new();
    if claude_on(c) {
        let files = talk::session_files(&c.root, filter, spec)
            .unwrap_or_else(|e| unreadable(c.root.display(), e));
        sessions.extend(
            files
                .iter()
                .filter_map(|p| talk::load_one(&c.root, p, filter, &cache).ok()),
        );
    }
    // `SESSION/AGENT` names a Claude subagent; a Codex id has no slash.
    if let Some(roots) = codex_roots(c)
        && !spec.contains('/')
    {
        let files = codex::session_files(roots, filter, spec)
            .unwrap_or_else(|e| unreadable("the Codex logs", e));
        sessions.extend(
            files
                .iter()
                .filter_map(|p| codex::load_one(roots, p, filter, &cache).ok()),
        );
    }
    sessions.retain(|s| !s.events.is_empty());
    if sessions.len() != 1 {
        if sessions.is_empty() {
            eprintln!(
                "agentlog: no session starts with `{spec}` under {} (a subagent is `SESSION/AGENT`; --since, --exclude and --include-self apply)",
                c.root.display()
            );
        } else {
            eprintln!(
                "agentlog: `{spec}` fits {} sessions; give more of the id:",
                sessions.len()
            );
            for s in &sessions {
                let first = s.events.first().map_or(0, |e| e.ts);
                eprintln!(
                    "  {}  {}  {}  {}",
                    s.id(),
                    s.source.name(),
                    time::minute(first),
                    s.project
                );
            }
        }
        std::process::exit(2);
    }
    match recall::show(&sessions.remove(0), o) {
        Ok((text, json)) => {
            if c.json {
                println!("{json}");
            } else {
                print!("{text}");
            }
        }
        Err(e) => {
            eprintln!("agentlog: {e}");
            std::process::exit(2);
        }
    }
}
