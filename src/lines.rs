//! Reading a log file line by line from a byte offset, without keeping the lines nobody
//! wants. Claude Code's and Codex's logs are mostly tool output on very long lines; a line's
//! first bytes say whether it is worth reading at all.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use memchr::memchr;

const CHUNK: usize = 4 << 20;
/// How much of a line's start `want` sees.
pub const HEAD: usize = 256;

/// Calls `on_line(number, line)` for each newline-terminated line from byte `from` that
/// `want(number, first bytes)` accepts. A line that is not accepted is passed over without
/// being kept, however long it is. `number` is 0 for the file's first line (when `from` is
/// 0) and counts up from 1 otherwise: only "is this the first line" is meant by it.
///
/// A last line with no newline is a line still being written: it is not passed on, and the
/// returned offset, where the next read should start, is just before it.
pub fn scan_from(
    path: &Path,
    from: u64,
    want: impl Fn(usize, &[u8]) -> bool,
    mut on_line: impl FnMut(usize, &[u8]),
) -> io::Result<u64> {
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(from))?;
    let mut buf: Vec<u8> = Vec::new();
    // Where buf[0] sits in the file, and the end of the last line given or passed over.
    let mut abs = from;
    let mut consumed = from;
    let mut n = usize::from(from > 0);
    let mut skipping = false;
    let head = |b: &[u8]| b.len().min(HEAD);
    loop {
        let got = (&mut file).take(CHUNK as u64).read_to_end(&mut buf)?;
        let mut pos = 0;
        while let Some(i) = memchr(b'\n', &buf[pos..]) {
            let line = &buf[pos..pos + i];
            if skipping {
                skipping = false;
            } else if want(n, &line[..head(line)]) {
                on_line(n, line);
            }
            n += 1;
            pos += i + 1;
            consumed = abs + pos as u64;
        }
        buf.drain(..pos);
        abs += pos as u64;
        if got == 0 {
            return Ok(consumed);
        }
        if skipping || (buf.len() >= HEAD && !want(n, &buf[..HEAD])) {
            skipping = true;
            abs += buf.len() as u64;
            buf.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(path: &Path, from: u64, want: usize) -> (Vec<(usize, String)>, u64) {
        let mut got = Vec::new();
        let end = scan_from(
            path,
            from,
            |_, h| h.len() >= want || !h.is_empty(),
            |n, l| got.push((n, String::from_utf8_lossy(l).into_owned())),
        )
        .unwrap();
        (got, end)
    }

    #[test]
    fn an_unfinished_last_line_is_left_for_the_next_read() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        fs::write(&p, "one\ntwo\nthr").unwrap();
        let (got, end) = collect(&p, 0, 1);
        assert_eq!(got, vec![(0, "one".into()), (1, "two".into())]);
        assert_eq!(end, 8);
        fs::write(&p, "one\ntwo\nthree\nfo").unwrap();
        let (got, end) = collect(&p, end, 1);
        assert_eq!(got, vec![(1, "three".into())]);
        assert_eq!(end, 14);
    }

    #[test]
    fn an_unwanted_long_line_is_skipped_and_the_offset_still_lands_on_a_line_end() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.jsonl");
        let long = "x".repeat(3 * HEAD);
        fs::write(&p, format!("keep\n{long}\nkeep2\n{long}")).unwrap();
        let mut got = Vec::new();
        let end = scan_from(
            &p,
            0,
            |_, h| h.starts_with(b"keep"),
            |_, l| got.push(String::from_utf8_lossy(l).into_owned()),
        )
        .unwrap();
        assert_eq!(got, vec!["keep", "keep2"]);
        // The unfinished long line at the end is not consumed.
        assert_eq!(
            end as usize,
            "keep\n".len() + long.len() + 1 + "keep2\n".len()
        );
    }
}
