import Foundation

// Today's predicted sunrise, sunset, and night window for the Night
// settings: the request and the answer of `nv_alerts_sun_times` (minor 13;
// docs/FFI.md "Smart alerts"). The arithmetic is the core's; this only
// writes the request and reads the answer.

public struct AlertSunTimes: Equatable, Sendable {
    public enum Kind: String, Sendable { case normal, polarDay = "polar_day", polarNight = "polar_night" }

    /// The local date the answer is for, "YYYY-MM-DD".
    public var date: String
    public var kind: Kind
    public var sunrise: Date?
    public var sunset: Date?
    /// When the night that starts this evening begins and ends (the next
    /// sunrise less its margin); nil when there is none.
    public var nightStart: Date?
    public var nightEnd: Date?

    public init(
        date: String, kind: Kind, sunrise: Date? = nil, sunset: Date? = nil, nightStart: Date? = nil,
        nightEnd: Date? = nil
    ) {
        self.date = date
        self.kind = kind
        self.sunrise = sunrise
        self.sunset = sunset
        self.nightStart = nightStart
        self.nightEnd = nightEnd
    }

    /// The request for `location` at `now` (`utcOffsetMinutes` picks the date).
    public static func request(
        location: AlertLocation, now: Date, utcOffsetMinutes: Int, night: AlertNightSettings
    ) -> String {
        let obj: [String: Any] = [
            "lat": location.lat, "lon": location.lon, "at_ms": Int64((now.timeIntervalSince1970 * 1000).rounded()),
            "utc_offset_min": utcOffsetMinutes, "after_sunset_min": night.afterSunsetMin,
            "before_sunrise_min": night.beforeSunriseMin,
        ]
        let data = (try? JSONSerialization.data(withJSONObject: obj, options: [.sortedKeys])) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    /// Reads the core's answer; nil when it is not one.
    public static func parse(_ json: String) -> AlertSunTimes? {
        guard let o = try? JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any],
            let date = o["date"] as? String, let kind = (o["kind"] as? String).flatMap(Kind.init(rawValue:))
        else { return nil }
        func at(_ key: String) -> Date? {
            (o[key] as? NSNumber).map { Date(timeIntervalSince1970: $0.doubleValue / 1000) }
        }
        return AlertSunTimes(
            date: date, kind: kind, sunrise: at("sunrise_ms"), sunset: at("sunset_ms"),
            nightStart: at("night_start_ms"), nightEnd: at("night_end_ms"))
    }

    /// The lines the settings show, in the time zone `zone`.
    public func lines(zone: TimeZone = .current, locale: Locale = .current) -> [String] {
        let f = DateFormatter()
        f.locale = locale
        f.timeZone = zone
        f.dateStyle = .none
        f.timeStyle = .short
        let day = DateFormatter()
        day.locale = locale
        day.timeZone = zone
        day.dateFormat = "yyyy-MM-dd"
        func time(_ d: Date, relativeTo base: Date?) -> String {
            let t = f.string(from: d)
            guard let base, day.string(from: d) != day.string(from: base) else { return t }
            return t + " (next day)"
        }
        switch kind {
        case .polarDay:
            return ["The sun does not set today, so it is never night."]
        case .polarNight:
            return ["The sun does not rise today, so it is night all day."]
        case .normal:
            var out: [String] = []
            if let r = sunrise, let s = sunset {
                out.append("Today: sunrise \(f.string(from: r)), sunset \(f.string(from: s)).")
            }
            if let a = nightStart, let b = nightEnd {
                out.append("Night: \(time(a, relativeTo: nil)) to \(time(b, relativeTo: a)).")
            } else {
                out.append("No night tonight with these margins.")
            }
            return out
        }
    }
}
