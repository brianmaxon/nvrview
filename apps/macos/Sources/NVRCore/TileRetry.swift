import Foundation

/// When a tile that shows no picture offers a Retry button (docs/UX.md,
/// "Tiles without a picture"). Pure rules on the viewer clock, so the delay
/// is tested without waiting.
public enum TileRetry {
    /// How long a tile must have been without a picture (and with a message
    /// a retry could help) before the button appears.
    public static let offerAfter: TimeInterval = 30

    /// Whether the message over a tile is one a retry can help: connecting,
    /// reconnecting, and a connected device whose stream has not delivered a
    /// picture. Never for a refused login (that state is terminal, to protect
    /// the account), a closed or removed device, an offline camera, or a
    /// stream the app cannot decode.
    public static func applies(link: DeviceLink, status: TileStatus?, hasPicture: Bool) -> Bool {
        switch link.state {
        case nil, .connecting?, .backoff?:
            return true
        case .loginFailed?, .closed?, .other?:
            return false
        case .connected?:
            switch status {
            case nil, .waiting?, .recovering?: return !hasPicture
            case .playing?, .undecodable?: return false
            }
        }
    }

    /// How long each camera's tile has had a message a retry could help.
    public struct Watch: Sendable, Equatable {
        private var since: [CameraRef: TimeInterval] = [:]

        public init() {}

        /// Notes the tile's state at `now` and says whether the button shows.
        public mutating func offered(_ camera: CameraRef, applies: Bool, now: TimeInterval) -> Bool {
            guard applies else {
                since[camera] = nil
                return false
            }
            let start = since[camera] ?? now
            since[camera] = start
            return now - start >= TileRetry.offerAfter
        }

        /// The user retried: the delay starts again.
        public mutating func restart(_ camera: CameraRef, now: TimeInterval) {
            if since[camera] != nil { since[camera] = now }
        }

        /// Cameras no longer on screen need no entry.
        public mutating func keep(_ cameras: Set<CameraRef>) {
            since = since.filter { cameras.contains($0.key) }
        }
    }
}
