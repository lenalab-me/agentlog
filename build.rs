//! Hashes the sources that decide what is taken out of a log into `AGENTLOG_READER`. The
//! cache of `search`, `show` and `timeline` records the hash it was made with; a build
//! whose readers differ in any way finds the old caches stale and makes them again, so a
//! change to how logs are read cannot be forgotten in the cache.
//!
//! Every `src/*.rs` counts except those listed here, which only count, sort or print what
//! was already read. A new file is a reader until it is listed: too many rebuilds of the
//! cache is a slow first run, too few is a wrong answer.

use std::fs;

const NOT_READERS: &[&str] = &[
    "cli.rs",
    "commands.rs",
    "load.rs",
    "main.rs",
    "needle.rs",
    "recall.rs",
    "sig.rs",
    "timeline.rs",
];

fn main() {
    let mut files: Vec<_> = fs::read_dir("src")
        .expect("src")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            !NOT_READERS.contains(&name.as_ref())
        })
        .collect();
    files.sort();
    let mut h: u64 = 0xcbf29ce484222325;
    let mut eat = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    };
    for f in &files {
        println!("cargo:rerun-if-changed={}", f.display());
        eat(f.file_name().unwrap().to_string_lossy().as_bytes());
        eat(&fs::read(f).expect("read source"));
    }
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rustc-env=AGENTLOG_READER={h:016x}");
}
