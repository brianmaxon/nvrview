import Foundation

/// Keeps the doorbell's own microphone from putting our words into the
/// "Visitor" captions (docs/UX.md "Captions", docs/AUDIO.md "Talk-back"). The
/// doorbell plays our talk-back about a second late and its microphone hears
/// its speaker, so while talk-back plays, and for a short while after it ends,
/// a visitor caption that closely matches what we recently said or typed is
/// echo: it is not shown, not saved, and not visitor activity. Visitor text
/// that does not resemble what we sent is never filtered. Pure, with the viewer
/// clock passed in; the same rules and vectors as `CaptionEcho` in
/// NvrView.Tiles.
public struct CaptionEcho: Sendable, Equatable {
    /// How long after talk-back has ended a visitor caption can still be our
    /// own words coming back: the device lag (~1 s), the recognizer's delay,
    /// and a margin. The talk ends with the core's `stopped` event, which
    /// comes after its 1.5 s silent tail (docs/AUDIO.md), so the tail is
    /// already inside "while talk-back plays".
    public static let window = 3.0
    /// What we said is remembered this long.
    public static let memory = 30.0
    /// Share of the longer phrase's words that must match in order.
    public static let similarity = 0.6
    /// A visitor line that is a fragment of what we said (captions arrive
    /// incrementally): share of its own words that must match in order.
    public static let fragment = 0.8

    private struct Phrase: Equatable {
        var tokens: [String]
        var at: Double
    }

    private var phrases: [Phrase] = []
    /// The microphone phrase in progress (replaced by each partial result).
    private var partial: Phrase?
    private var talking = false
    private var openUntil = -Double.infinity
    /// Visitor captions dropped as echo (diagnostics, tests).
    public private(set) var dropped = 0

    public init() {}

    /// We said or typed `text` at `now`: a final microphone phrase, a
    /// volatile one (`final: false`, replaces the previous volatile one), or
    /// a typed reply (`final: true`).
    public mutating func said(_ text: String, now: Double, final: Bool = true) {
        let tokens = Self.tokens(text)
        guard !tokens.isEmpty else {
            if !final { partial = nil }
            return
        }
        openUntil = max(openUntil, now + Self.window)
        if final {
            partial = nil
            phrases.append(Phrase(tokens: tokens, at: now))
        } else {
            partial = Phrase(tokens: tokens, at: now)
        }
        phrases.removeAll { now - $0.at > Self.memory }
    }

    /// Whether talk-back is being asked for, starting or playing (until the
    /// core's `stopped` event, tail included). Going quiet starts the window.
    public mutating func talking(_ on: Bool, now: Double) {
        if talking && !on { openUntil = max(openUntil, now + Self.window) }
        talking = on
    }

    /// Whether our own talk-back could be coming back at `now`.
    public func isOpen(now: Double) -> Bool { talking || now <= openUntil }

    /// Whether the visitor caption `text` is our words coming back.
    public func isEcho(_ text: String, now: Double) -> Bool {
        guard isOpen(now: now) else { return false }
        let heard = Self.tokens(text)
        guard !heard.isEmpty else { return false }
        var candidates = phrases.filter { now - $0.at <= Self.memory }.map(\.tokens)
        if let p = partial { candidates.append(p.tokens) }
        // Phrases said one after the other can come back as one caption.
        if candidates.count > 1 { candidates.append(candidates.flatMap { $0 }) }
        return candidates.contains { Self.matches(heard: heard, said: $0) }
    }

    /// `isEcho`, counting the drop.
    public mutating func filter(_ text: String, now: Double) -> Bool {
        let echo = isEcho(text, now: now)
        if echo { dropped += 1 }
        return echo
    }

    // MARK: Matching

    static func matches(heard: [String], said: [String]) -> Bool {
        guard !heard.isEmpty, !said.isEmpty else { return false }
        let common = Double(lcs(heard, said))
        if common / Double(max(heard.count, said.count)) >= similarity { return true }
        return common >= 2 && common / Double(heard.count) >= fragment
    }

    /// Lowercase words without punctuation, digits as words ("1 2" ->
    /// "one two"; 0-99).
    static func tokens(_ text: String) -> [String] {
        var out: [String] = []
        var word = ""
        func flush() {
            if !word.isEmpty { out.append(contentsOf: expand(word)) }
            word = ""
        }
        for ch in text.lowercased() {
            if ch.isLetter || ch.isNumber {
                word.append(ch)
            } else if ch == "'" || ch == "\u{2019}" {
                continue
            } else {
                flush()
            }
        }
        flush()
        return out
    }

    private static let ones = [
        "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
        "thirteen", "fourteen", "fifteen", "sixteen", "seventeen", "eighteen", "nineteen",
    ]
    private static let tens = ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"]

    private static func expand(_ word: String) -> [String] {
        guard word.count <= 2, word.allSatisfy({ $0.isASCII && $0.isNumber }), let n = Int(word) else { return [word] }
        if n < 20 { return [ones[n]] }
        return n % 10 == 0 ? [tens[n / 10]] : [tens[n / 10], ones[n % 10]]
    }

    private static func sameWord(_ a: String, _ b: String) -> Bool {
        if a == b { return true }
        // Recognizers garble: one letter off counts for words of four or more.
        guard a.count >= 4, b.count >= 4, abs(a.count - b.count) <= 1 else { return false }
        return editDistanceAtMostOne(Array(a), Array(b))
    }

    private static func editDistanceAtMostOne(_ a: [Character], _ b: [Character]) -> Bool {
        var i = 0
        while i < a.count, i < b.count, a[i] == b[i] { i += 1 }
        if a.count == b.count { return Array(a[(i + 1)...]) == Array(b[(i + 1)...]) }
        let (long, short) = a.count > b.count ? (a, b) : (b, a)
        return Array(long[(i + 1)...]) == Array(short[i...])
    }

    static func lcs(_ a: [String], _ b: [String]) -> Int {
        var row = [Int](repeating: 0, count: b.count + 1)
        for x in a {
            var diagonal = 0
            for (j, y) in b.enumerated() {
                let above = row[j + 1]
                row[j + 1] = sameWord(x, y) ? diagonal + 1 : max(above, row[j])
                diagonal = above
            }
        }
        return row[b.count]
    }
}
