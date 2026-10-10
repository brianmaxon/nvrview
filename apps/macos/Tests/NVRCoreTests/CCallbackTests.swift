import Foundation
import NVRCore
import NVRViewCore
import Synchronization
import Testing
import VideoKit

/// A `CoreAPI` that only records what the app tells it.
final class RecordingCore: CoreAPI {
    struct Log: Sendable, Equatable {
        var decoded: [Int64] = []
        var failed: [CoreDecodeFailure] = []
        var freed = false
    }
    let log = Mutex(Log())
    let sink: any CoreSink
    init(sink: any CoreSink) { self.sink = sink }

    func addDevice(host: String, port: UInt16, user: String, password: String) throws(CoreStatus) -> UInt64 { 1 }
    func removeDevice(_ device: UInt64) throws(CoreStatus) {}
    func openTile(device: UInt64, channel: UInt8, tier: StreamTier) throws(CoreStatus) -> UInt64 { 7 }
    func closeTile(_ tile: UInt64) throws(CoreStatus) {}
    func tileDecoded(_ tile: UInt64, ptsMicros: Int64) -> CoreStatus {
        log.withLock { $0.decoded.append(ptsMicros) }
        return .ok
    }
    func tileDecodeFailed(_ tile: UInt64, kind: CoreDecodeFailure) -> CoreStatus {
        log.withLock { $0.failed.append(kind) }
        return .ok
    }
    func gridLayout(viewport: PixelSize, gap: UInt32, pictureSizes: [PixelSize], fill: Bool)
        throws(CoreStatus) -> [TileLayout]
    { [] }
    func free() { log.withLock { $0.freed = true } }
}

/// Calls the @convention(c) callbacks the way the C library will: with
/// hand-built `NvAction` structs whose pointers borrow fixture bytes.
@Suite struct CCallbackTests {
    private struct Harness {
        let bridge: CoreBridge
        let core: RecordingCore
        let callbacks: NvCallbacks
        let context: Unmanaged<CallbackContext>
        let tile: CoreTile
        let events: Box<[CoreEvent]>

        init() throws {
            let captured = Box<RecordingCore?>(nil)
            let events = Box<[CoreEvent]>([])
            self.events = events
            bridge = try CoreBridge(
                factory: { sink throws(CoreStatus) in
                    let core = RecordingCore(sink: sink)
                    captured.set(core)
                    return core
                },
                onEvent: { event in events.update { $0.append(event) } })
            core = captured.get()!
            (callbacks, context) = CCallbacks.make(sink: core.sink)
            let device = try bridge.addDevice(host: "192.0.2.1", port: 9000, user: "example-user", password: "x")
            tile = try bridge.openTile(device: device, channel: 0, tier: .main)
        }

        func action(_ a: NvAction, tile id: UInt64? = nil) {
            var a = a
            callbacks.on_action!(callbacks.context, id ?? tile.id, &a)
        }

        func close() {
            bridge.shutdown()
            context.release()
        }
    }

    private static let matrix601: (Float, Float, Float, Float, Float, Float, Float, Float, Float, Float, Float, Float) =
        (1.164, 0, 1.596, -0.874, 1.164, -0.392, -0.813, 0.532, 1.164, 2.017, 0, -1.086)

    /// A Configure for the fixture's parameter sets, then Decodes of its
    /// access units, all through the C callback.
    private func run(_ name: String, codec: NvCodec, frames: Int) throws {
        let clip = try Fixtures.clip(name)
        let h = try Harness()
        defer { h.close() }
        let sets = Fixtures.parameterSets(clip)
        // Hold the bytes in one allocation, as the core would.
        let total = sets.reduce(0) { $0 + $1.count }
        let storage = UnsafeMutableRawPointer.allocate(byteCount: total, alignment: 1)
        defer { storage.deallocate() }
        var offset = 0
        var bytes: [NvBytes] = []
        for set in sets {
            set.copyBytes(to: UnsafeMutableRawBufferPointer(start: storage + offset, count: set.count))
            bytes.append(NvBytes(data: storage.advanced(by: offset).assumingMemoryBound(to: UInt8.self), len: set.count))
            offset += set.count
        }
        var configure = NvAction()
        configure.kind = NV_ACTION_CONFIGURE
        bytes.withUnsafeBufferPointer { sets in
            configure.configure = NvConfigure(
                codec: codec, parameter_sets: sets.baseAddress, parameter_set_count: sets.count,
                coded_size: NvSize(width: 320, height: 240),
                visible_rect: NvRect(x: 0, y: 0, width: 320, height: 240),
                color: NvColor(matrix: NV_MATRIX_BT601, full_range: false, ycbcr_to_rgb: Self.matrix601))
            h.action(configure)
        }
        let snapshot = h.tile.snapshot
        #expect(snapshot.configures == 1 && snapshot.codedSize == PixelSize(width: 320, height: 240))
        #expect(snapshot.color?.matrix == .bt601 && snapshot.color?.fullRange == false)
        #expect(snapshot.color?.ycbcrToRGB[2] == 1.596 && snapshot.color?.ycbcrToRGB[11] == -1.086)

        var status = NvAction()
        status.kind = NV_ACTION_STATUS
        status.status = NV_TILE_PLAYING
        h.action(status)
        #expect(h.tile.snapshot.status == .playing)

        for i in 0..<frames {
            let au = Fixtures.accessUnit(clip, i)
            au.withUnsafeBytes { raw in
                var decode = NvAction()
                decode.kind = NV_ACTION_DECODE
                decode.decode = NvDecode(
                    access_unit: NvBytes(data: raw.bindMemory(to: UInt8.self).baseAddress, len: raw.count),
                    keyframe: i == 0, pts_us: Int64(i) * 50_000)
                h.action(decode)
            }
            // The access unit memory is gone now (borrowed); the decoder copied what it needs.
            h.tile.decoder.waitForPendingFrames()
        }
        #expect(h.core.log.withLock { $0.decoded } == (0..<frames).map { Int64($0) * 50_000 })
        #expect(h.core.log.withLock { $0.failed }.isEmpty)
        let picture = h.tile.decoder.latestPicture()
        #expect(picture?.width == 320 && picture?.height == 240)

        // Flush, Reset, Clear carry no data. Clear drops the picture; Reset
        // then a Decode is answered "not configured" (exactly once).
        for kind in [NV_ACTION_FLUSH, NV_ACTION_CLEAR] {
            var a = NvAction()
            a.kind = kind
            h.action(a)
        }
        #expect(h.tile.decoder.latestPicture() == nil)
        var reset = NvAction()
        reset.kind = NV_ACTION_RESET
        h.action(reset)
        Fixtures.accessUnit(clip, 1).withUnsafeBytes { raw in
            var decode = NvAction()
            decode.kind = NV_ACTION_DECODE
            decode.decode = NvDecode(
                access_unit: NvBytes(data: raw.bindMemory(to: UInt8.self).baseAddress, len: raw.count),
                keyframe: false, pts_us: 1)
            h.action(decode)
        }
        #expect(h.core.log.withLock { $0.failed } == [.notConfigured])
    }

    @Test func h264ThroughTheCCallbacks() throws { try run("h264-basic.h264", codec: NV_CODEC_H264, frames: 12) }
    @Test func h265ThroughTheCCallbacks() throws { try run("h265-basic.h265", codec: NV_CODEC_H265, frames: 12) }

    @Test func unknownTilesKindsAndEventsAreHandled() throws {
        let h = try Harness()
        defer { h.close() }
        var flush = NvAction()
        flush.kind = NV_ACTION_FLUSH
        h.action(flush, tile: 12345)  // unknown tile: ignored
        var odd = NvAction()
        odd.kind = NvActionKind(rawValue: 99)  // a kind from a newer core: ignored
        h.action(odd)
        // A Configure with a codec this version does not know is ignored too.
        var configure = NvAction()
        configure.kind = NV_ACTION_CONFIGURE
        configure.configure.codec = NvCodec(rawValue: 77)
        h.action(configure)
        #expect(h.tile.snapshot.configures == 0)

        let json = #"{"type":"device_state","device":1,"state":"backoff","attempt":2,"retry_ms":4000,"new":true}"#
        var text = json
        text.withUTF8 { h.callbacks.on_event!(h.callbacks.context, UnsafeRawPointer($0.baseAddress!).assumingMemoryBound(to: CChar.self), $0.count) }
        let unknown = #"{"type":"from_the_future"}"#
        var text2 = unknown
        text2.withUTF8 { h.callbacks.on_event!(h.callbacks.context, UnsafeRawPointer($0.baseAddress!).assumingMemoryBound(to: CChar.self), $0.count) }
        #expect(h.events.get() == [.deviceState(device: 1, state: .backoff(attempt: 2, retryMilliseconds: 4000))])
    }

    @Test func noAnswersAfterShutdown() throws {
        let h = try Harness()
        h.bridge.shutdown()
        #expect(h.core.log.withLock { $0.freed })
        // A late decoder result must not reach the (freed) core.
        h.tile.decoder.decode(annexB: Data([0, 0, 0, 1, 0x65, 0x88]), keyframe: true, ptsMicros: 1)
        #expect(h.core.log.withLock { $0.failed }.isEmpty)
        #expect(h.core.log.withLock { $0.decoded }.isEmpty)
        h.context.release()
    }
}
