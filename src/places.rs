//! Where Claude Code and Codex keep their files, as they decide it: Claude Code's folder is
//! `$CLAUDE_CONFIG_DIR` when it is set (its `.claude.json` is then in there too), else
//! `~/.claude` and `~/.claude.json`; Codex's is `$CODEX_HOME`, else `~/.codex`.

use std::path::{Path, PathBuf};

fn var(k: &str) -> Option<PathBuf> {
    std::env::var_os(k)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `~/.claude`: projects (the logs and memories), settings, skills, plugins, CLAUDE.md.
pub fn claude_dir(home: &Path) -> PathBuf {
    var("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude"))
}

/// `~/.claude.json`: the MCP servers and the claude.ai connectors.
pub fn claude_json(home: &Path) -> PathBuf {
    var("CLAUDE_CONFIG_DIR")
        .map(|d| d.join(".claude.json"))
        .unwrap_or_else(|| home.join(".claude.json"))
}

/// `~/.codex`: sessions, archived_sessions, memories, AGENTS.md.
pub fn codex_dir(home: &Path) -> PathBuf {
    var("CODEX_HOME").unwrap_or_else(|| home.join(".codex"))
}
