/// The core as Swift sees it: one method per C function of docs/nvrview.h
/// (names follow `nv_core_new`, `nv_device_add`, `nv_tile_open`, ...).
///
/// Implementations: `CCore` (target NVRCoreC) calls the C library; `FakeCore`
/// (target FakeCore) is a Swift test double that plays files.
///
/// Threading, exactly as docs/FFI.md says: only `tileDecoded` and
/// `tileDecodeFailed` may be called from inside a sink callback (and from any
/// thread); everything else must come from outside, or it could wait on the
/// thread it runs on.
public protocol CoreAPI: Sendable {
    /// `nv_device_add`: connecting starts in the background.
    func addDevice(host: String, port: UInt16, user: String, password: String) throws(CoreStatus) -> UInt64

    /// `nv_device_remove`: the device's tiles end first.
    func removeDevice(_ device: UInt64) throws(CoreStatus)

    /// `nv_tile_open`: actions for the tile start arriving at the sink.
    func openTile(device: UInt64, channel: UInt8, tier: StreamTier) throws(CoreStatus) -> UInt64

    /// `nv_tile_close`: no action for the tile runs after this returns.
    func closeTile(_ tile: UInt64) throws(CoreStatus)

    /// `nv_tile_decoded`. Never blocks.
    @discardableResult
    func tileDecoded(_ tile: UInt64, ptsMicros: Int64) -> CoreStatus

    /// `nv_tile_decode_failed`. Never blocks.
    @discardableResult
    func tileDecodeFailed(_ tile: UInt64, kind: CoreDecodeFailure) -> CoreStatus

    /// `nv_grid_layout`: pure. One entry per picture size; a zero size is a
    /// tile with no picture yet. `fill` crops instead of letterboxing.
    func gridLayout(viewport: PixelSize, gap: UInt32, pictureSizes: [PixelSize], fill: Bool)
        throws(CoreStatus) -> [TileLayout]

    /// `nv_core_free`: stops everything, joins the core's threads; no
    /// callback runs after it returns. Idempotent. Never call it from a
    /// callback.
    func free()
}

/// Receives what the core sends (`NvCallbacks`). Both methods run on threads
/// the core owns and must return quickly; everything they are passed is
/// borrowed for the duration of the call.
public protocol CoreSink: AnyObject, Sendable {
    /// `on_action`: serialized per tile, never re-entrant.
    func core(didDeliver action: CoreAction, tile: UInt64)
    /// `on_event`: serialized across the core. UTF-8 JSON, not terminated.
    func core(didEmit json: UnsafeRawBufferPointer)
    /// An action that needs an answer but carries a value this version does
    /// not know (docs/FFI.md): answer it with `failure` so the core is not
    /// left waiting.
    func core(didReject failure: CoreDecodeFailure, tile: UInt64)
}

extension CoreSink {
    public func core(didReject failure: CoreDecodeFailure, tile: UInt64) {}
}

/// Creates a core that delivers to `sink`. `CCore.factory(...)` makes the
/// real one, `FakeCore.factory(...)` the double.
public typealias CoreFactory = @Sendable (_ sink: any CoreSink) throws(CoreStatus) -> any CoreAPI
