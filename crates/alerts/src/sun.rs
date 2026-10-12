//! Sunrise, sunset, and "is it night" from a coarse location.
//!
//! The arithmetic follows the published NOAA solar calculator equations
//! (Julian century, the sun's apparent longitude, the equation of time, and
//! the hour angle for a zenith of 90.833°, which allows for refraction and
//! the sun's radius), refined once at the event's own time. It is pure: the
//! caller passes the instant, the location, and the host's UTC offset.
//!
//! "Night" is the time from `after_sunset_min` after a sunset to
//! `before_sunrise_min` before the next sunrise. Close to the poles a day
//! can have no sunrise (polar night, all night) or no sunset (polar day,
//! never night). When the margins are longer than the dark part of a short
//! summer night, there is no night at all that day.

use crate::time::{Date, MINUTE_MS, Millis, local};

/// What the sun does on one calendar date at one place.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SunDay {
    /// The sun rises and sets. Both are UTC instants; for places far from
    /// Greenwich the sunset can fall on the next UTC date.
    Normal { rise_ms: Millis, set_ms: Millis },
    /// The sun stays above the horizon all day.
    PolarDay,
    /// The sun stays below the horizon all day.
    PolarNight,
}

const ZENITH_DEG: f64 = 90.833;

fn julian_century(jd: f64) -> f64 {
    (jd - 2_451_545.0) / 36_525.0
}

/// Solar declination (radians) and the equation of time (minutes) at
/// Julian century `t`.
fn sun_position(t: f64) -> (f64, f64) {
    let l0 = (280.466_46 + t * (36_000.769_83 + t * 0.000_303_2)).rem_euclid(360.0);
    let m = 357.529_11 + t * (35_999.050_29 - 0.000_153_7 * t);
    let e = 0.016_708_634 - t * (0.000_042_037 + 0.000_000_126_7 * t);
    let mr = m.to_radians();
    let c = mr.sin() * (1.914_602 - t * (0.004_817 + 0.000_014 * t))
        + (2.0 * mr).sin() * (0.019_993 - 0.000_101 * t)
        + (3.0 * mr).sin() * 0.000_289;
    let true_long = l0 + c;
    let omega = (125.04 - 1934.136 * t).to_radians();
    let app_long = true_long - 0.005_69 - 0.004_78 * omega.sin();
    let seconds = 21.448 - t * (46.815 + t * (0.000_59 - t * 0.001_813));
    let obliq_mean = 23.0 + (26.0 + seconds / 60.0) / 60.0;
    let obliq = (obliq_mean + 0.002_56 * omega.cos()).to_radians();
    let decl = (obliq.sin() * app_long.to_radians().sin()).asin();
    let y = (obliq / 2.0).tan().powi(2);
    let l0r = l0.to_radians();
    let eq = y * (2.0 * l0r).sin() - 2.0 * e * mr.sin()
        + 4.0 * e * y * mr.sin() * (2.0 * l0r).cos()
        - 0.5 * y * y * (4.0 * l0r).sin()
        - 1.25 * e * e * (2.0 * mr).sin();
    (decl, 4.0 * eq.to_degrees())
}

/// The cosine of the sunrise hour angle; outside [-1, 1] the sun does not
/// cross the horizon.
fn cos_hour_angle(lat_rad: f64, decl: f64) -> f64 {
    ZENITH_DEG.to_radians().cos() / (lat_rad.cos() * decl.cos()) - lat_rad.tan() * decl.tan()
}

/// Minutes after 00:00 UTC of the date with Julian day `jd0` (at 0h) when the
/// sun rises (`rise`) or sets, evaluated at Julian century `t`.
fn event_minutes(rise: bool, t: f64, lat_rad: f64, lon: f64) -> Option<f64> {
    let (decl, eq) = sun_position(t);
    let c = cos_hour_angle(lat_rad, decl);
    if !(-1.0..=1.0).contains(&c) {
        return None;
    }
    let ha = c.acos().to_degrees();
    let ha = if rise { ha } else { -ha };
    Some(720.0 - 4.0 * (lon + ha) - eq)
}

/// Sunrise and sunset for the solar day around local noon of `date` at
/// `lat`, `lon` (degrees; north and east positive).
pub fn sun_day(date: Date, lat: f64, lon: f64) -> SunDay {
    // Keep away from the exact poles, where the hour angle is undefined.
    let lat = lat.clamp(-89.9, 89.9);
    let lat_rad = lat.to_radians();
    let jd0 = date.days_since_epoch() as f64 + 2_440_587.5;
    let noon_min = 720.0 - 4.0 * lon;
    let (decl, _) = sun_position(julian_century(jd0 + noon_min / 1440.0));
    let c = cos_hour_angle(lat_rad, decl);
    if c > 1.0 {
        return SunDay::PolarNight;
    }
    if c < -1.0 {
        return SunDay::PolarDay;
    }
    let event = |rise: bool| -> f64 {
        let first = event_minutes(rise, julian_century(jd0 + noon_min / 1440.0), lat_rad, lon)
            .unwrap_or(noon_min);
        // Refine once at the event's own time; near the polar transitions
        // the refinement can fall off the horizon, so keep the first value.
        event_minutes(rise, julian_century(jd0 + first / 1440.0), lat_rad, lon).unwrap_or(first)
    };
    let base = date.utc_midnight_ms();
    let to_ms = |minutes: f64| base + (minutes * MINUTE_MS as f64).round() as Millis;
    SunDay::Normal {
        rise_ms: to_ms(event(true)),
        set_ms: to_ms(event(false)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Edge {
    Rise,
    Set,
}

/// Whether `at_ms` is night at `lat`, `lon`: at least `after_sunset_min`
/// after the last sunset and at least `before_sunrise_min` before the next
/// sunrise. `offset_min` (the host's UTC offset) picks the dates looked at;
/// the answer for an instant does not depend on it.
pub fn is_night(
    at_ms: Millis,
    lat: f64,
    lon: f64,
    offset_min: i32,
    after_sunset_min: u16,
    before_sunrise_min: u16,
) -> bool {
    let today = local(at_ms, offset_min).date;
    let mut edges: Vec<(Millis, Edge)> = Vec::with_capacity(10);
    for d in -2..=2 {
        if let SunDay::Normal { rise_ms, set_ms } = sun_day(today.add_days(d), lat, lon) {
            edges.push((rise_ms, Edge::Rise));
            edges.push((set_ms, Edge::Set));
        }
    }
    edges.sort_by_key(|e| e.0);
    let prev = edges.iter().rev().find(|e| e.0 <= at_ms);
    let next = edges.iter().find(|e| e.0 > at_ms);
    let down = match (prev, next) {
        (Some(p), _) => p.1 == Edge::Set,
        (None, Some(n)) => n.1 == Edge::Rise,
        (None, None) => {
            // No sunrise or sunset within two days either side: polar.
            matches!(sun_day(today, lat, lon), SunDay::PolarNight)
        }
    };
    if !down {
        return false;
    }
    let after = Millis::from(after_sunset_min) * MINUTE_MS;
    let before = Millis::from(before_sunrise_min) * MINUTE_MS;
    let prev_ok = prev.is_none_or(|p| at_ms - p.0 >= after);
    let next_ok = next.is_none_or(|n| n.0 - at_ms >= before);
    prev_ok && next_ok
}

/// The fixed-hours fallback when no location is set: night from
/// `start_min` to `end_min` local time (crossing midnight when start > end).
pub fn is_fixed_night(at_ms: Millis, offset_min: i32, start_min: u16, end_min: u16) -> bool {
    in_daily_window(local(at_ms, offset_min).minute_of_day, start_min, end_min)
}

/// Whether a minute of the day is within [start, end), wrapping at midnight
/// when start > end. Equal start and end mean the whole day.
pub fn in_daily_window(minute: u16, start: u16, end: u16) -> bool {
    let (start, end) = (start % 1440, end % 1440);
    if start == end {
        true
    } else if start < end {
        minute >= start && minute < end
    } else {
        minute >= start || minute < end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::HOUR_MS;

    fn at(date: Date, hh: i64, mm: i64, ss: i64) -> Millis {
        date.utc_midnight_ms() + hh * HOUR_MS + mm * MINUTE_MS + ss * 1000
    }

    /// Reference times, UTC, from an independent implementation of the
    /// NOAA equations, for the solar day around local noon of each date.
    /// Tolerance: two minutes (four close to the polar circle).
    /// A UTC date and time of day.
    type Utc = (Date, i64, i64, i64);

    #[test]
    fn matches_reference_values() {
        #[rustfmt::skip]
        let cases: &[(f64, f64, Date, Utc, Utc, i64)] = &[
            // Greenwich, solstices and an equinox.
            (51.5, 0.0, Date::new(2024, 6, 21), (Date::new(2024, 6, 21), 3, 43, 5), (Date::new(2024, 6, 21), 20, 20, 44), 2),
            (51.5, 0.0, Date::new(2024, 12, 21), (Date::new(2024, 12, 21), 8, 3, 48), (Date::new(2024, 12, 21), 15, 52, 49), 2),
            (51.5, 0.0, Date::new(2024, 3, 20), (Date::new(2024, 3, 20), 6, 2, 2), (Date::new(2024, 3, 20), 18, 13, 32), 2),
            // The equator.
            (0.0, 0.0, Date::new(2024, 3, 20), (Date::new(2024, 3, 20), 6, 4, 12), (Date::new(2024, 3, 20), 18, 10, 22), 2),
            (0.0, 0.0, Date::new(2024, 12, 21), (Date::new(2024, 12, 21), 5, 54, 45), (Date::new(2024, 12, 21), 18, 1, 53), 2),
            // West of Greenwich: the sunset falls on the next UTC date.
            (40.7, -74.0, Date::new(2024, 6, 21), (Date::new(2024, 6, 21), 9, 25, 24), (Date::new(2024, 6, 22), 0, 30, 30), 2),
            (40.7, -74.0, Date::new(2024, 12, 21), (Date::new(2024, 12, 21), 12, 17, 2), (Date::new(2024, 12, 21), 21, 31, 48), 2),
            // Southern hemisphere, east: the sunrise falls on the previous UTC date.
            (-33.9, 151.2, Date::new(2024, 6, 21), (Date::new(2024, 6, 20), 21, 0, 25), (Date::new(2024, 6, 21), 6, 53, 39), 2),
            (-33.9, 151.2, Date::new(2024, 12, 21), (Date::new(2024, 12, 20), 18, 41, 4), (Date::new(2024, 12, 21), 9, 5, 33), 2),
            // High latitudes.
            (64.1, -21.9, Date::new(2024, 6, 21), (Date::new(2024, 6, 21), 2, 57, 26), (Date::new(2024, 6, 22), 0, 1, 31), 4),
            (64.1, -21.9, Date::new(2024, 12, 21), (Date::new(2024, 12, 21), 11, 22, 29), (Date::new(2024, 12, 21), 15, 29, 25), 4),
            (69.6, 19.0, Date::new(2024, 1, 16), (Date::new(2024, 1, 16), 10, 20, 0), (Date::new(2024, 1, 16), 11, 27, 40), 4),
            (69.6, 19.0, Date::new(2024, 3, 20), (Date::new(2024, 3, 20), 4, 42, 1), (Date::new(2024, 3, 20), 17, 2, 45), 4),
            (-77.8, 166.7, Date::new(2024, 3, 20), (Date::new(2024, 3, 19), 18, 43, 10), (Date::new(2024, 3, 20), 7, 14, 12), 4),
        ];
        for &(lat, lon, date, rise, set, tol_min) in cases {
            let SunDay::Normal { rise_ms, set_ms } = sun_day(date, lat, lon) else {
                panic!("{lat},{lon} {date:?}: expected a normal day");
            };
            let want_rise = at(rise.0, rise.1, rise.2, rise.3);
            let want_set = at(set.0, set.1, set.2, set.3);
            let tol = tol_min * MINUTE_MS;
            assert!(
                (rise_ms - want_rise).abs() <= tol,
                "{lat},{lon} {date:?} rise off by {} s",
                (rise_ms - want_rise) / 1000
            );
            assert!(
                (set_ms - want_set).abs() <= tol,
                "{lat},{lon} {date:?} set off by {} s",
                (set_ms - want_set) / 1000
            );
        }
    }

    #[test]
    fn polar_days_and_nights() {
        assert_eq!(
            sun_day(Date::new(2024, 6, 21), 69.6, 19.0),
            SunDay::PolarDay
        );
        assert_eq!(
            sun_day(Date::new(2024, 5, 20), 69.6, 19.0),
            SunDay::PolarDay
        );
        assert_eq!(
            sun_day(Date::new(2024, 12, 21), 69.6, 19.0),
            SunDay::PolarNight
        );
        assert_eq!(
            sun_day(Date::new(2024, 6, 21), -77.8, 166.7),
            SunDay::PolarNight
        );
        assert_eq!(
            sun_day(Date::new(2024, 12, 21), -77.8, 166.7),
            SunDay::PolarDay
        );
        // The exact pole does not panic.
        assert_eq!(sun_day(Date::new(2024, 6, 21), 90.0, 0.0), SunDay::PolarDay);
        // Polar night: night all day, whatever the margins.
        let d = Date::new(2024, 12, 21);
        for h in [0, 6, 11, 12, 18, 23] {
            assert!(is_night(at(d, h, 0, 0), 69.6, 19.0, 60, 30, 30));
        }
        // Polar day: never night.
        let d = Date::new(2024, 6, 21);
        for h in [0, 6, 12, 23] {
            assert!(!is_night(at(d, h, 0, 0), 69.6, 19.0, 120, 30, 30));
        }
    }

    #[test]
    fn night_uses_the_margins() {
        // Greenwich, 2024-12-21: sunset about 15:53, next sunrise about 08:04.
        let d = Date::new(2024, 12, 21);
        let n = |h, m| is_night(at(d, h, m, 0), 51.5, 0.0, 0, 30, 30);
        assert!(!n(12, 0));
        assert!(!n(16, 10)); // after sunset, inside the 30 min margin
        assert!(n(16, 30));
        assert!(n(23, 59));
        let n2 = |h, m| is_night(at(d.add_days(1), h, m, 0), 51.5, 0.0, 0, 30, 30);
        assert!(n2(3, 0));
        assert!(n2(7, 30)); // 34 min before sunrise
        assert!(!n2(7, 40)); // inside the margin before sunrise
        assert!(!n2(9, 0));
        // No margins: night starts right at sunset.
        assert!(is_night(at(d, 15, 55, 0), 51.5, 0.0, 0, 0, 0));
    }

    #[test]
    fn short_summer_night_can_vanish_under_margins() {
        // Near 64°N at midsummer, sunset to sunrise is about 3 h; with
        // 90-minute margins on each side nothing is left.
        let d = Date::new(2024, 6, 22);
        for m in (0..(5 * 60)).step_by(5) {
            assert!(!is_night(
                at(d, 0, 0, 0) + m * MINUTE_MS,
                64.1,
                -21.9,
                0,
                90,
                90
            ));
        }
        // With 30-minute margins, the middle of the night is night.
        assert!(is_night(at(d, 1, 30, 0), 64.1, -21.9, 0, 30, 30));
    }

    #[test]
    fn daylight_saving_is_only_an_offset() {
        // The same instants give the same answer under standard and summer
        // offsets (and any offset that keeps the dates in range).
        let base = at(Date::new(2024, 3, 10), 0, 0, 0);
        for step in 0..(48 * 4) {
            let t = base + step * 15 * MINUTE_MS;
            let a = is_night(t, 40.7, -74.0, -300, 30, 30);
            let b = is_night(t, 40.7, -74.0, -240, 30, 30);
            assert_eq!(a, b, "at step {step}");
        }
    }

    #[test]
    fn fixed_fallback_wraps_midnight() {
        let d = Date::new(2024, 1, 1);
        assert!(is_fixed_night(at(d, 23, 0, 0), 0, 22 * 60, 6 * 60));
        assert!(is_fixed_night(at(d, 5, 59, 0), 0, 22 * 60, 6 * 60));
        assert!(!is_fixed_night(at(d, 6, 0, 0), 0, 22 * 60, 6 * 60));
        assert!(!is_fixed_night(at(d, 12, 0, 0), 0, 22 * 60, 6 * 60));
        // 21:30 UTC is 22:30 at UTC+1.
        assert!(is_fixed_night(at(d, 21, 30, 0), 60, 22 * 60, 6 * 60));
        assert!(in_daily_window(100, 0, 0));
        assert!(in_daily_window(9 * 60, 8 * 60, 17 * 60));
        assert!(!in_daily_window(17 * 60, 8 * 60, 17 * 60));
    }
}
