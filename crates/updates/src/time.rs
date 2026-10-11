//! The manifest's dates: UTC times written as `YYYY-MM-DDTHH:MM:SSZ` (the
//! one RFC 3339 form accepted), held as seconds since the Unix epoch.

/// Parses `2026-10-11T12:00:00Z`. Other offsets, fractions of a second,
/// and dates before 1970 are refused.
pub fn parse(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<u64> {
        let s = &text[from..to];
        if s.bytes().all(|c| c.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if year < 1970
        || !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Formats seconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
pub fn format(secs: u64) -> String {
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Now, seconds since the epoch (0 if the system clock is before it).
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

fn days_in_month(y: u64, m: u64) -> u64 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 of a date in the proleptic Gregorian calendar
/// (year 1970 or later).
fn days_from_civil(y: u64, m: u64, d: u64) -> u64 {
    let days_before_year = (1970..y).map(|yy| if is_leap(yy) { 366 } else { 365 });
    let days_before_month = (1..m).map(|mm| days_in_month(y, mm));
    days_before_year.sum::<u64>() + days_before_month.sum::<u64>() + d - 1
}

fn civil_from_days(mut days: u64) -> (u64, u64, u64) {
    let mut y = 1970;
    loop {
        let len = if is_leap(y) { 366 } else { 365 };
        if days < len {
            break;
        }
        days -= len;
        y += 1;
    }
    let mut m = 1;
    while days >= days_in_month(y, m) {
        days -= days_in_month(y, m);
        m += 1;
    }
    (y, m, days + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_known_values() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(parse("2026-10-11T12:34:56Z"), Some(1_791_722_096));
        for s in [
            "1970-01-01T00:00:00Z",
            "2000-02-29T23:59:59Z",
            "2026-10-11T12:34:56Z",
            "2100-12-31T00:00:01Z",
        ] {
            assert_eq!(format(parse(s).unwrap()), s);
        }
    }

    #[test]
    fn refuses_other_forms() {
        for bad in [
            "",
            "2026-10-11",
            "2026-10-11T12:34:56",
            "2026-10-11T12:34:56+00:00",
            "2026-10-11T12:34:56.5Z",
            "2026-10-11 12:34:56Z",
            "1969-12-31T23:59:59Z",
            "2026-13-01T00:00:00Z",
            "2026-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-10-11T24:00:00Z",
            "2026-10-11T12:60:00Z",
            "2026-10-11T12:00:60Z",
            "2026-1a-11T12:00:00Z",
            "+026-10-11T12:00:00Z",
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }
}
