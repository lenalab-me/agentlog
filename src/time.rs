//! Times are epoch seconds (UTC). Session logs carry UTC ISO 8601 stamps; what people type
//! and read is local time, so parsing without a zone and every printed date use the local
//! offset of this machine.

use std::time::{SystemTime, UNIX_EPOCH};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Local offset from UTC in seconds at time `t`.
pub fn local_offset(t: i64) -> i64 {
    let tt = t as libc::time_t;
    // SAFETY: localtime_r only writes into the tm we own.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&tt, &mut tm).is_null() {
            0
        } else {
            tm.tm_gmtoff as i64
        }
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn num(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `YYYY-MM-DD[THH:MM[:SS[.fff]]][Z|±HH:MM]`. Without a zone the time is local.
pub fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_at(s.find(['T', ' ']).unwrap_or(s.len()));
    let mut dp = date.split('-');
    let (y, mo, d) = (num(dp.next()?)?, num(dp.next()?)?, num(dp.next()?)?);
    if dp.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let rest = rest.get(1..).unwrap_or("");
    let (clock, zone) = match rest.find(['Z', '+', '-']) {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    let mut secs = 0;
    if !clock.is_empty() {
        let clock = clock.split('.').next().unwrap_or("");
        let mut cp = clock.split(':');
        let h = num(cp.next()?)?;
        let mi = num(cp.next()?)?;
        let se = match cp.next() {
            Some(x) => num(x)?,
            None => 0,
        };
        if cp.next().is_some() || h > 23 || mi > 59 || se > 60 {
            return None;
        }
        secs = h * 3600 + mi * 60 + se;
    }
    let naive = days_from_civil(y, mo, d) * 86400 + secs;
    let offset = match zone {
        None => local_offset(naive),
        Some("Z") => 0,
        Some(z) => {
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let mut zp = z[1..].split(':');
            let h = num(zp.next()?)?;
            let m = match zp.next() {
                Some(x) => num(x)?,
                None => 0,
            };
            sign * (h * 3600 + m * 60)
        }
    };
    Some(naive - offset)
}

/// `--since`/`--until`: an ISO time (local unless it has a zone), or `Nd` / `Nh` before now.
pub fn parse_when(s: &str, now: i64) -> Option<i64> {
    if let Some(n) = s.strip_suffix('d').and_then(num) {
        return Some(now - n * 86400);
    }
    if let Some(n) = s.strip_suffix('h').and_then(num) {
        return Some(now - n * 3600);
    }
    parse_iso(s)
}

fn local_parts(t: i64) -> (i64, i64, i64, i64, i64) {
    let l = t + local_offset(t);
    let (y, m, d) = civil_from_days(l.div_euclid(86400));
    let s = l.rem_euclid(86400);
    (y, m, d, s / 3600, s % 3600 / 60)
}

/// Local calendar day, `YYYY-MM-DD`.
pub fn day(t: i64) -> String {
    let (y, m, d, _, _) = local_parts(t);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Local time to the minute, `YYYY-MM-DD HH:MM`.
pub fn minute(t: i64) -> String {
    let (y, m, d, h, mi) = local_parts(t);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}")
}

/// UTC, `YYYY-MM-DDTHH:MM:SSZ`.
pub fn iso_utc(t: i64) -> String {
    let (y, m, d) = civil_from_days(t.div_euclid(86400));
    let s = t.rem_euclid(86400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s % 3600 / 60,
        s % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_stamps() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2026-10-13T11:49:19.333Z"), Some(1791892159));
        assert_eq!(parse_iso("2026-10-13T20:49:19+09:00"), Some(1791892159));
        assert_eq!(
            parse_iso("2026-10-13T11:49:19-00:30"),
            Some(1791892159 + 1800)
        );
        assert_eq!(iso_utc(1791892159), "2026-10-13T11:49:19Z");
        assert_eq!(parse_iso(&iso_utc(1791892159)), Some(1791892159));
    }

    #[test]
    fn rejects_garbage() {
        for s in [
            "",
            "2026",
            "2026-13-01",
            "2026-10-01T25:00",
            "10/01",
            "2026-10-01Tab",
        ] {
            assert_eq!(parse_iso(s), None, "{s}");
        }
    }

    #[test]
    fn local_round_trip() {
        let t = parse_iso("2026-10-07T08:57").unwrap();
        assert_eq!(minute(t), "2026-10-07 08:57");
        assert_eq!(day(t), "2026-10-07");
    }

    #[test]
    fn relative() {
        assert_eq!(parse_when("2d", 1_000_000), Some(1_000_000 - 172800));
        assert_eq!(parse_when("5h", 1_000_000), Some(1_000_000 - 18000));
    }
}
