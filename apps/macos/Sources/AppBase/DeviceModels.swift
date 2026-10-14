import Foundation
import NVRCore
import VideoKit

/// One configured device and what the core says about it.
package struct DeviceRuntime: Identifiable {
    package var config: DeviceConfig
    /// The per-run device ID while the device is added to the core.
    package var deviceID: UInt64?
    package var link = DeviceLink()
    package var reportedName: String?
    package var model: String?
    package var channels: [ChannelInfo] = []

    package init(
        config: DeviceConfig, deviceID: UInt64? = nil, link: DeviceLink = DeviceLink(), reportedName: String? = nil,
        model: String? = nil, channels: [ChannelInfo] = []
    ) {
        self.config = config
        self.deviceID = deviceID
        self.link = link
        self.reportedName = reportedName
        self.model = model
        self.channels = channels
    }

    /// The name layouts and groups give the device's cameras: the core's
    /// stable device key once known (`nv_device_key`), else the app's.
    package var key: String { config.coreKey ?? config.key }
    package var id: String { config.key }
}

/// What the user set for one camera (saved in the app's data directory). The
/// doorbell options belong to a channel (its first lens).
package struct CameraOptions: Codable, Equatable, Sendable {
    package var camera: CameraRef
    /// A person on this camera opens an alert inset (docs/UX.md "Alert insets").
    package var entrance = false
    /// A doorbell: audible when it rings even if nothing was playing.
    package var unmuteOnRing = false
    /// A doorbell: talk-back allowed (camera motion control; off by default).
    package var allowTalk = false
    /// Automations (`auto_correction`, a local API client) the user let move
    /// this camera (camera motion control; none by default). The user's own
    /// pan, tilt, lens and preset controls need no grant.
    package var motionGrants: [String] = []
    /// A fisheye camera: how it is mounted (docs/UX.md "Fisheye cameras").
    package var fisheyeMount = FisheyeMount.ceiling
    /// Where its calls open: true in their own window, false in the insets,
    /// nil as "Open calls in their own window" says (the default).
    package var callWindow: Bool?

    /// The override for `CallPresentation.resolve`.
    package var callPresentation: CallPresentation? { callWindow.map { $0 ? .window : .inset } }

    package init(
        camera: CameraRef, entrance: Bool = false, unmuteOnRing: Bool = false, allowTalk: Bool = false,
        fisheyeMount: FisheyeMount = .ceiling
    ) {
        self.camera = camera
        self.entrance = entrance
        self.unmuteOnRing = unmuteOnRing
        self.allowTalk = allowTalk
        self.fisheyeMount = fisheyeMount
    }

    private enum CodingKeys: String, CodingKey {
        case camera, entrance, unmuteOnRing, allowTalk, fisheyeMount, callWindow, motionGrants
    }

    package init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        camera = try c.decode(CameraRef.self, forKey: .camera)
        entrance = try c.decodeIfPresent(Bool.self, forKey: .entrance) ?? false
        unmuteOnRing = try c.decodeIfPresent(Bool.self, forKey: .unmuteOnRing) ?? false
        allowTalk = try c.decodeIfPresent(Bool.self, forKey: .allowTalk) ?? false
        motionGrants = try c.decodeIfPresent([String].self, forKey: .motionGrants) ?? []
        fisheyeMount = (try? c.decodeIfPresent(FisheyeMount.self, forKey: .fisheyeMount)) ?? .ceiling
        callWindow = try c.decodeIfPresent(Bool.self, forKey: .callWindow)
    }

    package var isDefault: Bool {
        !entrance && !unmuteOnRing && !allowTalk && fisheyeMount == .ceiling && callWindow == nil && motionGrants.isEmpty
    }
}
