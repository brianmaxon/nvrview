import NVRCore
import SwiftUI

// Settings > Smart Alerts > Live checks (docs/UX.md "Live checks"): where a
// check shows, how much of it, when it pops up, and how long it stays.

package struct SmartAlertsLiveViewSettings: View {
    package let alerts: SmartAlertsModel

    package var body: some View {
        Form {
            Section("Live checks") {
                Picker("Show checks", selection: setting(alerts, \.view.placement)) {
                    ForEach(VerifyPlacementSetting.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                Picker("Show in each check", selection: setting(alerts, \.view.detail)) {
                    ForEach(VerifyDetailSetting.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                .help("A single headline reads \"Confirmed: person at <camera>\", \"Not confirmed: ...\", or \"Checking: ...\"; the full log stays in the Smart Alert Events window.")
                Picker("Open an inset or window", selection: setting(alerts, \.view.popUp)) {
                    ForEach(VerifyPopUpSetting.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                .help("Only when the alert is confirmed: a check runs without an inset or window, and one opens when it confirms. A check that is not confirmed never opens one.")
                Toggle("Also keep checks that end quiet or suppressed", isOn: setting(alerts, \.view.showQuiet))
                Picker("Stay after the decision", selection: setting(alerts, \.view.minSeconds)) {
                    ForEach(VerifyViewSettings.stayChoices(including: alerts.settings.view.minSeconds), id: \.self) {
                        Text(VerifyViewSettings.stayTitle($0)).tag($0)
                    }
                }
                caption(
                    "Each check shows the camera's picture with the detector's boxes and every step from the trigger to the decision. It stays longer while the alert is still going, never takes the keyboard, and closes with its × button, Esc, or Dismiss All. Every check also says how long ago it started (\"12 s ago\", \"3 min ago\")."
                )
                caption(
                    "\"Only when the alert is confirmed\" holds back the inset and the small window: the check runs unseen, and the inset opens when it confirms the alert, showing the verdict and the log as chosen above. A check that is not confirmed never opens one, though the Smart Alert Events window still logs it and no notification is sent. A camera that is already in a tile keeps its live band there while it checks."
                )
            }
        }
        .formStyle(.grouped)
    }

    package init(alerts: SmartAlertsModel) {
        self.alerts = alerts
    }
}
