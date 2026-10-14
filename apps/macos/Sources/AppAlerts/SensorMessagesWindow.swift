import AppKit
import NVRCore
import SwiftUI

// A sensor's message log (docs/UX.md "Smart alerts", "Sensor messages"): the
// last messages its MQTT input received, each with what became of it. Opened
// from Settings > Smart Alerts > Sensors and from the Events window. The log
// lives in the core's memory only (payloads may be private): the window shows
// it and never saves it.

@MainActor
@Observable
package final class SensorMessagesModel {
    @ObservationIgnored package let app: any AlertsHost
    package let key: String
    package private(set) var log = SensorMessageLog()
    package var filter: SensorOutcomeFilter = .all
    /// The core cannot say (smart alerts off, or no such sensor).
    package private(set) var unavailable = false
    @ObservationIgnored private var timer: Timer?

    package init(app: any AlertsHost, key: String) {
        self.app = app
        self.key = key
        refresh()
        // Live updating: the core is asked for what came after the last
        // message about once a second.
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.refresh() }
        }
    }

    package func stop() {
        timer?.invalidate()
        timer = nil
    }

    /// Fetches the messages that came since the last refresh (all pages).
    package func refresh() {
        let alerts = app.alertsModel!
        var guardCount = 0
        while guardCount < 10 {
            guardCount += 1
            guard let page = alerts.sensorMessages(key: key, after: log.nextSeq) else {
                unavailable = true
                return
            }
            unavailable = false
            log.merge(page)
            if !page.more { break }
        }
        alerts.refreshStatus()
    }

    package func clear() {
        _ = app.alertsModel.clearSensorMessages(key: key)
        log.clear()
    }

    package var shown: [SensorMessage] { log.shown(filter) }

    package var title: String {
        let name = app.alertsModel.settings.sensors.first { $0.key == key }?.name ?? ""
        return "Sensor Messages: \(name.isEmpty ? key : name)"
    }

    /// The connection line above the list.
    package var connection: String? {
        SensorConnection.find(id: key, in: app.alertsModel.status)?.line()
    }

    package static func time(_ ms: Int64) -> String {
        let f = DateFormatter()
        f.dateStyle = .none
        f.timeStyle = .medium
        return f.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }
}

@MainActor
package final class SensorMessagesWindowController: NSWindowController, NSWindowDelegate {
    package let messages: SensorMessagesModel
    package var onClose: (SensorMessagesWindowController) -> Void = { _ in }

    package init(app: any AlertsHost, key: String) {
        messages = SensorMessagesModel(app: app, key: key)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 760, height: 480),
            styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = messages.title
        window.isReleasedWhenClosed = false
        window.contentMinSize = NSSize(width: 520, height: 300)
        super.init(window: window)
        let hosting = NSHostingController(rootView: SensorMessagesView(messages: messages))
        hosting.sizingOptions = []
        window.contentViewController = hosting
        window.setContentSize(NSSize(width: 760, height: 480))
        window.delegate = self
    }

    @available(*, unavailable)
    package required init?(coder: NSCoder) { fatalError("not used") }

    package func windowWillClose(_ notification: Notification) {
        messages.stop()
        onClose(self)
    }
}

package struct SensorMessagesView: View {
    @Bindable package var messages: SensorMessagesModel

    package var body: some View {
        VStack(spacing: 0) {
            HStack {
                Picker("Show", selection: $messages.filter) {
                    ForEach(SensorOutcomeFilter.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                .frame(maxWidth: 260)
                Spacer()
                Text("\(messages.shown.count) of \(messages.log.entries.count) messages").foregroundStyle(.secondary)
                Button("Clear") { messages.clear() }
                    .help("Empties the log. New messages still arrive.")
            }
            .padding(8)
            if let line = messages.connection {
                Text(line).font(.callout).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading).padding([.horizontal, .bottom], 8)
            }
            Divider()
            if messages.unavailable {
                Text("The core has no log for this sensor. Turn smart alerts on and save the sensor first.")
                    .foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if messages.shown.isEmpty {
                Text(messages.log.entries.isEmpty ? "No messages yet." : "No messages match.")
                    .foregroundStyle(.secondary).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                List(messages.shown) { m in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(SensorMessagesModel.time(m.atMs)).font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary).frame(width: 80, alignment: .leading)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(m.topic).font(.callout).lineLimit(1)
                            Text(m.payload + (m.truncated ? "… (\(m.payloadLength) bytes)" : ""))
                                .font(.system(.caption, design: .monospaced)).lineLimit(3).textSelection(.enabled)
                        }
                        Spacer(minLength: 8)
                        Text(m.outcomeText)
                            .font(.callout).foregroundStyle(m.matched ? Color.green : Color.secondary)
                            .multilineTextAlignment(.trailing).frame(maxWidth: 280, alignment: .trailing)
                    }
                }
            }
        }
    }
}
