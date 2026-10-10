// The ABI 3 additions (docs/FFI-ABI3.md) as Swift sees them, with defaults
// for cores that predate them.
//
// Until the C header copy in NVRViewCore carries ABI 3, `CCore` cannot call
// the new functions; it gets these defaults, and the app degrades: no tier
// switch (focus keeps the grid's stream), no level meter, and layouts kept
// only for the session. `FakeCore` implements all of it.
//
// Contract note for the real core: `nv_tile_decoded` and
// `nv_tile_decode_failed` carry no generation. While a switch runs, both
// pipelines' Decodes are outstanding, a camera's main and sub streams carry
// the same camera timestamps (so `pts_us` does not tell them apart), and a
// failure carries no pts at all. The Swift interface therefore passes the
// generation with every answer; the C interface needs the same (an extra
// argument, or `nv_tile_decoded_gen`) for the core to account answers
// exactly. CCore drops it until then.

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
    public func layoutsGet() throws(CoreStatus) -> String { throw .invalidArgument }
    public func layoutPut(json: String) throws(CoreStatus) { throw .invalidArgument }
    public func layoutDelete(id: String) throws(CoreStatus) { throw .invalidArgument }
}
