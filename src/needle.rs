//! A cheap test on a raw log line, run before the line is read as JSON: does the line hold
//! a string that every match of the search pattern must contain? Codex logs are big,
//! mostly tool output, so the lines that cannot match are never parsed.
//!
//! The test may let a line through that does not match (the regex decides later). It must
//! never turn away a line that does. So only plain characters that JSON writes as they are
//! count: a `"`, a backslash, a newline or a tab is escaped inside a log line, and `\s`,
//! `.`, `[^x]` stand for a different number of characters there than in the decoded text.
//! A pattern with nothing safe to look for (`\d+`, `.*`) gets no test, and every message
//! is read.

use memchr::{memchr, memchr2, memmem};

/// Alternatives: a line may match when it holds any one of them.
pub struct Prefilter {
    alts: Vec<Needle>,
    /// A needle with a non-ASCII character misses a line that wrote it as `\uXXXX`
    /// (rare, but possible); such lines are let through.
    escapes_possible: bool,
}

enum Needle {
    Exact(memmem::Finder<'static>),
    /// ASCII case-insensitive: the lowercased bytes, and the byte position to look for.
    Folded {
        lower: Vec<u8>,
        pivot: usize,
    },
}

impl Prefilter {
    /// `None` when some alternative of the pattern has no safe literal.
    pub fn new(pattern: &str, ignore_case: bool) -> Option<Prefilter> {
        if pattern.contains("(?") {
            // Inline flags can change what the rest of the pattern means.
            return None;
        }
        let mut alts = Vec::new();
        let mut escapes_possible = false;
        for alt in split_alternatives(pattern) {
            let lit = longest_literal(&alt, ignore_case)?;
            escapes_possible |= !lit.is_ascii();
            alts.push(
                if ignore_case && lit.bytes().any(|b| b.is_ascii_alphabetic()) {
                    let lower = lit.to_ascii_lowercase().into_bytes();
                    let pivot = rarest(&lower);
                    Needle::Folded { lower, pivot }
                } else {
                    Needle::Exact(memmem::Finder::new(lit.as_bytes()).into_owned())
                },
            );
        }
        if alts.is_empty() {
            return None;
        }
        Some(Prefilter {
            alts,
            escapes_possible,
        })
    }

    pub fn may_match(&self, line: &[u8]) -> bool {
        self.alts.iter().any(|n| n.found(line))
            || self.escapes_possible && memmem::find(line, b"\\u").is_some()
    }
}

impl Needle {
    fn found(&self, line: &[u8]) -> bool {
        match self {
            Needle::Exact(f) => f.find(line).is_some(),
            Needle::Folded { lower, pivot } => folded_find(line, lower, *pivot),
        }
    }
}

/// Position of the byte that is the least common in text: a non-letter if there is one,
/// else the letter latest in a rough most-to-least-common order.
fn rarest(lower: &[u8]) -> usize {
    const ORDER: &[u8] = b"etaoinshrdlcumwfgypbvkjxqz";
    let rank = |b: u8| match b {
        b' ' => 0,
        b if !b.is_ascii() => 3,
        b => ORDER
            .iter()
            .position(|&c| c == b)
            .unwrap_or(ORDER.len() + 1),
    };
    (0..lower.len())
        .max_by_key(|&i| rank(lower[i]))
        .unwrap_or(0)
}

fn folded_find(line: &[u8], lower: &[u8], pivot: usize) -> bool {
    let p = lower[pivot];
    let mut from = pivot;
    while from < line.len() {
        let hit = if p.is_ascii_alphabetic() {
            memchr2(p, p.to_ascii_uppercase(), &line[from..])
        } else {
            memchr(p, &line[from..])
        };
        let Some(i) = hit else { return false };
        let at = from + i;
        let start = at - pivot;
        if line.len() >= start + lower.len()
            && line[start..start + lower.len()].eq_ignore_ascii_case(lower)
        {
            return true;
        }
        from = at + 1;
    }
    false
}

/// The pattern cut at its top-level `|`.
fn split_alternatives(pattern: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let (mut depth, mut in_class) = (0usize, false);
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let cur = out.last_mut().unwrap();
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
                continue;
            }
            '[' if !in_class => in_class = true,
            ']' if in_class => in_class = false,
            '(' if !in_class => depth += 1,
            ')' if !in_class => depth = depth.saturating_sub(1),
            '|' if !in_class && depth == 0 => {
                out.push(String::new());
                continue;
            }
            _ => {}
        }
        out.last_mut().unwrap().push(c);
    }
    out
}

/// Whether a character may stand in a needle: it is written as it is in a log line, and
/// (case-insensitively) it has no other spelling.
fn plain(c: char, ignore_case: bool) -> bool {
    if c == '"' || c == '\\' || c.is_control() {
        return false;
    }
    !(ignore_case && !c.is_ascii() && c.to_lowercase().ne(c.to_uppercase()))
}

/// The longest run of characters that every match of `alt` contains, outside groups and
/// classes. `None` if there is none.
fn longest_literal(alt: &str, ignore_case: bool) -> Option<String> {
    let mut best = String::new();
    let mut run = String::new();
    let flush = |run: &mut String, best: &mut String| {
        if run.chars().count() > best.chars().count() {
            *best = run.clone();
        }
        run.clear();
    };
    let chars: Vec<char> = alt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => {
                let Some(&n) = chars.get(i) else { break };
                i += 1;
                if n.is_ascii_punctuation() && plain(n, ignore_case) {
                    run.push(n);
                } else {
                    flush(&mut run, &mut best);
                }
            }
            '[' => {
                // Skip the class: `[]a]` and `[^]a]` take a `]` first.
                if chars.get(i) == Some(&'^') {
                    i += 1;
                }
                if chars.get(i) == Some(&']') {
                    i += 1;
                }
                while i < chars.len() && chars[i] != ']' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                flush(&mut run, &mut best);
            }
            '(' => {
                let mut depth = 1;
                while i < chars.len() && depth > 0 {
                    match chars[i] {
                        '\\' => i += 1,
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                flush(&mut run, &mut best);
            }
            '*' | '?' => {
                // The character before may be absent.
                run.pop();
                flush(&mut run, &mut best);
            }
            '{' => {
                // `{n,m}`: the character before may be absent when n is 0; drop it either way.
                run.pop();
                while i < chars.len() && chars[i] != '}' {
                    i += 1;
                }
                i += 1;
                flush(&mut run, &mut best);
            }
            '+' => flush(&mut run, &mut best),
            '.' | '^' | '$' | ')' | ']' | '}' => flush(&mut run, &mut best),
            c if plain(c, ignore_case) => run.push(c),
            _ => flush(&mut run, &mut best),
        }
    }
    flush(&mut run, &mut best);
    (!best.is_empty()).then_some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(p: &str, ic: bool) -> Option<String> {
        split_alternatives(p)
            .iter()
            .map(|a| longest_literal(a, ic))
            .collect::<Option<Vec<_>>>()
            .map(|v| v.join("|"))
    }

    #[test]
    fn the_needle_is_what_every_match_must_contain() {
        assert_eq!(lit("Planconfig", false).as_deref(), Some("Planconfig"));
        assert_eq!(lit("foo.*barbaz", false).as_deref(), Some("barbaz"));
        assert_eq!(lit("colou?r check", false).as_deref(), Some("r check"));
        assert_eq!(lit("ab{0,2}cd", false).as_deref(), Some("cd"));
        assert_eq!(lit("abc+defg", false).as_deref(), Some("defg"));
        assert_eq!(lit(r"a\.b", false).as_deref(), Some("a.b"));
        assert_eq!(lit("(x|y)zzz", false).as_deref(), Some("zzz"));
        assert_eq!(lit("疎にしすぎ", false).as_deref(), Some("疎にしすぎ"));
        assert_eq!(lit("alpha|beta", false).as_deref(), Some("alpha|beta"));
        assert_eq!(lit("[ab]c", false).as_deref(), Some("c"));
    }

    #[test]
    fn what_json_escapes_or_a_wildcard_may_stretch_is_never_in_a_needle() {
        // A newline is `\n` in a log line, a quote `\"`, a tab `\t`.
        assert_eq!(lit(r"alpha\sbeta", false).as_deref(), Some("alpha"));
        assert_eq!(lit("say \"hi\" now", false).as_deref(), Some("say "));
        assert_eq!(lit("a\tb", false).as_deref(), Some("a"));
        assert_eq!(lit(r"a\\b", false).as_deref(), Some("a"));
        // `\d+` and `.*` leave nothing to look for.
        assert_eq!(lit(r"\d+", false), None);
        assert_eq!(lit(".*", false), None);
        // One alternative without a literal means no test at all.
        assert_eq!(lit(r"abc|\d+", false), None);
        assert!(Prefilter::new("(?i)abc", false).is_none());
    }

    #[test]
    fn folded_search_ignores_ascii_case_only() {
        let p = Prefilter::new("Too Loose", true).unwrap();
        assert!(p.may_match(b"{\"text\":\"it is TOO loose here\"}"));
        assert!(!p.may_match(b"{\"text\":\"too tight\"}"));
        let q = Prefilter::new("Too Loose", false).unwrap();
        assert!(!q.may_match(b"too loose"));
        assert!(q.may_match(b"Too Loose"));
    }

    #[test]
    fn a_non_ascii_needle_lets_escaped_lines_through() {
        let p = Prefilter::new("疎", false).unwrap();
        assert!(p.may_match("疎にしすぎ".as_bytes()));
        assert!(p.may_match(b"\\u758e escaped"));
        assert!(!p.may_match(b"plain"));
        let a = Prefilter::new("abc", false).unwrap();
        assert!(!a.may_match(b"\\u758e escaped"));
    }
}
