import FakeCore
import Foundation
import NVRCore
import Testing

// The lenient reading of a typed or pasted latitude and longitude (Settings >
// Smart Alerts > Night), and the sun-times answer the settings show. All
// coordinates are made up.

@Suite struct CoordinateInputTests {
    private func location(_ lat: String, _ lon: String) -> AlertLocation? {
        if case .success(let l) = CoordinateInput.resolve(lat: lat, lon: lon) { return l }
        return nil
    }

    @Test func plainDecimals() {
        #expect(location("12.3", "-45.6") == AlertLocation(lat: 12.3, lon: -45.6))
        #expect(location("  +7 ", " 0 ") == AlertLocation(lat: 7, lon: 0))
        #expect(location(".5", "5.") == AlertLocation(lat: 0.5, lon: 5))
    }

    @Test func commaAsTheDecimalSeparator() {
        #expect(location("12,3", "-45,6") == AlertLocation(lat: 12.3, lon: -45.6))
    }

    @Test func degreeSignAndHemisphereLetters() {
        #expect(location("12.3° N", "45.6 W") == AlertLocation(lat: 12.3, lon: -45.6))
        #expect(location("12.3°S", "45.6°e") == AlertLocation(lat: -12.3, lon: 45.6))
        #expect(location("N 12.3", "W 45.6°") == AlertLocation(lat: 12.3, lon: -45.6))
        #expect(location("s12,3º", "w 45,6") == AlertLocation(lat: -12.3, lon: -45.6))
        #expect(location("-12.3 S", "-45.6 W") == AlertLocation(lat: -12.3, lon: -45.6))
    }

    @Test func dashLookAlikes() {
        for dash in ["\u{2212}", "\u{2013}", "\u{2014}", "\u{2012}", "\u{2010}", "\u{FE63}", "\u{FF0D}"] {
            #expect(location("\(dash)12.3", "\(dash)45.6°") == AlertLocation(lat: -12.3, lon: -45.6), "\(dash)")
        }
    }

    @Test func aPairPastedIntoEitherFieldFillsBoth() {
        let want = AlertLocation(lat: 12.3, lon: -45.6)
        #expect(location("12.3, -45.6", "") == want)
        #expect(location("", "12.3, -45.6") == want)
        #expect(location("12.3,-45.6", "9") == want)
        #expect(location("12.3; -45.6", "") == want)
        #expect(location("12.3° N, 45.6° W", "") == want)
        #expect(location("12,3 -45,6", "") == want)
        #expect(location("12.3 N 45.6 W", "") == want)
        #expect(location("N 12.3 W 45.6", "") == want)
        #expect(location("\u{2212}12.3, \u{2212}45.6", "") == AlertLocation(lat: -12.3, lon: -45.6))
        // Longitude first, with letters: put the right way round.
        #expect(location("45.6 W, 12.3 N", "") == want)
    }

    @Test func rounding() {
        #expect(location("12.34", "-45.66") == AlertLocation(lat: 12.3, lon: -45.7))
    }

    @Test func refusals() {
        func failure(_ lat: String, _ lon: String) -> CoordinateInput.Failure? {
            if case .failure(let f) = CoordinateInput.resolve(lat: lat, lon: lon) { return f }
            return nil
        }
        #expect(failure("", "5") == .empty(.latitude))
        #expect(failure("5", "  ") == .empty(.longitude))
        #expect(failure("north", "5") == .unreadable(.latitude))
        #expect(failure("5", "12°18'") == .unreadable(.longitude))
        #expect(failure("1.2.3", "5") == .unreadable(.latitude))
        #expect(location("1e2", "5") == nil)
        #expect(failure("nan", "5") == .unreadable(.latitude))
        #expect(failure("91", "5") == .outOfRange(.latitude))
        #expect(failure("5", "-180.5") == .outOfRange(.longitude))
        #expect(failure("12 E", "5") == .wrongLetter(.latitude))
        #expect(failure("12", "5 N") == .wrongLetter(.longitude))
        // Never a zero: nothing in a refusal produces a location.
        #expect(location("x", "y") == nil)
        for f in [CoordinateInput.Failure.empty(.latitude), .unreadable(.longitude), .outOfRange(.latitude), .wrongLetter(.longitude)] {
            #expect(!f.message.isEmpty)
        }
    }

    @Test func oneCoordinateOnItsOwn() {
        #expect(CoordinateInput.parse("12,3° S", axis: .latitude) == .success(-12.3))
        #expect(CoordinateInput.parse("181", axis: .longitude) == .failure(.outOfRange(.longitude)))
    }
}

@Suite struct AlertSunTimesTests {
    private let now = Date(timeIntervalSince1970: 1_718_971_200)  // 2024-06-21 12:00 UTC

    @Test func theRequestCarriesTheLocationClockAndMargins() throws {
        var night = AlertNightSettings()
        night.afterSunsetMin = 20
        night.beforeSunriseMin = 10
        let json = AlertSunTimes.request(
            location: AlertLocation(lat: 12.3, lon: -45.6), now: now, utcOffsetMinutes: -180, night: night)
        let o = try #require(try JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
        #expect(o["lat"] as? Double == 12.3 && o["lon"] as? Double == -45.6)
        #expect(o["at_ms"] as? Int64 == 1_718_971_200_000)
        #expect(o["utc_offset_min"] as? Int == -180 && o["after_sunset_min"] as? Int == 20 && o["before_sunrise_min"] as? Int == 10)
    }

    @Test func theAnswerIsRead() throws {
        let json = """
            {"date":"2024-06-21","kind":"normal","sunrise_ms":1718949600000,"sunset_ms":1718992800000,
             "night_start_ms":1718994600000,"night_end_ms":1719035400000,"future":1}
            """
        let t = try #require(AlertSunTimes.parse(json))
        #expect(t.kind == .normal && t.date == "2024-06-21")
        #expect(t.sunrise == Date(timeIntervalSince1970: 1_718_949_600) && t.nightEnd == Date(timeIntervalSince1970: 1_719_035_400))
        let utc = try #require(TimeZone(identifier: "UTC"))
        let lines = t.lines(zone: utc, locale: Locale(identifier: "en_US"))
        #expect(lines.count == 2 && lines[0].contains("sunrise") && lines[0].contains("sunset"))
        #expect(lines[1].hasPrefix("Night:") && lines[1].contains("(next day)"))
    }

    @Test func polarDaysAndNoNight() throws {
        let day = try #require(
            AlertSunTimes.parse(
                #"{"date":"2024-06-21","kind":"polar_day","sunrise_ms":null,"sunset_ms":null,"night_start_ms":null,"night_end_ms":null}"#))
        #expect(day.kind == .polarDay && day.sunrise == nil && day.lines().count == 1)
        let none = try #require(
            AlertSunTimes.parse(
                #"{"date":"2024-06-21","kind":"normal","sunrise_ms":1718949600000,"sunset_ms":1718992800000,"night_start_ms":null,"night_end_ms":null}"#
            ))
        #expect(none.lines().last == "No night tonight with these margins.")
        #expect(AlertSunTimes.parse("{}") == nil && AlertSunTimes.parse("[") == nil)
    }

    @Test func theFakeCoreRefusesWhatTheCoreRefuses() throws {
        let rig = try Rig(FakeCore.Options(clips: [try Fixtures.clip("h264-basic.h264")]))
        defer { rig.bridge.shutdown() }
        let fake = rig.fake
        let ok = try fake.alertsSunTimes(json: #"{"lat":12.3,"lon":-45.6,"at_ms":1718971200000,"utc_offset_min":-180}"#)
        #expect(AlertSunTimes.parse(ok)?.kind == .normal)
        #expect(throws: CoreStatus.invalidArgument) {
            _ = try fake.alertsSunTimes(json: #"{"lat":91,"lon":0,"at_ms":0}"#)
        }
    }
}
