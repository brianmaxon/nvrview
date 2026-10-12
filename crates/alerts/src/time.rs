//! Calendar arithmetic on UTC milliseconds.
//!
//! The core has no time-zone database, so every local time here comes from a
//! UTC offset the host passes in (its current offset, which already includes
//! daylight saving time).

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch, UTC.
pub type Millis = i64;

pub const SECOND_MS: Millis = 1_000;
pub const MINUTE_MS: Millis = 60 * SECOND_MS;
pub const HOUR_MS: Millis = 60 * MINUTE_MS;
pub const DAY_MS: Millis = 24 * HOUR_MS;

/// The largest UTC offset accepted, in minutes (UTC+14:00 and UTC-14:00).
pub const MAX_OFFSET_MIN: i32 = 14 * 60;

/// A proleptic Gregorian calendar date.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
}

impl Date {
    pub const fn new(year: i32, month: u8, day: u8) -> Self {
        Date { year, month, day }
    }

    /// Days since 1970-01-01.
    pub fn days_since_epoch(self) -> i64 {
        // Howard Hinnant's days_from_civil.
        let m = i64::from(self.month);
        let d = i64::from(self.day);
        let y = i64::from(self.year) - i64::from(m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// The date `days` days after 1970-01-01.
    pub fn from_days(days: i64) -> Self {
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as i32;
        Date { year, month, day }
    }

    pub fn add_days(self, n: i64) -> Self {
        Date::from_days(self.days_since_epoch() + n)
    }

    /// Midnight at the start of this date, UTC.
    pub fn utc_midnight_ms(self) -> Millis {
        self.days_since_epoch() * DAY_MS
    }

    /// Day of the week, 0 = Monday through 6 = Sunday.
    pub fn weekday(self) -> u8 {
        // 1970-01-01 was a Thursday (3 when Monday is 0).
        (self.days_since_epoch() + 3).rem_euclid(7) as u8
    }
}

/// A moment seen on the local wall clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub date: Date,
    /// Minutes since local midnight, 0..1440.
    pub minute_of_day: u16,
    /// 0 = Monday through 6 = Sunday.
    pub weekday: u8,
}

/// Clamps a host-supplied UTC offset to the range real zones use.
pub fn clamp_offset(offset_min: i32) -> i32 {
    offset_min.clamp(-MAX_OFFSET_MIN, MAX_OFFSET_MIN)
}

/// The local wall-clock time of `at_ms` under `offset_min` (minutes east of
/// UTC).
pub fn local(at_ms: Millis, offset_min: i32) -> LocalTime {
    let local_ms = at_ms + Millis::from(clamp_offset(offset_min)) * MINUTE_MS;
    let days = local_ms.div_euclid(DAY_MS);
    let minute_of_day = (local_ms.rem_euclid(DAY_MS) / MINUTE_MS) as u16;
    let date = Date::from_days(days);
    LocalTime {
        date,
        minute_of_day,
        weekday: date.weekday(),
    }
}

/// "HH:MM" for a minute of the day.
pub fn hhmm(minute_of_day: u16) -> String {
    let m = minute_of_day % 1440;
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// A duration in words for explanations: "40 s", "4 min", "2 h 5 min".
pub fn duration_words(ms: Millis) -> String {
    let s = ms.max(0) / SECOND_MS;
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s / 60)
    } else {
        let h = s / 3600;
        let m = (s % 3600) / 60;
        if m == 0 {
            format!("{h} h")
        } else {
            format!("{h} h {m} min")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_round_trip() {
        for days in [
            -800_000i64,
            -1,
            0,
            1,
            59,
            60,
            365,
            10_957,
            19_723,
            2_000_000,
        ] {
            assert_eq!(Date::from_days(days).days_since_epoch(), days);
        }
        assert_eq!(Date::from_days(0), Date::new(1970, 1, 1));
        assert_eq!(Date::new(2024, 2, 29).add_days(1), Date::new(2024, 3, 1));
        assert_eq!(Date::new(2000, 1, 1).days_since_epoch(), 10_957);
    }

    #[test]
    fn weekdays() {
        assert_eq!(Date::new(1970, 1, 1).weekday(), 3); // Thursday
        assert_eq!(Date::new(2024, 6, 17).weekday(), 0); // Monday
        assert_eq!(Date::new(2024, 6, 23).weekday(), 6); // Sunday
    }

    #[test]
    fn local_time_with_offsets() {
        let at = Date::new(2024, 6, 21).utc_midnight_ms() + HOUR_MS; // 01:00 UTC
        let l = local(at, -300);
        assert_eq!(l.date, Date::new(2024, 6, 20));
        assert_eq!(hhmm(l.minute_of_day), "20:00");
        let l = local(at, 330);
        assert_eq!(hhmm(l.minute_of_day), "06:30");
        // Offsets beyond real zones are clamped.
        assert_eq!(local(at, 10_000), local(at, MAX_OFFSET_MIN));
    }

    #[test]
    fn words() {
        assert_eq!(duration_words(40_000), "40 s");
        assert_eq!(duration_words(4 * MINUTE_MS + 5_000), "4 min");
        assert_eq!(duration_words(2 * HOUR_MS), "2 h");
        assert_eq!(duration_words(2 * HOUR_MS + 5 * MINUTE_MS), "2 h 5 min");
        assert_eq!(duration_words(-5), "0 s");
    }
}
