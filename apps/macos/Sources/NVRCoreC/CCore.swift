import Foundation
import NVRCore
import NVRViewCore
import Synchronization

// The real core: `CoreAPI` over the C functions of docs/nvrview.h.
//
// This target is the only one that calls them, so it is the only one that
// needs the Rust library at link time. Nothing links it unless the package is
// built with NVRVIEW_REAL_CORE=1 (see Package.swift).

public final class CCore: CoreAPI {
    // @unchecked Sendable state: the handle is created once and freed once,
    // and every C function we call is documented thread-safe (FFI.md).
    private struct Handle: @unchecked Sendable {
        var core: OpaquePointer?
        var context: Unmanaged<CallbackContext>?
    }
    private let handle: Mutex<Handle>

    /// `nv_abi_version` and the header's `NV_ABI_VERSION` must agree
    /// (docs/FFI.md Versioning): checked here, at startup.
    public static var abiMatches: Bool { nv_abi_version() == UInt32(NV_ABI_VERSION) }

    /// `dataDirectory` is where the core keeps layouts and device keys
    /// (nil: in memory for the run).
    public static func factory(logDirectory: String? = nil, dataDirectory: String? = nil) -> CoreFactory {
        { sink throws(CoreStatus) in
            guard abiMatches else { throw .internal }
            return try CCore(sink: sink, logDirectory: logDirectory, dataDirectory: dataDirectory)
        }
    }

    init(sink: any CoreSink, logDirectory: String?, dataDirectory: String?) throws(CoreStatus) {
        let (callbacks, context) = CCallbacks.make(sink: sink)
        var out: OpaquePointer?
        // Both strings are copied by nv_core_new before it returns.
        let status = Self.withOptionalCString(logDirectory) { log in
            Self.withOptionalCString(dataDirectory) { data in
                var config = NvConfig(log_dir: log, log_max_files: 0, log_max_total_bytes: 0, data_dir: data)
                return CoreStatus(nv_core_new(&config, callbacks, &out))
            }
        }
        guard status.isOK, let out else {
            context.release()
            throw status.isOK ? .internal : status
        }
        handle = Mutex(Handle(core: out, context: context))
    }

    deinit { free() }

    private static func withOptionalCString<R>(_ s: String?, _ body: (UnsafePointer<CChar>?) -> R) -> R {
        guard let s else { return body(nil) }
        return s.withCString { body($0) }
    }

    /// Calls a string-returning C function, growing the buffer as
    /// `NV_ERR_BUFFER_TOO_SMALL` asks (`*len` + 1 bytes).
    private static func string(
        _ call: (UnsafeMutablePointer<CChar>?, Int, UnsafeMutablePointer<Int>) -> NvStatus
    ) throws(CoreStatus) -> String {
        var capacity = 4096
        for _ in 0..<4 {
            var buffer = [CChar](repeating: 0, count: capacity)
            var length = 0
            let status = CoreStatus(buffer.withUnsafeMutableBufferPointer { call($0.baseAddress, $0.count, &length) })
            if status == .bufferTooSmall {
                capacity = length + 1
                continue
            }
            if !status.isOK { throw status }
            return buffer.withUnsafeBufferPointer { b in
                String(decoding: UnsafeRawBufferPointer(start: b.baseAddress, count: min(length, b.count)), as: UTF8.self)
            }
        }
        throw .internal
    }

    private func core() throws(CoreStatus) -> OpaquePointer {
        guard let c = handle.withLock({ $0.core }) else { throw .closed }
        return c
    }

    public func addDevice(host: String, port: UInt16, user: String, password: String) throws(CoreStatus) -> UInt64 {
        let c = try core()
        var id: UInt64 = 0
        let status = CoreStatus(nv_device_add(c, host, port, user, password, &id))
        if !status.isOK { throw status }
        return id
    }

    public func removeDevice(_ device: UInt64) throws(CoreStatus) {
        let status = CoreStatus(nv_device_remove(try core(), device))
        if !status.isOK { throw status }
    }

    public func openTile(device: UInt64, channel: UInt8, tier: StreamTier) throws(CoreStatus) -> UInt64 {
        var id: UInt64 = 0
        let status = CoreStatus(nv_tile_open(try core(), device, channel, NvStreamTier(tier), &id))
        if !status.isOK { throw status }
        return id
    }

    public func closeTile(_ tile: UInt64) throws(CoreStatus) {
        let status = CoreStatus(nv_tile_close(try core(), tile))
        if !status.isOK { throw status }
    }

    public func tileDecoded(_ tile: UInt64, ptsMicros: Int64) -> CoreStatus {
        // Held across the call so `free` cannot run underneath it; the call never blocks.
        handle.withLock { h in
            guard let c = h.core else { return .closed }
            return CoreStatus(nv_tile_decoded(c, tile, 0, ptsMicros))
        }
    }

    public func tileDecoded(_ tile: UInt64, generation: UInt32, ptsMicros: Int64) -> CoreStatus {
        handle.withLock { h in
            guard let c = h.core else { return .closed }
            return CoreStatus(nv_tile_decoded(c, tile, generation, ptsMicros))
        }
    }

    public func tileDecodeFailed(_ tile: UInt64, generation: UInt32, kind: CoreDecodeFailure) -> CoreStatus {
        handle.withLock { h in
            guard let c = h.core else { return .closed }
            return CoreStatus(nv_tile_decode_failed(c, tile, generation, NvDecodeFailure(kind)))
        }
    }

    // MARK: ABI 3

    public func tileSetTier(_ tile: UInt64, tier: StreamTier) throws(CoreStatus) {
        let status = CoreStatus(nv_tile_set_tier(try core(), tile, NvStreamTier(tier)))
        if !status.isOK { throw status }
    }

    public func tileAudioLevel(_ tile: UInt64) throws(CoreStatus) -> AudioLevel {
        // Lock-free in the core; held here only so `free` cannot run underneath.
        try handle.withLock { h throws(CoreStatus) -> AudioLevel in
            guard let c = h.core else { throw .closed }
            var peak: Float = 0
            var rms: Float = 0
            let status = CoreStatus(nv_tile_audio_level(c, tile, &peak, &rms))
            if !status.isOK { throw status }
            return AudioLevel(peak: peak, rms: rms)
        }
    }

    public func deviceKey(_ device: UInt64) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_device_key(c, device, $0, $1, $2) }
    }

    public func layoutsGet() throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_layouts_get(c, $0, $1, $2) }
    }

    public func layoutPut(json: String) throws(CoreStatus) {
        let c = try core()
        let status = CoreStatus(nv_layout_put(c, json))
        if !status.isOK { throw status }
    }

    public func layoutDelete(id: String) throws(CoreStatus) {
        let c = try core()
        let status = CoreStatus(nv_layout_delete(c, id))
        if !status.isOK { throw status }
    }

    public func tileDecodeFailed(_ tile: UInt64, kind: CoreDecodeFailure) -> CoreStatus {
        tileDecodeFailed(tile, generation: 0, kind: kind)
    }

    // MARK: Audio

    public func audioSetOutput(sampleRate: UInt32, channels: UInt32) throws(CoreStatus) {
        let status = CoreStatus(nv_audio_set_output(try core(), sampleRate, channels))
        if !status.isOK { throw status }
    }

    public func tileAudioSet(_ tile: UInt64, audible: Bool, volume: Float) throws(CoreStatus) {
        let status = CoreStatus(nv_tile_audio_set(try core(), tile, audible, volume))
        if !status.isOK { throw status }
    }

    public func tileAudioTap(_ tile: UInt64, enabled: Bool) throws(CoreStatus) {
        let status = CoreStatus(nv_tile_audio_tap(try core(), tile, enabled))
        if !status.isOK { throw status }
    }

    /// `nv_audio_render` with the core handle as its context. Players stop
    /// rendering before `free` (CoreBridge.shutdown sees to it).
    public var audioRenderHook: AudioRenderHook {
        guard let c = handle.withLock({ $0.core }) else { return .silent }
        return AudioRenderHook(
            function: { context, out, frames, channels, host in
                nv_audio_render(OpaquePointer(context), out, frames, channels, host).rawValue
            }, context: UnsafeMutableRawPointer(c))
    }

    public func clockNowNanos() -> UInt64 { nv_clock_now_ns() }

    public func gridLayout(viewport: PixelSize, gap: UInt32, pictureSizes: [PixelSize], fill: Bool)
        throws(CoreStatus) -> [TileLayout]
    {
        let sizes = pictureSizes.map(NvSize.init)
        var out = [NvTileLayout](repeating: NvTileLayout(), count: sizes.count)
        let status = CoreStatus(
            sizes.withUnsafeBufferPointer { s in
                out.withUnsafeMutableBufferPointer { o in
                    nv_grid_layout(UInt32(sizes.count), NvSize(viewport), gap, s.baseAddress, fill, o.baseAddress)
                }
            })
        if !status.isOK { throw status }
        return out.map(TileLayout.init)
    }

    public func free() {
        let taken = handle.withLock { h -> Handle in
            defer { h = Handle() }
            return h
        }
        guard let core = taken.core else { return }
        nv_core_free(core)  // joins the core's threads; no callback runs after it
        taken.context?.release()
    }
}
