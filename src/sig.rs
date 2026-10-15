//! A failure's signature: the tool, for Bash the program it ran, and the first line that
//! says what went wrong, with paths, ids and numbers flattened so that the same mistake in
//! another file or another session reads the same.

use std::sync::LazyLock;

use regex_lite::Regex;

use crate::load::Call;

static ERROR_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(error|not found|no such file|failed|\bfail\b|traceback|eaddrinuse|already in use|timed out|timeout|denied|refused|cannot|can't|unknown|invalid|unexpected|fatal|conflict|rejected|missing|blocked|refusing|no matches|illegal|usage:)",
    )
    .unwrap()
});
static LEAD: LazyLock<Regex> = LazyLock::new(|| {
    // cd X && / cd X; / VAR=x / timeout N / rtk proxy, before the program itself
    Regex::new(r"^\s*(?:cd\s+[^;&|]+(?:&&|;)\s*|[A-Za-z_][A-Za-z0-9_]*=\S*\s+|timeout\s+\d+[smh]?\s+|rtk\s+proxy\s+)*").unwrap()
});
static PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"~?[A-Za-z0-9_.@-]*(?:/[A-Za-z0-9_.@-]+)+/?").unwrap());
static ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[0-9a-f]{7,}\b").unwrap());
static DIGITS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+").unwrap());
static SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static TAGS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"</?tool_use_error>").unwrap());

/// Programs whose first argument says more than the program does.
const TWO_WORDS: &[&str] = &[
    "npm", "npx", "pnpm", "yarn", "bun", "git", "gh", "docker", "kubectl", "cargo", "go", "uv",
    "node", "python", "python3", "devsrv", "orca", "aws", "wrangler", "brew", "claude",
];

pub fn program(cmd: &str) -> String {
    let rest = &cmd[LEAD.find(cmd).map_or(0, |m| m.end())..];
    let mut words = rest.split_whitespace();
    let Some(first) = words.next() else {
        return "?".into();
    };
    let base = first.rsplit('/').next().unwrap_or(first);
    if TWO_WORDS.contains(&base)
        && let Some(second) = words.next().filter(|w| !w.starts_with('<') && w.len() < 30)
    {
        return format!("{base} {second}");
    }
    base.chars().take(30).collect()
}

pub fn normalize(line: &str) -> String {
    let s = TAGS.replace_all(line, "");
    let s = PATH.replace_all(&s, "<path>");
    let s = ID.replace_all(&s, "<id>");
    let s = DIGITS.replace_all(&s, "N");
    let s = SPACE.replace_all(s.trim(), " ");
    s.chars().take(110).collect()
}

/// The user said no or stopped it: not a mistake of the agent.
pub fn by_user(call: &Call) -> bool {
    let r = call.result.trim_start_matches("<tool_use_error>");
    r.starts_with("The user doesn't want to")
        || r.starts_with("Error calling tool (Bash): interrupt")
        || r.starts_with("[Request interrupted by user")
}

/// A Bash call that ran and exited non-zero. Other failed Bash calls never ran: the
/// harness or a hook refused them, and their message is the same whatever the program.
fn ran(call: &Call) -> bool {
    call.tool == "Bash" && call.result.starts_with("Exit code ")
}

/// The line that says what went wrong, raw. For a command that ran: the first line that
/// reads like an error, else the first line of output (output that differs every time
/// then stays a signature of one, and a terse error such as `rpatch: line 3: a FROM
/// part needs …` still groups), else `Exit code N`.
pub fn error_line(call: &Call) -> &str {
    let mut lines = call.result.lines().filter(|l| !l.trim().is_empty());
    let first = lines.next().unwrap_or("");
    if ran(call) {
        let rest: Vec<&str> = lines.collect();
        return rest
            .iter()
            .find(|l| ERROR_LINE.is_match(l))
            .or(rest.first())
            .copied()
            .unwrap_or(first);
    }
    first
}

pub fn signature(call: &Call) -> String {
    let line = normalize(error_line(call));
    let line = if line.is_empty() {
        "(no output)".into()
    } else {
        line
    };
    if ran(call) {
        format!("Bash {} | {line}", program(&call.input))
    } else {
        format!("{} | {line}", call.tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn program_skips_cd_env_and_timeout() {
        assert_eq!(
            program("cd /a/b && FOO=1 timeout 30 npm test 2>&1"),
            "npm test"
        );
        assert_eq!(program("/usr/bin/rpatch <<'EOF'"), "rpatch");
        assert_eq!(program("rtk proxy docker logs x"), "docker logs");
    }

    #[test]
    fn same_mistake_in_other_files_reads_the_same() {
        let a = bash(
            "rpatch <<'EOF'",
            "Exit code 1\nM a.rs\nerror: edits[0] src/a.rs (input line 3): \"old\" not found\n",
        );
        let b = bash(
            "cd /x && rpatch <<'EOF'",
            "Exit code 1\nerror: edits[4] docs/plan.md (input line 51): \"old\" not found\n",
        );
        assert_eq!(signature(&a), signature(&b));
        assert_eq!(
            signature(&a),
            "Bash rpatch | error: edits[N] <path> (input line N): \"old\" not found"
        );
    }

    #[test]
    fn non_bash_uses_first_line() {
        let c = Call {
            ctx: 0,
            ts: 0,
            tool: "Edit".into(),
            input: "/a/b.rs".into(),
            failed: true,
            result:
                "<tool_use_error>String to replace not found in file.\nString: x</tool_use_error>"
                    .into(),
        };
        assert_eq!(signature(&c), "Edit | String to replace not found in file.");
    }

    #[test]
    fn refusals_group_whatever_the_program() {
        let msg = "This agent is isolated in the worktree /w/a1, but this command is too complex";
        let a = bash("cd /w/a1 && git status", msg);
        let b = bash("rpatch <<'EOF'", msg);
        assert_eq!(signature(&a), signature(&b));
        assert!(signature(&a).starts_with("Bash | This agent is isolated"));
    }

    #[test]
    fn terse_error_without_a_keyword_still_groups() {
        let a = bash(
            "rpatch <<'EOF'",
            "Exit code 2\nrpatch: line 19: a FROM part needs `<<<<<<< TO`",
        );
        let b = bash(
            "rpatch <<'EOF'",
            "Exit code 2\nrpatch: line 4: a FROM part needs `<<<<<<< TO`",
        );
        assert_eq!(signature(&a), signature(&b));
        let bare = bash("false", "Exit code 1");
        assert_eq!(signature(&bare), "Bash false | Exit code N");
    }
}
