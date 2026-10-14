import AlertHost
import Foundation

/// The credentials smart alerts use (an MQTT password, a Home Assistant
/// access token), as the host asks for them: the app's credential store
/// (the Keychain, under the app's Keychain service) with the account
/// `alerts.<id>`, where `<id>` is the `id` of the input or output entry.
/// Settings > Smart Alerts saves them there (`SmartAlertsModel.setSecret`).
/// Read each time the settings are sent, never cached, so a changed password
/// counts at once; never written to a settings file or a log.
///
/// The host builds the core's document on the main actor, where this is
/// asked.
package final class ModelAlertSecrets: AlertSecrets, @unchecked Sendable {
    private weak var model: (any AlertsHost)?

    @MainActor
    package init(model: any AlertsHost) { self.model = model }

    package func secret(for id: String) -> String? {
        MainActor.assumeIsolated { model?.alertsModel.secret(id: id) }
    }
}
