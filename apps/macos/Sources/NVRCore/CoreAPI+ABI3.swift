// The ABI 3 additions (docs/FFI-ABI3.md) as Swift sees them, with defaults
// for cores that predate them.
//
// `CCore` implements them over the C functions (docs/FFI.md, ABI 3);
// `FakeCore` models them. A core without them gets these defaults, and the
// app degrades: no tier switch (focus keeps the grid's stream), no level
// meter, layouts kept only for the session, app-assigned device keys.

/// A tile's audio level for the meter (`nv_tile_audio_level`), 0...1.
public struct AudioLevel: Sendable, Equatable {
    public var peak: Float
    public var rms: Float
    public init(peak: Float, rms: Float) {
        self.peak = peak
        self.rms = rms
    }
    public static let silent = AudioLevel(peak: 0, rms: 0)
}

extension CoreAPI {
    public func tileDecoded(_ tile: UInt64, generation: UInt32, ptsMicros: Int64) -> CoreStatus {
        tileDecoded(tile, ptsMicros: ptsMicros)
    }

    public func tileDecodeFailed(_ tile: UInt64, generation: UInt32, kind: CoreDecodeFailure) -> CoreStatus {
        tileDecodeFailed(tile, kind: kind)
    }

    public func tileSetTier(_ tile: UInt64, tier: StreamTier) throws(CoreStatus) { throw .invalidArgument }
    public func tileAudioLevel(_ tile: UInt64) throws(CoreStatus) -> AudioLevel { throw .invalidArgument }
    public func deviceKey(_ device: UInt64) throws(CoreStatus) -> String { throw .invalidArgument }
    public func layoutsGet() throws(CoreStatus) -> String { throw .invalidArgument }
    public func layoutPut(json: String) throws(CoreStatus) { throw .invalidArgument }
    public func layoutDelete(id: String) throws(CoreStatus) { throw .invalidArgument }
}

// Device discovery (minor 1). A core without it refuses, and the app shows
// no list.
extension CoreAPI {
    public func discoverStart(json: String?) throws(CoreStatus) { throw .invalidArgument }
    public func discoverStop() throws(CoreStatus) {}
}

// Updates (minor 2). A core without them checks nothing, and the app's
// update settings say so.
extension CoreAPI {
    public func updateCheck(json: String) throws(CoreStatus) { throw .invalidArgument }
    public func updateDownload(json: String) throws(CoreStatus) { throw .invalidArgument }
    public func updateCancel() throws(CoreStatus) {}
    public func updateParseKey(_ text: String) -> String? { nil }
    public func updateParseSource(_ text: String) -> String {
        #"{"ok":false,"reason":"source_invalid","message":"this build cannot update itself"}"#
    }
}

// Talk-back (docs/FFI.md "Talk-back"): camera motion control, off for every
// camera until the app passes the user's per-camera permission. A core
// without it refuses everything, so nothing is ever sent.
extension CoreAPI {
    public func talkSetPermission(device: UInt64, channel: UInt8, allowed: Bool) throws(CoreStatus) {
        throw .invalidArgument
    }
    public func talkStart(_ tile: UInt64) throws(CoreStatus) { throw .notPermitted }
    public func talkPush(_ tile: UInt64, samples: UnsafeBufferPointer<Float>, sampleRate: UInt32) throws(CoreStatus) {
        throw .invalidArgument
    }
    public func talkStop(_ tile: UInt64, drain: Bool) throws(CoreStatus) { throw .invalidArgument }
}
