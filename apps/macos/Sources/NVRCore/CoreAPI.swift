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

    /// `nv_tile_decoded` for an action of a given `generation` (ABI 3: while a
    /// tier switch runs, a tile has two pipelines, and both streams of a
    /// camera can carry the same timestamps). Answers for a generation that
    /// has ended are ignored by the core.
    @discardableResult
    func tileDecoded(_ tile: UInt64, generation: UInt32, ptsMicros: Int64) -> CoreStatus

    /// `nv_tile_decode_failed` for an action of a given `generation`.
    @discardableResult
    func tileDecodeFailed(_ tile: UInt64, generation: UInt32, kind: CoreDecodeFailure) -> CoreStatus

    /// ABI 3 `nv_tile_set_tier`: show `tier` in this tile. The current stream
    /// keeps presenting until the new one's first keyframe has decoded; the
    /// switch is announced by a `tile_tier` event. Returns at once.
    func tileSetTier(_ tile: UInt64, tier: StreamTier) throws(CoreStatus)

    /// ABI 3, minor 3 `nv_tile_retry`: restart this tile's stream now (and,
    /// when its device waits before a reconnect, connect now). The core
    /// keeps a 2 s floor and does nothing for a refused login. Returns at
    /// once.
    func tileRetry(_ tile: UInt64) throws(CoreStatus)

    /// ABI 3 `nv_tile_audio_level`: the latest peak and RMS (0...1) of the
    /// tile's audio, measured by the audio render before the tile's volume;
    /// 0 for a tile that is not audible or not known. Callable at display
    /// rate, from any thread, also inside callbacks.
    func tileAudioLevel(_ tile: UInt64) throws(CoreStatus) -> AudioLevel

    /// ABI 3 `nv_device_key`: the stable key layouts name the device by.
    func deviceKey(_ device: UInt64) throws(CoreStatus) -> String

    /// ABI 3 `nv_layouts_get`: every stored layout, as JSON.
    func layoutsGet() throws(CoreStatus) -> String

    /// ABI 3 `nv_layout_put`: insert or replace one layout (JSON) by its id.
    func layoutPut(json: String) throws(CoreStatus)

    /// ABI 3 `nv_layout_delete`.
    func layoutDelete(id: String) throws(CoreStatus)

    /// `nv_grid_layout`: pure. One entry per picture size; a zero size is a
    /// tile with no picture yet. `fill` crops instead of letterboxing.
    func gridLayout(viewport: PixelSize, gap: UInt32, pictureSizes: [PixelSize], fill: Bool)
        throws(CoreStatus) -> [TileLayout]

    /// Minor 1 `nv_discover_start`: searches the local network for devices.
    /// `json` nil: the defaults; otherwise `{"seconds":3,"interfaces":[...],"onvif":true}`.
    /// Answers arrive as `discovered` events, the end as `discovery_done`.
    /// A search already running is stopped first.
    func discoverStart(json: String?) throws(CoreStatus)

    /// Minor 1 `nv_discover_stop`: ends the running search, if any.
    func discoverStop() throws(CoreStatus)

    /// Minor 2 `nv_update_check` (docs/FFI.md "Updates"): checks the update
    /// source; the outcome arrives as `update` events. `NV_ERR_LIMIT` while
    /// another update operation runs.
    func updateCheck(json: String) throws(CoreStatus)

    /// Minor 2 `nv_update_download`: downloads, verifies, and unpacks the
    /// accepted version into the request's `staging_dir`.
    func updateDownload(json: String) throws(CoreStatus)

    /// Minor 2 `nv_update_cancel`: the running operation's final event is
    /// queued before it returns.
    func updateCancel() throws(CoreStatus)

    /// Minor 2 `nv_update_parse_key` (pure): `{"key":...,"id":...}`, or nil
    /// when the text is not one update public key.
    func updateParseKey(_ text: String) -> String?

    /// Minor 2 `nv_update_parse_source` (pure): `{"ok":true,"kind":...,
    /// "display":...}` or `{"ok":false,"reason":...,"message":...}`.
    func updateParseSource(_ text: String) -> String

    /// `nv_audio_set_output`: the format the real-time callback renders
    /// (1 or 2 interleaved channels). Call again after a device change.
    func audioSetOutput(sampleRate: UInt32, channels: UInt32) throws(CoreStatus)

    /// `nv_tile_audio_set`: audible (claims a playout slot) or silent, at
    /// `volume` 0 (muted, keeping its slot) to 1.
    func tileAudioSet(_ tile: UInt64, audible: Bool, volume: Float) throws(CoreStatus)

    /// `nv_tile_audio_tap`: the tile's audio as 16 kHz blocks to the sink.
    func tileAudioTap(_ tile: UInt64, enabled: Bool) throws(CoreStatus)

    /// `nv_talk_set_permission`: enables (or withdraws) talk-back on one
    /// channel of a device for this run; withdrawing stops a running talk.
    func talkSetPermission(device: UInt64, channel: UInt8, allowed: Bool) throws(CoreStatus)

    /// `nv_talk_start`: asks to talk to the tile's camera; progress arrives as
    /// `talk` events. `.notPermitted` (and a `refused` event) without the
    /// permission.
    func talkStart(_ tile: UInt64) throws(CoreStatus)

    /// `nv_talk_push`: mono float samples (-1...1) at `sampleRate` for the
    /// tile's talk. Not from a real-time audio callback.
    func talkPush(_ tile: UInt64, samples: UnsafeBufferPointer<Float>, sampleRate: UInt32) throws(CoreStatus)

    /// `nv_talk_stop`: at once, or after the audio already pushed (`drain`)
    /// and the core's 1.5 s silent tail; a push before the close continues
    /// the talk.
    func talkStop(_ tile: UInt64, drain: Bool) throws(CoreStatus)

    /// ABI 3, minor 4 `nv_talk_set_conversation`: the tile's camera is in a
    /// call the user has open (`open`), or no longer. While it is, talk
    /// starts on it are not limited per minute; they still need the
    /// permission and are logged.
    func talkSetConversation(_ tile: UInt64, open: Bool) throws(CoreStatus)

    // MARK: Recordings, part 1 (minor 5; docs/FFI.md "Recordings (minor 5)")
    //
    // JSON in and out as the contract shapes it (docs/FFI-RECORDINGS.md);
    // none of these waits on the network. Not from inside a callback.

    /// `nv_recordings_configure`: the recordings settings (JSON).
    func recordingsConfigure(json: String) throws(CoreStatus)
    /// `nv_recordings_enable`: allows (`json`) or stops (nil) indexing a device by its key.
    func recordingsEnable(deviceKey: String, json: String?) throws(CoreStatus)
    /// `nv_recordings_calendar`: days with recordings per camera.
    func recordingsCalendar(query: String) throws(CoreStatus) -> String
    /// `nv_recordings_spans`: recording spans per camera.
    func recordingsSpans(query: String) throws(CoreStatus) -> String
    /// `nv_recordings_events`: events matching the query.
    func recordingsEvents(query: String) throws(CoreStatus) -> String
    /// `nv_recordings_want`: asks for days to be indexed; returns at once.
    func recordingsWant(json: String) throws(CoreStatus)
    /// `nv_recordings_hold_today`: keeps today fresh for a window's cameras; nil releases.
    func recordingsHoldToday(_ hold: UInt64, json: String?) throws(CoreStatus)
    /// `nv_recordings_status`: the index state of every enabled device.
    func recordingsStatus() throws(CoreStatus) -> String
    /// `nv_incidents_get`: incidents overlapping the query's range, newest first.
    func incidentsGet(query: String) throws(CoreStatus) -> String
    /// `nv_incident_get`: one incident by key.
    func incidentGet(key: String) throws(CoreStatus) -> String
    /// `nv_incident_edit`: applies and stores one edit (made once, even if
    /// the answer were lost).
    func incidentEdit(json: String) throws(CoreStatus) -> String
    /// `nv_incident_lock`: locks (and downloads) or unlocks an incident.
    func incidentLock(key: String, locked: Bool) throws(CoreStatus) -> String
    /// `nv_camera_groups_get`.
    func cameraGroupsGet() throws(CoreStatus) -> String
    /// `nv_camera_groups_put`: replaces the groups and name overrides.
    func cameraGroupsPut(json: String) throws(CoreStatus)
    /// `nv_thumbnail_get`: a cached cover thumbnail, or `.pending` (a
    /// `thumbnail` event follows).
    func thumbnailGet(key: String) throws(CoreStatus) -> ThumbnailResult
    /// `nv_thumbnail_put`: replaces the cached entry (usually the app's small image).
    func thumbnailPut(key: String, thumbnail: Thumbnail) throws(CoreStatus)
    /// `nv_thumbnail_cancel`: drops queued fetches of a key, or all (nil).
    func thumbnailCancel(key: String?) throws(CoreStatus)
    /// `nv_download_incident`: queues an incident's download.
    func downloadIncident(json: String) throws(CoreStatus) -> String
    /// `nv_download_pause`: a whole download (camera nil) or one camera (JSON camera).
    func downloadPause(_ download: String, camera: String?, paused: Bool) throws(CoreStatus)
    /// `nv_download_retry`: failed cameras again (camera nil: all).
    func downloadRetry(_ download: String, camera: String?) throws(CoreStatus)
    /// `nv_download_forget`: a download with no job running.
    func downloadForget(_ download: String, deletePartial: Bool) throws(CoreStatus)
    /// `nv_download_trash`: `{"folder":...}` to move to the Trash after its
    /// `download_removed` event.
    func downloadTrash(_ download: String) throws(CoreStatus) -> String
    /// `nv_downloads_get`: every known download.
    func downloadsGet() throws(CoreStatus) -> String
    /// `nv_sound_configure`: live sound settings (JSON).
    func soundConfigure(json: String) throws(CoreStatus)

    /// `nv_audio_render` and its context, for the real-time callback. Valid
    /// until `free`; stop rendering before that.
    var audioRenderHook: AudioRenderHook { get }

    /// `nv_clock_now_ns`: the clock `audioRenderHook`'s host times are on.
    func clockNowNanos() -> UInt64

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
    /// An unanswerable action of a given generation (ABI 3).
    func core(didReject failure: CoreDecodeFailure, tile: UInt64, generation: UInt32)
    /// `on_action` with the action's `generation` (ABI 3 `NvAction.generation`).
    /// The default hands generation-0 actions to `core(didDeliver:tile:)`.
    func core(didDeliver action: CoreAction, tile: UInt64, generation: UInt32)
    /// `on_event`: serialized across the core. UTF-8 JSON, not terminated.
    func core(didEmit json: UnsafeRawBufferPointer)
    /// An action that needs an answer but carries a value this version does
    /// not know (docs/FFI.md): answer it with `failure` so the core is not
    /// left waiting.
    func core(didReject failure: CoreDecodeFailure, tile: UInt64)
    /// `on_audio_tap`: serialized across the core on its tap thread; the
    /// block's samples are borrowed for the duration of the call.
    func core(didTap block: CoreAudioBlock, tile: UInt64)
}

extension CoreSink {
    public func core(didDeliver action: CoreAction, tile: UInt64, generation: UInt32) {
        core(didDeliver: action, tile: tile)
    }
    public func core(didReject failure: CoreDecodeFailure, tile: UInt64) {}
    public func core(didReject failure: CoreDecodeFailure, tile: UInt64, generation: UInt32) {
        core(didReject: failure, tile: tile)
    }
    public func core(didTap block: CoreAudioBlock, tile: UInt64) {}
}

/// Creates a core that delivers to `sink`. `CCore.factory(...)` makes the
/// real one, `FakeCore.factory(...)` the double.
public typealias CoreFactory = @Sendable (_ sink: any CoreSink) throws(CoreStatus) -> any CoreAPI
