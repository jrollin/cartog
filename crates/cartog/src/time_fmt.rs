//! Hand-rolled UTC ↔ RFC3339 helpers used by `state.toml` writers and
//! the auto-check predicate. Lives here so a leap-year fix can't drift
//! between the two callers.
//!
//! Format is the strict `YYYY-MM-DDTHH:MM:SSZ` subset of RFC3339 — no
//! fractional seconds, no offsets, always Z. The parser is equally
//! strict because we own the writer.

use std::time::SystemTime;

/// RFC3339 timestamp for `now` formatted as `YYYY-MM-DDTHH:MM:SSZ`.
/// Hand-rolled to avoid a chrono / time dep for one call site.
pub fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (year, month, day, hour, minute, second) = utc_breakdown(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// A coarse "how long ago" for human output: `3m ago`, `4h ago`, `2d ago`.
///
/// Deliberately coarse — the registry records when a project was last indexed,
/// and a person reading a list of projects wants to know "today or last month",
/// not a duration to the second. A future timestamp (clock skew between the
/// writer and this reader) reads as `just now` rather than a negative age.
pub fn format_relative(secs: i64) -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let age = now.saturating_sub(secs);
    if age < 60 {
        return "just now".to_string();
    }
    if age < 3600 {
        return format!("{}m ago", age / 60);
    }
    if age < 86_400 {
        return format!("{}h ago", age / 3600);
    }
    format!("{}d ago", age / 86_400)
}

/// Convert a Unix timestamp (seconds since 1970-01-01 UTC) to broken-down
/// `(year, month, day, hour, minute, second)`. Handles leap years.
pub fn utc_breakdown(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let day_secs = 86_400u64;
    let mut days = secs / day_secs;
    let rem = secs % day_secs;
    let hour = (rem / 3600) as u32;
    let minute = ((rem % 3600) / 60) as u32;
    let second = (rem % 60) as u32;

    let mut year: u32 = 1970;
    loop {
        let dy = if is_leap(year) { 366 } else { 365 };
        if days < dy {
            break;
        }
        days -= dy;
        year += 1;
    }
    let months = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month = 1u32;
    for (idx, &dm) in months.iter().enumerate() {
        let dm = if idx == 1 && is_leap(year) { 29 } else { dm };
        if days < dm {
            break;
        }
        days -= dm;
        month += 1;
    }
    let day = (days + 1) as u32;
    (year, month, day, hour, minute, second)
}

/// Parse a strict `YYYY-MM-DDTHH:MM:SSZ` timestamp into seconds since
/// Unix epoch. Returns `None` for any deviation from that exact shape.
pub fn parse_rfc3339_secs(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    if bytes.len() != 20 || bytes[19] != b'Z' {
        return None;
    }
    let read = |i: usize, n: usize| -> Option<u64> {
        let slice = std::str::from_utf8(&bytes[i..i + n]).ok()?;
        slice.parse::<u64>().ok()
    };
    if bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year = read(0, 4)?;
    let month = read(5, 2)?;
    let day = read(8, 2)?;
    let hour = read(11, 2)?;
    let minute = read(14, 2)?;
    let second = read(17, 2)?;
    // We own the writer (rfc3339_now): it never emits :60, so rejecting
    // 60 here keeps the arithmetic exact (no over-count).
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    Some(days_since_epoch(year, month, day)? * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days from 1970-01-01 to (year, month, day). Returns `None` for invalid
/// month/day combinations (e.g. 2023-02-30).
fn days_since_epoch(year: u64, month: u64, day: u64) -> Option<u64> {
    if year < 1970 {
        return None;
    }
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y as u32) { 366 } else { 365 };
    }
    let months = [31u64, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    for (idx, &dm) in months.iter().enumerate() {
        let m = (idx + 1) as u64;
        if m >= month {
            break;
        }
        let dm = if idx == 1 && is_leap(year as u32) {
            29
        } else {
            dm
        };
        days += dm;
    }
    let dim = if month == 2 && is_leap(year as u32) {
        29
    } else {
        months[(month - 1) as usize]
    };
    if day > dim {
        return None;
    }
    Some(days + day - 1)
}

/// Proleptic Gregorian leap-year rule: divisible by 4, except century years not divisible by 400.
pub fn is_leap(year: u32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {

    #[test]
    fn format_relative_buckets_by_magnitude() {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_relative(now), "just now");
        assert_eq!(format_relative(now - 300), "5m ago");
        assert_eq!(format_relative(now - 7200), "2h ago");
        assert_eq!(format_relative(now - 172_800), "2d ago");
    }

    #[test]
    fn format_relative_treats_a_future_timestamp_as_just_now() {
        // Clock skew between the writing process and this reader must not
        // render as a negative age.
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert_eq!(format_relative(now + 10_000), "just now");
    }

    use super::*;

    #[test]
    fn is_leap_handles_century_rule() {
        assert!(is_leap(2000));
        assert!(!is_leap(1900));
        assert!(is_leap(2024));
        assert!(!is_leap(2023));
    }

    #[test]
    fn utc_breakdown_known_timestamps() {
        assert_eq!(utc_breakdown(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(utc_breakdown(1_767_225_600), (2026, 1, 1, 0, 0, 0));
        assert_eq!(utc_breakdown(1_709_210_096), (2024, 2, 29, 12, 34, 56));
        assert_eq!(utc_breakdown(951_868_800), (2000, 3, 1, 0, 0, 0));
    }

    #[test]
    fn parse_rfc3339_known_timestamps() {
        assert_eq!(parse_rfc3339_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_secs("2024-01-01T00:00:00Z"),
            Some(1_704_067_200)
        );
        assert_eq!(
            parse_rfc3339_secs("2024-02-29T12:34:56Z"),
            Some(1_709_210_096)
        );
    }

    #[test]
    fn parse_rfc3339_rejects_malformed() {
        assert_eq!(parse_rfc3339_secs(""), None);
        assert_eq!(parse_rfc3339_secs("2024-01-01"), None);
        assert_eq!(parse_rfc3339_secs("2024-01-01T00:00:00"), None);
        assert_eq!(parse_rfc3339_secs("2024-13-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_secs("2023-02-29T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_secs("2024-01-01T25:00:00Z"), None);
        assert_eq!(parse_rfc3339_secs("2024-06-30T23:59:60Z"), None);
    }

    /// Round-trip property: format → parse must recover the exact second.
    /// Catches divergence between `utc_breakdown` and `parse_rfc3339_secs`.
    #[test]
    fn breakdown_and_parse_round_trip() {
        for &secs in &[
            0u64,
            86_400,
            1_577_836_800,  // 2020-01-01
            1_704_067_200,  // 2024-01-01
            1_709_210_096,  // 2024-02-29 leap
            951_868_800,    // 2000-03-01
            32_503_680_000, // 3000-01-01
        ] {
            let (y, m, d, h, mi, s) = utc_breakdown(secs);
            let formatted = format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z");
            assert_eq!(
                parse_rfc3339_secs(&formatted),
                Some(secs),
                "round-trip failed for {secs}: {formatted}"
            );
        }
    }
}
