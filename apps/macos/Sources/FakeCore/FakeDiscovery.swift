import Foundation
import NVRCore
import Synchronization

// Device discovery (minor 1) in the fake core: a made-up network. Searches
// send `discovery_started`, one `discovered` per made-up device, and end with
// `discovery_done` when the app stops them, when `finishDiscovery` says the
// time is up, or (with `Options.discoveryDuration`) after a real timer.

/// A made-up device on the made-up network. Documentation addresses only.
public struct FakeDiscoverable: Sendable {
    public var kind: String
    public var name: String?
    public var model: String?
    public var address: String
    public var port: Int?
    public var channels: Int?
    public var batteryPercent: Int?
    public var services: [(kind: String, port: Int?)]

    public init(
        kind: String, name: String?, model: String? = nil, address: String, port: Int? = 9000,
        channels: Int? = nil, batteryPercent: Int? = nil, services: [(kind: String, port: Int?)]? = nil
    ) {
        self.kind = kind
        self.name = name
        self.model = model
        self.address = address
        self.port = port
        self.channels = channels
        self.batteryPercent = batteryPercent
        self.services = services ?? [("native", port)]
    }

    /// Two recorders (the addresses the fake core's own devices use, so the
    /// list shows them as already added), a battery camera that answered only
    /// the probe, and an ONVIF-only camera.
    public static let samples: [FakeDiscoverable] = [
        FakeDiscoverable(kind: "nvr", name: "NVR 1", address: "192.0.2.10", channels: 6),
        FakeDiscoverable(kind: "nvr", name: "NVR 3", address: "192.0.2.12", channels: 16),
        FakeDiscoverable(
            kind: "camera", name: nil, address: "192.0.2.30", port: nil, batteryPercent: 80,
            services: [("native", nil)]),
        FakeDiscoverable(
            kind: "camera", name: "Camera 2", model: "EXAMPLE-CAM", address: "192.0.2.40", port: nil,
            services: [("onvif", 8000)]),
    ]

    fileprivate var json: String {
        func s(_ v: String) -> String {
            let data = (try? JSONSerialization.data(withJSONObject: [v])) ?? Data("[\"\"]".utf8)
            return String(decoding: data.dropFirst().dropLast(), as: UTF8.self)
        }
        var out = #""kind":\#(s(kind)),"address":\#(s(address))"#
        if let name { out += #","name":\#(s(name))"# }
        if let model { out += #","model":\#(s(model))"# }
        if let port { out += #","port":\#(port)"# }
        if let channels { out += #","channels":\#(channels)"# }
        if let batteryPercent { out += #","battery_percent":\#(batteryPercent)"# }
        let services = self.services.map { svc in
            svc.port.map { #"{"kind":\#(s(svc.kind)),"port":\#($0)}"# } ?? #"{"kind":\#(s(svc.kind))}"#
        }
        return out + #","services":[\#(services.joined(separator: ","))]"#
    }
}

/// What the fake core remembers about its one search.
final class FakeDiscoveryState: Sendable {
    struct Running: Sendable {
        var search: UInt64
        var count: Int
    }
    let nextSearch = Atomic<UInt64>(1)
    let running = Mutex<Running?>(nil)
}

extension FakeCore {
    public func discoverStart(json: String?) throws(CoreStatus) {
        // The request is validated as the real core does (an object whose
        // members have the right types).
        if let json, !json.trimmingCharacters(in: .whitespaces).isEmpty {
            guard let object = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] else {
                throw .invalidArgument
            }
            if let s = object["seconds"] {
                guard let seconds = s as? Double, (0.5...30).contains(seconds) else { throw .invalidArgument }
            }
            if let i = object["interfaces"], !(i is [String]) { throw .invalidArgument }
            if let o = object["onvif"], !(o is Bool) { throw .invalidArgument }
        }
        try discoverEnd(stopped: true)
        let devices = options.discoverable
        let search = discovery.nextSearch.wrappingAdd(1, ordering: .relaxed).oldValue
        discovery.running.withLock { $0 = .init(search: search, count: devices.count) }
        emit(event: #"{"type":"discovery_started","search":\#(search)}"#)
        for (i, d) in devices.enumerated() {
            emit(event: #"{"type":"discovered","search":\#(search),"key":"d\#(i + 1)",\#(d.json)}"#)
        }
        if let duration = options.discoveryDuration {
            DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(Int(duration.milliseconds))) {
                [weak self] in
                self?.finishDiscovery(search: search)
            }
        }
    }

    public func discoverStop() throws(CoreStatus) { try discoverEnd(stopped: true) }

    /// The search's time is up: `discovery_done` (not stopped). Without
    /// `search`, whichever search runs.
    public func finishDiscovery(search: UInt64? = nil) {
        guard let running = discovery.running.withLock({ $0 }), search == nil || search == running.search else {
            return
        }
        try? discoverEnd(stopped: false)
    }

    /// Whether a search is running.
    public var isDiscovering: Bool { discovery.running.withLock { $0 != nil } }

    /// Ends the running search, if any, with its `discovery_done`.
    func discoverEnd(stopped: Bool) throws(CoreStatus) {
        let ended = discovery.running.withLock { r -> FakeDiscoveryState.Running? in
            defer { r = nil }
            return r
        }
        guard let ended else { return }
        emit(
            event:
                #"{"type":"discovery_done","search":\#(ended.search),"count":\#(ended.count),"stopped":\#(stopped),"errors":[]}"#
        )
    }
}

extension Duration {
    fileprivate var milliseconds: Int64 {
        let (s, a) = components
        return s * 1000 + a / 1_000_000_000_000_000
    }
}
