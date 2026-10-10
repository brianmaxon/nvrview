import Foundation

// LAN device discovery as Swift sees it (docs/FFI.md "Device discovery", minor
// 1 of ABI 3): the events a search sends, and `DiscoveryList`, the state a
// "find devices" screen shows. No C types; everything here is testable
// without the library.

/// How a found device can be reached (`services` of a `discovered` event).
public struct DiscoveredService: Sendable, Equatable {
    /// `native` (the port-9000 protocol) or `onvif`; more later.
    public var kind: String
    public var port: UInt16?
    public var url: String?

    public init(kind: String, port: UInt16? = nil, url: String? = nil) {
        self.kind = kind
        self.port = port
        self.url = url
    }
}

/// A device that answered a search. The core never sends device IDs or MAC
/// addresses, so there are none here.
public struct DiscoveredDevice: Sendable, Equatable, Identifiable {
    public enum Kind: String, Sendable { case nvr, camera, unknown }

    /// The search that found it.
    public var search: UInt64
    /// Opaque; the same device keeps its key for the whole search.
    public var key: String
    public var kind: Kind
    public var name: String?
    public var model: String?
    public var address: String
    /// The native protocol's TCP port, when the answer said.
    public var port: UInt16?
    public var channels: Int?
    public var batteryPercent: Int?
    public var services: [DiscoveredService]

    public var id: String { key }

    public init(
        search: UInt64, key: String, kind: Kind = .unknown, name: String? = nil, model: String? = nil,
        address: String, port: UInt16? = nil, channels: Int? = nil, batteryPercent: Int? = nil,
        services: [DiscoveredService] = []
    ) {
        self.search = search
        self.key = key
        self.kind = kind
        self.name = name
        self.model = model
        self.address = address
        self.port = port
        self.channels = channels
        self.batteryPercent = batteryPercent
        self.services = services
    }

    /// The viewer speaks only the native protocol so far; a device that
    /// offered nothing else (an ONVIF camera) cannot be added.
    public var isNative: Bool { services.isEmpty || services.contains { $0.kind == "native" } }

    /// The port to fill into an add form: the answer's, else the usual 9000.
    public var suggestedPort: UInt16 { port ?? 9000 }

    /// What to call it in a list.
    public var title: String {
        if let name, !name.isEmpty { return name }
        switch kind {
        case .nvr: return "NVR"
        case .camera: return "Camera"
        case .unknown: return "Device"
        }
    }
}

/// How a search ended (`discovery_done`).
public struct DiscoveryEnd: Sendable, Equatable {
    public var search: UInt64
    public var count: Int
    public var stopped: Bool
    /// Per-interface problems (interface names and addresses: for a details
    /// view, never for logs above debug level).
    public var errors: [String]

    public init(search: UInt64, count: Int, stopped: Bool, errors: [String] = []) {
        self.search = search
        self.count = count
        self.stopped = stopped
        self.errors = errors
    }
}

/// What a "find devices" screen shows: the devices of the current search and
/// whether it is still running.
///
/// The app calls `requested()` after each successful start and `stopped()`
/// after its own stop; `apply` takes every core event. Starts in quick
/// succession are told apart by `discovery_started`: only the events of the
/// last requested search count, so a stopped search's tail never lands in
/// the new list.
public struct DiscoveryList: Sendable, Equatable {
    public enum Phase: Sendable, Equatable {
        /// No search has been requested, or it was stopped.
        case idle
        case searching
        /// The search ran its time out (or failed to start).
        case finished(DiscoveryEnd)
    }

    public private(set) var phase = Phase.idle
    public private(set) var devices: [DiscoveredDevice] = []
    /// The core could not start a search (it predates discovery, or refused).
    public private(set) var unavailable = false
    private var pendingStarts = 0
    private var current: UInt64?

    public init() {}

    /// A search was started (`nv_discover_start` returned OK). The list
    /// empties and shows a search in progress.
    public mutating func requested() {
        pendingStarts += 1
        current = nil
        devices = []
        unavailable = false
        phase = .searching
    }

    /// A start was refused: nothing runs, and the screen says it cannot search.
    public mutating func failedToStart() {
        current = nil
        devices = []
        unavailable = true
        phase = .idle
    }

    /// The app stopped the search itself.
    public mutating func stopped() {
        if phase == .searching { phase = .idle }
    }

    public mutating func apply(_ event: CoreEvent) {
        switch event {
        case .discoveryStarted(let search):
            guard pendingStarts > 0 else { return }
            pendingStarts -= 1
            if pendingStarts == 0 { current = search }
        case .discovered(let device):
            guard phase == .searching, device.search == current else { return }
            if let i = devices.firstIndex(where: { $0.key == device.key }) {
                devices[i] = device
            } else {
                devices.append(device)
            }
        case .discoveryDone(let end):
            guard end.search == current, phase == .searching else { return }
            phase = .finished(end)
        default:
            break
        }
    }

    /// True while a search runs.
    public var isSearching: Bool { phase == .searching }

    /// A search that ended with nothing: the usual cause on macOS is that
    /// Local Network access is still waiting for the user (the system drops
    /// broadcasts silently), so the screen points there.
    public var foundNothing: Bool {
        if case .finished(let end) = phase { return devices.isEmpty && end.count == 0 }
        return false
    }

    /// Nothing found, and the system refused the sends ("No route to host"
    /// with no route problem to explain it): macOS answers that way while
    /// Local Network access is denied or still waiting for the user.
    public var permissionLikelyBlocked: Bool {
        foundNothing && errors.contains { $0.localizedCaseInsensitiveContains("no route to host") }
    }

    /// The problems the last search reported, for a details view.
    public var errors: [String] {
        if case .finished(let end) = phase { return end.errors }
        return []
    }
}

extension DiscoveredDevice {
    /// Whether the user already added this device: same address and port
    /// (a host name in a stored device never matches an address; the user
    /// would see it listed, which is harmless).
    public func isAdded<S: Sequence>(among devices: S) -> Bool where S.Element == (host: String, port: UInt16) {
        devices.contains { $0.host.lowercased() == address.lowercased() && $0.port == suggestedPort }
    }
}
