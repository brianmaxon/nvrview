import Foundation
import Synchronization
import UserNotifications

/// One desktop notification for an alert.
public struct AlertNotification: Sendable, Equatable {
    /// The alert's ID: a later revision replaces the earlier notification.
    public var alertID: UInt64
    public var title: String
    /// The explanation the core built from the trace.
    public var body: String
    /// A critical alert is time-sensitive.
    public var critical: Bool
    /// A JPEG of the alert, when the host made one.
    public var snapshot: Data?
    /// Notifications of one camera group together.
    public var thread: String

    public init(alertID: UInt64, title: String, body: String, critical: Bool, snapshot: Data?, thread: String) {
        self.alertID = alertID
        self.title = title
        self.body = body
        self.critical = critical
        self.snapshot = snapshot
        self.thread = thread
    }
}

public protocol AlertNotifying: Sendable {
    /// Asks the system for permission (once; a later call returns the answer).
    func authorize() async -> Bool
    func post(_ notification: AlertNotification) async
}

/// Keeps what it was given, for tests and the self-test (no system dialog).
public final class RecordingNotifier: AlertNotifying {
    private let posted = Mutex<[AlertNotification]>([])
    public init() {}
    public func authorize() async -> Bool { true }
    public func post(_ notification: AlertNotification) async { posted.withLock { $0.append(notification) } }
    public var notifications: [AlertNotification] { posted.withLock { $0 } }
}

/// `UNUserNotificationCenter`. Critical alerts are time-sensitive (they
/// break through a Focus when the app has the entitlement). A process that
/// is not an app bundle (a test run, `swift run`) has no notification
/// center: posting does nothing there.
public final class SystemNotifier: AlertNotifying {
    public init() {}

    private var center: UNUserNotificationCenter? {
        Bundle.main.bundleURL.pathExtension == "app" && Bundle.main.bundleIdentifier != nil
            ? UNUserNotificationCenter.current() : nil
    }

    public func authorize() async -> Bool {
        guard let center else { return false }
        return (try? await center.requestAuthorization(options: [.alert, .sound])) ?? false
    }

    public func post(_ n: AlertNotification) async {
        guard let center else { return }
        let content = UNMutableNotificationContent()
        content.title = n.title
        content.body = n.body
        content.threadIdentifier = n.thread
        content.interruptionLevel = n.critical ? .timeSensitive : .active
        content.sound = .default
        let identifier = "nvrview-alert-\(n.alertID)"
        if let jpeg = n.snapshot {
            let file = FileManager.default.temporaryDirectory.appendingPathComponent("\(identifier)-\(UUID().uuidString).jpg")
            if (try? jpeg.write(to: file)) != nil,
                let attachment = try? UNNotificationAttachment(identifier: "snapshot", url: file)
            {
                content.attachments = [attachment]
            }
        }
        try? await center.add(UNNotificationRequest(identifier: identifier, content: content, trigger: nil))
    }
}
