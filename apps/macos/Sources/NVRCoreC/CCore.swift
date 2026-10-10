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

    public static func factory(logDirectory: String? = nil) -> CoreFactory {
        { sink throws(CoreStatus) in
            guard abiMatches else { throw .internal }
            return try CCore(sink: sink, logDirectory: logDirectory)
        }
    }

    init(sink: any CoreSink, logDirectory: String?) throws(CoreStatus) {
        let (callbacks, context) = CCallbacks.make(sink: sink)
        var out: OpaquePointer?
        let status: CoreStatus
        // log_dir is copied by nv_core_new before it returns.
        if let logDirectory {
            status = logDirectory.withCString { dir in
                var config = NvConfig(log_dir: dir, log_max_files: 0, log_max_total_bytes: 0)
                return CoreStatus(nv_core_new(&config, callbacks, &out))
            }
        } else {
            var config = NvConfig(log_dir: nil, log_max_files: 0, log_max_total_bytes: 0)
            status = CoreStatus(nv_core_new(&config, callbacks, &out))
        }
        guard status.isOK, let out else {
            context.release()
            throw status.isOK ? .internal : status
        }
        handle = Mutex(Handle(core: out, context: context))
    }

    deinit { free() }

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
            return CoreStatus(nv_tile_decoded(c, tile, ptsMicros))
        }
    }

    public func tileDecodeFailed(_ tile: UInt64, kind: CoreDecodeFailure) -> CoreStatus {
        handle.withLock { h in
            guard let c = h.core else { return .closed }
            return CoreStatus(nv_tile_decode_failed(c, tile, NvDecodeFailure(kind)))
        }
    }

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
