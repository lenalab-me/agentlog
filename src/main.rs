// The parsed command is built once per run; its size does not matter.
#![allow(clippy::large_enum_variant)]

mod cli;
mod commands;
mod load;
mod sig;
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
        Cmd::Help | Cmd::Version => unreachable!(),
    };
    if c.json {
        println!("{json}");
    } else {
        print!("{text}");
    }
}
