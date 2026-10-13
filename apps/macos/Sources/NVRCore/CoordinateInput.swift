import Foundation

// Reading a latitude and a longitude the way people type or paste them
// (Settings > Smart Alerts > Night). Lenient about the writing, strict about
// the meaning: anything that is not clearly one coordinate, or is out of
// range, is refused with a sentence the user can act on, and never turned
// into 0.
//
// Accepted: plain decimals ("12.3", "-45.6", "+7"), a comma as the decimal
// separator ("12,3"), a degree sign ("12.3°"), N/S/E/W before or after the
// number (S and W mean negative), the Unicode minus and other dash
// look-alikes, surrounding spaces, and "lat, lon" (also separated by ";" or
// spaces) pasted into either field, which fills both. Degrees and decimal
// minutes ("12°18'") are not read: they are refused, not guessed.

public enum CoordinateInput {
    /// Why an entry was refused.
    public enum Failure: Error, Equatable, Sendable {
        /// A field is empty.
        case empty(Axis)
        /// The text is not a coordinate (or has a degrees-and-minutes form).
        case unreadable(Axis)
        /// The number is outside -90...90 or -180...180.
        case outOfRange(Axis)
        /// A letter that belongs to the other axis (E or W in the latitude).
        case wrongLetter(Axis)

        public var message: String {
            switch self {
            case .empty(let a): return "Enter a \(a.name)."
            case .unreadable(let a):
                return
                    "Could not read the \(a.name). Use a decimal number such as \(a.example), with an optional N/S/E/W and °."
            case .outOfRange(let a): return "The \(a.name) must be between -\(a.limit) and \(a.limit) degrees."
            case .wrongLetter(let a): return "The \(a.name) cannot use \(a == .latitude ? "E or W" : "N or S")."
            }
        }
    }

    public enum Axis: Sendable {
        case latitude, longitude

        var name: String { self == .latitude ? "latitude" : "longitude" }
        var example: String { self == .latitude ? "12.3 or 12.3° N" : "-45.6 or 45.6° W" }
        var limit: Int { self == .latitude ? 90 : 180 }
    }

    /// One number with the hemisphere letter it came with, if any.
    struct Token: Equatable {
        var value: Double
        var letter: Character?
    }

    /// The location for the two fields' text, or why not. A "lat, lon" pair
    /// in either field fills both (the other field's text is then ignored);
    /// a pair written longitude first (E or W before N or S) is put the
    /// right way round.
    public static func resolve(lat: String, lon: String) -> Result<AlertLocation, Failure> {
        for text in [lat, lon] {
            if let pair = tokens(text), pair.count == 2 { return place(pair[0], pair[1]) }
        }
        let latTokens = tokens(lat), lonTokens = tokens(lon)
        let la = lat.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        let lo = lon.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        if la { return .failure(.empty(.latitude)) }
        guard let a = latTokens, a.count == 1 else { return .failure(.unreadable(.latitude)) }
        if lo { return .failure(.empty(.longitude)) }
        guard let b = lonTokens, b.count == 1 else { return .failure(.unreadable(.longitude)) }
        return place(a[0], b[0])
    }

    /// One coordinate on its own (a field's text).
    public static func parse(_ text: String, axis: Axis) -> Result<Double, Failure> {
        if text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { return .failure(.empty(axis)) }
        guard let t = tokens(text), t.count == 1 else { return .failure(.unreadable(axis)) }
        return check(t[0], axis)
    }

    private static func place(_ first: Token, _ second: Token) -> Result<AlertLocation, Failure> {
        var (a, b) = (first, second)
        if let x = a.letter, let y = b.letter, "EW".contains(x), "NS".contains(y) { (a, b) = (b, a) }
        switch (check(a, .latitude), check(b, .longitude)) {
        case (.success(let la), .success(let lo)): return .success(AlertLocation(lat: la, lon: lo))
        case (.failure(let f), _): return .failure(f)
        case (_, .failure(let f)): return .failure(f)
        }
    }

    private static func check(_ t: Token, _ axis: Axis) -> Result<Double, Failure> {
        if let l = t.letter {
            let ok = axis == .latitude ? "NS".contains(l) : "EW".contains(l)
            if !ok { return .failure(.wrongLetter(axis)) }
        }
        guard abs(t.value) <= Double(axis.limit) else { return .failure(.outOfRange(axis)) }
        return .success(t.value)
    }

    private static let dashes: Set<Character> = [
        "\u{2212}", "\u{2010}", "\u{2011}", "\u{2012}", "\u{2013}", "\u{2014}", "\u{2015}", "\u{FE58}", "\u{FE63}",
        "\u{FF0D}", "\u{02D7}",
    ]
    private static let degrees: Set<Character> = ["\u{00B0}", "\u{00BA}", "\u{02DA}", "\u{2218}"]

    // A number with its hemisphere letter before or after it.
    private static let pattern: NSRegularExpression = {
        let num = #"[-+]?(?:\d+(?:[.,]\d*)?|[.,]\d+)"#
        let p = #"([NSEW])\s*(\#(num))(?:\s*°)?|(\#(num))(?:\s*°)?(?:\s*([NSEW]))?"#
        // Force-try: the pattern is a constant.
        return try! NSRegularExpression(pattern: p, options: [.caseInsensitive])
    }()

    /// The numbers in `text`, or nil when something else is in it (other
    /// than spaces, commas, and semicolons between numbers).
    static func tokens(_ text: String) -> [Token]? {
        var s = ""
        for ch in text {
            if dashes.contains(ch) {
                s.append("-")
            } else if degrees.contains(ch) {
                s.append("°")
            } else if ch.isWhitespace {
                s.append(" ")
            } else {
                s.append(ch)
            }
        }
        let ns = s as NSString
        var out: [Token] = []
        var at = 0
        for m in pattern.matches(in: s, range: NSRange(location: 0, length: ns.length)) {
            // Only separators may lie between numbers.
            let gap = ns.substring(with: NSRange(location: at, length: m.range.location - at))
            guard gap.allSatisfy({ $0 == " " || $0 == "," || $0 == ";" }) else { return nil }
            // "1.2.3" is not two numbers.
            if gap.isEmpty, let last = out.last, last.letter == nil { return nil }
            at = m.range.location + m.range.length
            let lead = m.range(at: 1).location != NSNotFound
            let number = ns.substring(with: m.range(at: lead ? 2 : 3))
            let letterRange = m.range(at: lead ? 1 : 4)
            let letter = letterRange.location == NSNotFound ? nil : ns.substring(with: letterRange).uppercased().first
            guard var v = Double(number.replacingOccurrences(of: ",", with: ".")) else { return nil }
            if let l = letter, "SW".contains(l), v > 0 { v = -v }
            out.append(Token(value: v, letter: letter))
        }
        let tail = ns.substring(from: at)
        guard tail.allSatisfy({ $0 == " " || $0 == "," || $0 == ";" }), !out.isEmpty else { return nil }
        return out
    }
}
