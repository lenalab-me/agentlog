// The parsed command is built once per run; its size does not matter.
#![allow(clippy::large_enum_variant)]

mod cli;
mod commands;
mod load;
mod recall;
mod sig;
mod talk;
mod time;

use cli::Cmd;
use commands::Window;

fn main() {
    let parsed = match cli::parse(std::env::args().skip(1).collect()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("agentlog: {e}");
            std::process::exit(2);
        }
    };
    let c = &parsed.common;
    match &parsed.cmd {
        Cmd::Help => {
            print!("{}", cli::HELP);
            return;
        }
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
        Cmd::Help | Cmd::Version | Cmd::Search(_) | Cmd::Show(..) => unreachable!(),
    };
    if c.json {
        println!("{json}");
    } else {
        print!("{text}");
    }
}

fn unreadable(c: &cli::Common, e: std::io::Error) -> ! {
    eprintln!("agentlog: cannot read {}: {e}", c.root.display());
    std::process::exit(3);
}

fn search(c: &cli::Common, filter: &load::Filter, o: &recall::SearchOpts) {
    let sessions = talk::load_sessions(&c.root, filter, false).unwrap_or_else(|e| unreadable(c, e));
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

fn show(c: &cli::Common, filter: &load::Filter, spec: &str, o: &recall::ShowOpts) {
    let files = talk::session_files(&c.root, filter, spec).unwrap_or_else(|e| unreadable(c, e));
    let mut sessions: Vec<talk::Session> = files
        .iter()
        .filter_map(|p| talk::load_one(&c.root, p, filter).ok())
        .filter(|s| !s.events.is_empty())
        .collect();
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
                eprintln!("  {}  {}  {}", s.id(), time::minute(first), s.project);
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
