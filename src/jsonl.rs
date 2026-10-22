//! One line of a session log as JSON. Claude Code writes a string cut in the middle of an
//! emoji (a tool's output cut to a length) with half of a UTF-16 surrogate pair, `\ud83d`,
//! which is valid in JavaScript but not to serde_json: such a line, and the call or the
//! words in it, would be lost. The half is read as U+FFFD instead.

use serde_json::Value;

pub fn parse(line: &[u8]) -> Option<Value> {
    if let Ok(v) = serde_json::from_slice(line) {
        return Some(v);
    }
    let fixed = without_lone_surrogates(line)?;
    serde_json::from_slice(&fixed).ok()
}

fn hex4(b: &[u8]) -> Option<u16> {
    let s = std::str::from_utf8(b.get(..4)?).ok()?;
    u16::from_str_radix(s, 16).ok()
}

/// The line with each `\uD800`-`\uDFFF` escape that is not part of a pair written as
/// `�`; `None` if there is none (the line is bad some other way).
fn without_lone_surrogates(line: &[u8]) -> Option<Vec<u8>> {
    let high = |u: u16| (0xD800..0xDC00).contains(&u);
    let low = |u: u16| (0xDC00..0xE000).contains(&u);
    let mut out = Vec::with_capacity(line.len());
    let mut changed = false;
    let mut i = 0;
    while i < line.len() {
        if line[i] != b'\\' || i + 1 >= line.len() {
            out.push(line[i]);
            i += 1;
            continue;
        }
        if line[i + 1] != b'u' {
            // `\\`, `\"`, `\n`…: two bytes, so that `\\ud83d` stays text
            out.extend_from_slice(&line[i..i + 2]);
            i += 2;
            continue;
        }
        let Some(u) = hex4(&line[i + 2..]) else {
            out.push(line[i]);
            i += 1;
            continue;
        };
        let paired = high(u)
            && line.get(i + 6..i + 8) == Some(b"\\u")
            && hex4(&line[i + 8..]).is_some_and(low);
        if paired {
            out.extend_from_slice(&line[i..i + 12]);
            i += 12;
        } else if high(u) || low(u) {
            out.extend_from_slice(b"\\ufffd");
            changed = true;
            i += 6;
        } else {
            out.extend_from_slice(&line[i..i + 6]);
            i += 6;
        }
    }
    changed.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &str) -> Option<String> {
        parse(line.as_bytes())?
            .get("t")?
            .as_str()
            .map(str::to_string)
    }

    #[test]
    fn half_of_a_surrogate_pair_reads_as_a_replacement_character() {
        assert_eq!(
            text(r#"{"t":"cut \ud83d"}"#).as_deref(),
            Some("cut \u{fffd}")
        );
        assert_eq!(
            text(r#"{"t":"\ude00 end"}"#).as_deref(),
            Some("\u{fffd} end")
        );
        // a whole pair beside a half, and an escaped backslash before `ud83d`, stay as they are
        let pair = concat!(r#"{"t":""#, "\\ud83d", "\\ude00", r#" \ud83d"}"#);
        assert_eq!(text(pair).as_deref(), Some("\u{1f600} \u{fffd}"));
        assert_eq!(
            text(r#"{"t":"\\ud83d \ud83d"}"#).as_deref(),
            Some("\\ud83d \u{fffd}")
        );
        assert_eq!(text(r#"{"t":"half"#), None);
    }
}
