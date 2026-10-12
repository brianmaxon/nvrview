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
    /// The minor version may be newer than the header's, never older (the
    /// bindings call functions of the minor they were written for).
    public static var abiMatches: Bool {
        nv_abi_version() == UInt32(NV_ABI_VERSION) && nv_abi_minor() >= UInt32(NV_ABI_MINOR)
    }

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

    public func tileRetry(_ tile: UInt64) throws(CoreStatus) {
        let status = CoreStatus(nv_tile_retry(try core(), tile))
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

    public func discoverStart(json: String?) throws(CoreStatus) {
        let c = try core()
        let status = Self.withOptionalCString(json) { CoreStatus(nv_discover_start(c, $0)) }
        if !status.isOK { throw status }
    }

    public func updateCheck(json: String) throws(CoreStatus) {
        let status = CoreStatus(nv_update_check(try core(), json))
        if !status.isOK { throw status }
    }

    public func updateDownload(json: String) throws(CoreStatus) {
        let status = CoreStatus(nv_update_download(try core(), json))
        if !status.isOK { throw status }
    }

    public func updateCancel() throws(CoreStatus) {
        let status = CoreStatus(nv_update_cancel(try core()))
        if !status.isOK { throw status }
    }

    public func updateParseKey(_ text: String) -> String? {
        text.withCString { t in try? Self.string { nv_update_parse_key(t, $0, $1, $2) } }
    }

    public func updateParseSource(_ text: String) -> String {
        text.withCString { t in try? Self.string { nv_update_parse_source(t, $0, $1, $2) } }
            ?? #"{"ok":false,"reason":"source_invalid","message":"invalid text"}"#
    }

    public func discoverStop() throws(CoreStatus) {
        let status = CoreStatus(nv_discover_stop(try core()))
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

    public func talkSetPermission(device: UInt64, channel: UInt8, allowed: Bool) throws(CoreStatus) {
        let status = CoreStatus(nv_talk_set_permission(try core(), device, channel, allowed))
        if !status.isOK { throw status }
    }

    public func talkStart(_ tile: UInt64) throws(CoreStatus) {
        let status = CoreStatus(nv_talk_start(try core(), tile))
        if !status.isOK { throw status }
    }

    public func talkPush(_ tile: UInt64, samples: UnsafeBufferPointer<Float>, sampleRate: UInt32) throws(CoreStatus) {
        let c = try core()
        guard let base = samples.baseAddress, !samples.isEmpty else { return }
        let status = CoreStatus(nv_talk_push(c, tile, base, samples.count, sampleRate))
        if !status.isOK { throw status }
    }

    public func talkStop(_ tile: UInt64, drain: Bool) throws(CoreStatus) {
        let status = CoreStatus(nv_talk_stop(try core(), tile, drain))
        if !status.isOK { throw status }
    }

    public func talkSetConversation(_ tile: UInt64, open: Bool) throws(CoreStatus) {
        let status = CoreStatus(nv_talk_set_conversation(try core(), tile, open))
        if !status.isOK { throw status }
    }

    // MARK: Recordings, part 1 (minor 5)

    private func check(_ status: NvStatus) throws(CoreStatus) {
        let s = CoreStatus(status)
        if !s.isOK { throw s }
    }

    /// Calls a function that changes state once, with a buffer large enough
    /// for its answer: calling again would apply the change again
    /// (docs/FFI.md, "Recordings (minor 5)").
    private static func once(
        _ call: (UnsafeMutablePointer<CChar>?, Int, UnsafeMutablePointer<Int>) -> NvStatus
    ) throws(CoreStatus) -> String {
        var buffer = [CChar](repeating: 0, count: 64 * 1024)
        var length = 0
        let status = CoreStatus(buffer.withUnsafeMutableBufferPointer { call($0.baseAddress, $0.count, &length) })
        if !status.isOK { throw status }
        return buffer.withUnsafeBufferPointer { b in
            String(decoding: UnsafeRawBufferPointer(start: b.baseAddress, count: min(length, b.count)), as: UTF8.self)
        }
    }

    public func recordingsConfigure(json: String) throws(CoreStatus) {
        try check(nv_recordings_configure(try core(), json))
    }

    public func recordingsEnable(deviceKey: String, json: String?) throws(CoreStatus) {
        let c = try core()
        try check(Self.withOptionalCString(json) { nv_recordings_enable(c, deviceKey, $0) })
    }

    public func recordingsCalendar(query: String) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_recordings_calendar(c, query, $0, $1, $2) }
    }

    public func recordingsSpans(query: String) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_recordings_spans(c, query, $0, $1, $2) }
    }

    public func recordingsEvents(query: String) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_recordings_events(c, query, $0, $1, $2) }
    }

    public func recordingsWant(json: String) throws(CoreStatus) {
        try check(nv_recordings_want(try core(), json))
    }

    public func recordingsHoldToday(_ hold: UInt64, json: String?) throws(CoreStatus) {
        let c = try core()
        try check(Self.withOptionalCString(json) { nv_recordings_hold_today(c, hold, $0) })
    }

    public func recordingsStatus() throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_recordings_status(c, $0, $1, $2) }
    }

    public func incidentsGet(query: String) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_incidents_get(c, query, $0, $1, $2) }
    }

    public func incidentGet(key: String) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_incident_get(c, key, $0, $1, $2) }
    }

    public func incidentEdit(json: String) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.once { nv_incident_edit(c, json, $0, $1, $2) }
    }

    public func incidentLock(key: String, locked: Bool) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.once { nv_incident_lock(c, key, locked, $0, $1, $2) }
    }

    public func cameraGroupsGet() throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_camera_groups_get(c, $0, $1, $2) }
    }

    public func cameraGroupsPut(json: String) throws(CoreStatus) {
        try check(nv_camera_groups_put(try core(), json))
    }

    public func thumbnailGet(key: String) throws(CoreStatus) -> ThumbnailResult {
        let c = try core()
        var capacity = 64 * 1024
        for _ in 0..<4 {
            var info = NvThumbnail()
            var data = Data(count: capacity)
            var length = 0
            let status = CoreStatus(
                data.withUnsafeMutableBytes { b in
                    nv_thumbnail_get(c, key, &info, b.bindMemory(to: UInt8.self).baseAddress, b.count, &length)
                })
            if status == .pending { return .pending }
            if status == .bufferTooSmall {
                capacity = max(length, 1)
                continue
            }
            if !status.isOK { throw status }
            guard let thumbnail = Thumbnail(info, data: data.prefix(length)) else { throw .internal }
            return .ready(thumbnail)
        }
        throw .internal
    }

    public func thumbnailPut(key: String, thumbnail: Thumbnail) throws(CoreStatus) {
        let c = try core()
        var info = thumbnail.info
        let status = thumbnail.data.withUnsafeBytes { b in
            nv_thumbnail_put(c, key, &info, b.bindMemory(to: UInt8.self).baseAddress, b.count)
        }
        try check(status)
    }

    public func thumbnailCancel(key: String?) throws(CoreStatus) {
        let c = try core()
        try check(Self.withOptionalCString(key) { nv_thumbnail_cancel(c, $0) })
    }

    public func downloadIncident(json: String) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.once { nv_download_incident(c, json, $0, $1, $2) }
    }

    public func downloadPause(_ download: String, camera: String?, paused: Bool) throws(CoreStatus) {
        let c = try core()
        try check(Self.withOptionalCString(camera) { nv_download_pause(c, download, $0, paused) })
    }

    public func downloadRetry(_ download: String, camera: String?) throws(CoreStatus) {
        let c = try core()
        try check(Self.withOptionalCString(camera) { nv_download_retry(c, download, $0) })
    }

    public func downloadForget(_ download: String, deletePartial: Bool) throws(CoreStatus) {
        try check(nv_download_forget(try core(), download, deletePartial))
    }

    public func downloadTrash(_ download: String) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.once { nv_download_trash(c, download, $0, $1, $2) }
    }

    public func downloadsGet() throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_downloads_get(c, $0, $1, $2) }
    }

    public func soundConfigure(json: String) throws(CoreStatus) {
        try check(nv_sound_configure(try core(), json))
    }

    // MARK: Recorded playback (minor 6)

    public func playbackOpen(json: String) throws(CoreStatus) -> OpenedPlayback {
        let c = try core()
        var playback: UInt64 = 0
        // At most 4 cameras per playback, so 4 entries always fit.
        var tiles = [UInt64](repeating: 0, count: 4)
        var count = 0
        let status = tiles.withUnsafeMutableBufferPointer { b in
            nv_playback_open(c, json, &playback, b.baseAddress, b.count, &count)
        }
        try check(status)
        return OpenedPlayback(playback: playback, tiles: Array(tiles.prefix(count)))
    }

    public func playbackAddCamera(_ playback: UInt64, json: String) throws(CoreStatus) -> UInt64 {
        var tile: UInt64 = 0
        try check(nv_playback_add_camera(try core(), playback, json, &tile))
        return tile
    }

    public func playbackRemoveCamera(_ playback: UInt64, tile: UInt64) throws(CoreStatus) {
        try check(nv_playback_remove_camera(try core(), playback, tile))
    }

    public func playbackPlay(_ playback: UInt64) throws(CoreStatus) {
        try check(nv_playback_play(try core(), playback))
    }

    public func playbackPause(_ playback: UInt64) throws(CoreStatus) {
        try check(nv_playback_pause(try core(), playback))
    }

    public func playbackSeek(_ playback: UInt64, positionMs: Int64, mode: SeekMode) throws(CoreStatus) {
        try check(nv_playback_seek(try core(), playback, positionMs, NvSeekMode(rawValue: mode.rawValue)))
    }

    public func playbackSetRate(_ playback: UInt64, rate: Double) throws(CoreStatus) {
        try check(nv_playback_set_rate(try core(), playback, rate))
    }

    public func playbackSetOffset(_ playback: UInt64, tile: UInt64, offsetMs: Int64) throws(CoreStatus) {
        try check(nv_playback_set_offset(try core(), playback, tile, offsetMs))
    }

    public func playbackPosition(_ playback: UInt64) throws(CoreStatus) -> PlaybackPosition {
        var position: Int64 = 0
        var state = NV_PLAYBACK_FAILED
        try check(nv_playback_position(try core(), playback, &position, &state))
        return PlaybackPosition(positionMs: position, state: PlaybackState(rawValue: state.rawValue) ?? .failed)
    }

    public func playbackStatus(_ playback: UInt64) throws(CoreStatus) -> String {
        let c = try core()
        return try Self.string { nv_playback_status(c, playback, $0, $1, $2) }
    }

    public func playbackClose(_ playback: UInt64) throws(CoreStatus) {
        try check(nv_playback_close(try core(), playback))
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
