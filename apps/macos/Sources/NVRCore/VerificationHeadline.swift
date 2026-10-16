import Foundation

// The words a live check and an alert notification lead with
// (docs/UX.md "Live checks", "Notifications"): a headline that starts with
// the verdict, how long ago the alert fired, when a check pops up, and the
// notification's title that starts with the camera's name. Pure rules with
// unit tests; both apps use the same words.

extension VerifyCheck {
    /// "person", "CO", "smoke": the class in words; "activity" before the
    /// check knows.
    public var classWords: String {
        guard let raw = className ?? kind else { return "activity" }
        return raw == "co" ? "CO" : raw.replacingOccurrences(of: "_", with: " ")
    }

    /// Whether the check ended without confirming anything: the cameras did
    /// not show it (or could not be looked at), as opposed to a decision
    /// made for other reasons (a mute, a class that is off).
    private var endedUnconfirmed: Bool {
        switch reason {
        case nil, "not_confirmed", "nothing_seen", "outside_region", "could_not_look": true
        default: false
        }
    }

    /// The decision step said the alert went out without a confirmation (an
    /// always-alert case that could not be checked).
    private var alertedUnverified: Bool {
        steps.last { $0.step == "decision" }?.fields["unverified"]?.bool == true
    }

    /// One line that leads with the verdict, for the "single headline"
    /// setting:
    ///
    /// - "Checking: person at Front Door" while it runs;
    /// - "Confirmed: person at Front Door" once it alerts ("(not notified)"
    ///   when a cooldown kept it quiet; "Alert, not confirmed: ..." for an
    ///   always-alert case that could not be checked);
    /// - "Not confirmed: person at Front Door" when the check did not see it;
    /// - "Confirmed, quiet log: ..." and "Not alerted: ..." for the rest.
    ///
    /// The camera is left out when the check has none (a sensor's own
    /// alarm).
    public func verdictHeadline(cameraName: (AlertCamera) -> String) -> String {
        var what = classWords
        if let c = primaryCamera { what += " at \(cameraName(c))" }
        switch decision {
        case nil:
            return "Checking: \(what)"
        case "alert":
            if alertedUnverified { return "Alert, not confirmed: \(what)" }
            return "Confirmed: \(what)" + (quieted == nil ? "" : " (not notified)")
        case "quiet" where !endedUnconfirmed:
            return "Confirmed, quiet log: \(what)"
        default:
            if endedUnconfirmed { return "Not confirmed: \(what)" }
            return "Not alerted: \(what)"
        }
    }
}

extension VerificationBoard {
    /// Whether a check may open an inset or the small window now: always,
    /// or with "only when confirmed" once it has alerted. A check in a tile
    /// is not a pop-up: its band stays.
    public func mayPopUp(_ c: VerifyCheck) -> Bool {
        settings.popUp == .always || c.decision == "alert"
    }
}

/// "12 s ago", "3 min ago", "2 h ago": how long ago something happened.
public enum ElapsedText {
    /// Under a second "just now"; whole seconds under a minute; whole
    /// minutes under an hour; then whole hours (rounded down).
    public static func ago(seconds: Double) -> String {
        let s = Int(max(seconds, 0))
        if s < 1 { return "just now" }
        if s < 60 { return "\(s) s ago" }
        if s < 3600 { return "\(s / 60) min ago" }
        return "\(s / 3600) h ago"
    }

    /// The text for something that started at `start` (viewer clock).
    public static func ago(since start: Double, now: Double) -> String { ago(seconds: now - start) }
}

/// The desktop notification's title: the camera's name first.
public enum AlertText {
    /// "Front Door: Critical person alert"; with no name (a sensor's own
    /// alarm) "Critical smoke alarm". A simulated trigger says so:
    /// "Front Door: Test person alert", "Test: Smoke alarm".
    public static func title(cameraName: String?, className: String, severity: String, isTest: Bool) -> String {
        let noun: String
        switch className {
        case "smoke": noun = "smoke alarm"
        case "co": noun = "carbon monoxide alarm"
        case "visitor": noun = "doorbell"
        default: noun = "\(className) alert"
        }
        var text = severity == "critical" ? "Critical \(noun)" : noun
        let name = cameraName?.trimmingCharacters(in: .whitespaces).nilIfEmpty
        if let name {
            if isTest { text = "Test " + text.prefix(1).lowercased() + text.dropFirst() }
            return "\(name): " + text.prefix(1).uppercased() + text.dropFirst()
        }
        let title = text.prefix(1).uppercased() + text.dropFirst()
        return isTest ? "Test: \(title)" : title
    }
}

extension String {
    fileprivate var nilIfEmpty: String? { isEmpty ? nil : self }
}
