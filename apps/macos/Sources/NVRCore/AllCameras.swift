import Foundation

/// What "All Cameras" needs to know about one device.
public struct AllCamerasDevice: Sendable, Equatable {
    public var key: String
    /// Whether the device link is connected. Channels of a device that is
    /// not connected are not known to be online, so they are left out until
    /// it connects.
    public var connected: Bool
    public var channels: [ChannelInfo]

    public init(key: String, connected: Bool, channels: [ChannelInfo]) {
        self.key = key
        self.connected = connected
        self.channels = channels
    }
}

/// The "All Cameras" arrangement: every camera (every lens) of every online
/// channel of every connected device, in sidebar order, capped at the
/// layout model's leaf limit.
public enum AllCameras {
    public static let name = "All Cameras"

    /// The cameras of one channel: one per lens.
    public static func cameras(device key: String, channel c: ChannelInfo) -> [CameraRef] {
        (0..<max(min(c.lenses, CameraRef.maxLens + 1), 1)).map { CameraRef(device: key, channel: c.channel, lens: $0) }
    }

    /// The cameras of one device's online channels (none when the device is
    /// not connected).
    public static func cameras(of device: AllCamerasDevice) -> [CameraRef] {
        guard device.connected else { return [] }
        return device.channels.filter(\.connected).flatMap { cameras(device: device.key, channel: $0) }
    }

    /// The cameras of every device, in order, up to `limit`.
    public static func select(_ devices: [AllCamerasDevice], limit: Int = LayoutNode.maxLeaves) -> [CameraRef] {
        var out: [CameraRef] = []
        for d in devices {
            for c in cameras(of: d) {
                if out.count >= limit { return out }
                out.append(c)
            }
        }
        return out
    }
}

/// Keeps an unedited All Cameras window in step with the devices: it
/// rebuilds only when the camera list changes, and never after the user has
/// edited the window.
public struct AllCamerasWatch: Sendable, Equatable {
    public private(set) var shown: [CameraRef]
    public private(set) var edited = false

    public init(shown: [CameraRef]) { self.shown = shown }

    /// The user split, swapped, resized, or otherwise edited the window.
    public mutating func markEdited() { edited = true }

    /// Offers the current list; true when the window should be rebuilt
    /// from it (it differs and the window is unedited).
    public mutating func refresh(to cameras: [CameraRef]) -> Bool {
        guard !edited, cameras != shown else { return false }
        shown = cameras
        return true
    }
}
