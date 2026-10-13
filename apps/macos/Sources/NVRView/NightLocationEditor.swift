import Foundation
import NVRCore
import Observation

// The "Use a location for sunset and sunrise" part of Settings > Smart
// Alerts > Night, apart from the view so the self-test can drive it. Turning
// the toggle on stores nothing: the stored location (or the fixed night
// hours) stays in force until a Set that reads. Text that does not read
// leaves the fields as typed, the stored location as it was, and a message.

@MainActor
@Observable
final class NightLocationEditor {
    private let alerts: SmartAlertsModel
    /// The fields' text.
    var latText = ""
    var lonText = ""
    /// The user switched the location on (fields are shown) but may not have
    /// set one yet.
    private(set) var wanted: Bool
    /// Why the last Set was refused, in words; nil when it was not.
    private(set) var message: String?

    init(alerts: SmartAlertsModel) {
        self.alerts = alerts
        wanted = alerts.settings.location != nil
        load()
    }

    /// Whether the fields show.
    var isOn: Bool { wanted || alerts.settings.location != nil }

    /// The toggle. Turning it on shows empty fields and keeps what is stored.
    func setOn(_ on: Bool) {
        message = nil
        if on {
            wanted = true
            if alerts.settings.location == nil {
                latText = ""
                lonText = ""
            }
        } else {
            wanted = false
            alerts.update { $0.location = nil }
            latText = ""
            lonText = ""
        }
    }

    /// Shows the stored location in the fields.
    func load() {
        latText = alerts.settings.location.map { String($0.lat) } ?? latText
        lonText = alerts.settings.location.map { String($0.lon) } ?? lonText
    }

    /// The Set button (and Return in a field).
    func set() {
        switch CoordinateInput.resolve(lat: latText, lon: lonText) {
        case .success(let location):
            message = nil
            alerts.update { $0.location = location }
            wanted = true
            load()
        case .failure(let why):
            message = why.message
        }
    }
}
