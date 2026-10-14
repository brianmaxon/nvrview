import AVFoundation
import CoreGraphics
import FakeCore
import Foundation
import Testing
import VideoKit

@testable import NVRCore

/// Export (docs/UX.md "Recordings", "Export"): the in and out points, the
/// requests and plans, the output rules, and the writers over synthetic
/// recordings.
struct ExportPlanTests {
    private let range: ClosedRange<Int64> = 10_000...20_000

    @Test func pointsSetClampAndNeverCross() {
        var p = InOutPoints()
        #expect(p.isEmpty && p.window(in: range)! == (10_000, 20_000))
        p.setIn(12_000, within: range)
        p.setOut(15_000, within: range)
        #expect(p.window(in: range)! == (12_000, 15_000))
        p.setIn(99_000, within: range)  // clamped to the end: the out point goes
        #expect(p.inMs == 20_000 && p.outMs == nil && p.window(in: range) == nil)
        p.setOut(11_000, within: range)  // before the in point: the in point goes
        #expect(p.inMs == nil && p.outMs == 11_000 && p.window(in: range)! == (10_000, 11_000))
        p.setIn(5_000, within: range)
        #expect(p.inMs == 10_000)
        p.clear()
        #expect(p.isEmpty)
    }

    @Test func pointsOutsideANewRangeAreDropped() {
        let p = InOutPoints(inMs: 12_000, outMs: 18_000)
        #expect(p.kept(within: 11_000...19_000) == p)
        #expect(p.kept(within: 13_000...19_000) == InOutPoints(inMs: nil, outMs: 18_000))
        #expect(p.kept(within: 0...5_000).isEmpty)
    }

    @Test func handlesAreGrabbedAndMovedWithinLimits() {
        var p = InOutPoints(inMs: 12_000, outMs: 18_000)
        #expect(p.handle(atFraction: 0.21, range: range, tolerance: 0.03) == .in)
        #expect(p.handle(atFraction: 0.79, range: range, tolerance: 0.03) == .out)
        #expect(p.handle(atFraction: 0.5, range: range, tolerance: 0.03) == nil)
        #expect(InOutPoints().handle(atFraction: 0.5, range: range, tolerance: 1) == nil)
        p.move(.in, to: 19_000, within: range)  // stops short of the out point
        #expect(p.inMs == 18_000 - InOutPoints.minimumMs)
        p.move(.out, to: 1_000, within: range)  // stops past the in point
        #expect(p.outMs == p.inMs! + InOutPoints.minimumMs)
        p.move(.in, to: -5, within: range)
        #expect(p.inMs == 10_000)
    }

    @Test func requestsAreJSONTheCoreReads() throws {
        let cam = RecordingCamera(device: "d-example", channel: 2)
        func object(_ s: String) throws -> [String: Any] { try #require(JSONSerialization.jsonObject(with: Data(s.utf8)) as? [String: Any]) }
        let shared = ExportOptions(dest: "/Volumes/Out Folder/with \"quotes\"", shareSafe: true)
        let trim = try object(ExportRequest.trim(download: "d1", cameras: [cam], inMs: 5, outMs: 9, exact: true, options: shared))
        #expect(trim["kind"] as? String == "trim" && trim["cut"] as? String == "exact" && trim["share_safe"] as? Bool == true)
        #expect(trim["dest"] as? String == "/Volumes/Out Folder/with \"quotes\"" && trim["in_ms"] as? Int == 5)
        #expect((trim["cameras"] as? [[String: Any]])?.first?["channel"] as? Int == 2)
        let plain = try object(ExportRequest.trim(download: "d1", cameras: nil, inMs: 5, outMs: 9, exact: false, options: .init()))
        #expect(plain["cameras"] == nil && plain["dest"] == nil && plain["share_safe"] == nil && plain["cut"] as? String == "keyframe")
        let grid = try object(ExportRequest.edit(grid: "4up", download: "d1", window: (1, 2), options: .init()))
        #expect(grid["kind"] as? String == "grid" && grid["layout"] as? String == "4up" && grid["out_ms"] as? Int == 2)
        let follow = try object(ExportRequest.edit(grid: nil, download: "d1", window: nil, options: .init()))
        #expect(follow["kind"] as? String == "follow" && follow["in_ms"] == nil && follow["canvas"] == nil)
        // Whole seconds: the start rounds down, the end up.
        let range = try object(ExportRequest.range(startMs: 1_500, endMs: 4_001, cameras: [cam], name: "  ", options: .init()))
        #expect(range["start"] as? Int == 1 && range["end"] as? Int == 5 && range["name"] == nil)
        #expect((range["cameras"] as? [[String: Any]])?.first?["tier"] as? String == "main")
        let named = try object(ExportRequest.range(startMs: 0, endMs: 1_000, cameras: [cam], name: "Gate", options: .init()))
        #expect(named["name"] as? String == "Gate")
        let stills = try object(ExportRequest.stills(playback: 7, tiles: [3, 4], format: .jpeg, options: .init()))
        #expect(stills["playback"] as? Int == 7 && stills["tiles"] as? [Int] == [3, 4] && stills["format"] as? String == "jpeg")
        let file = try object(ExportRequest.still(download: "d1", camera: cam, atMs: 77, exact: false, format: .png, options: .init()))
        #expect(file["at_ms"] as? Int == 77 && file["exact"] as? Bool == false && file["download"] as? String == "d1")
    }

    @Test func plansParse() throws {
        let trim = """
            {"version":1,"kind":"trim","download":"d1","start_ms":1000,"end_ms":4000,"share_safe":true,
             "outputs":[{"source":0,"camera":{"device":"d-example","channel":1,"lens":0},"name":"Camera A",
             "file":"/x/a.mp4","source_in_ms":2000,"source_out_ms":4500,"copy_possible":true,"suggested_file":"/y/a trim.mp4"}]}
            """
        let plan = try #require(TrimPlan.parse(trim))
        #expect(plan.outputs.first?.sourceInMs == 2000 && plan.outputs.first?.copyPossible == true && plan.shareSafe)
        #expect(TrimPlan.parse(#"{"kind":"follow","download":"d","start_ms":0,"end_ms":1,"outputs":[]}"#) == nil)
        #expect(TrimPlan.parse(trim.replacingOccurrences(of: "\"source_out_ms\":4500", with: "\"source_out_ms\":1500")) == nil)
        let stills = try #require(
            StillPlan.parse(
                #"{"stills":[{"tile":11,"camera":{"device":"d-example","channel":0,"lens":0},"name":"Camera A","generation":4,"position_ms":99,"suggested_file":"/z/a.png"}]}"#))
        #expect(stills.stills.first?.tile == 11 && stills.stills.first?.positionMs == 99 && stills.stills.first?.file == nil)
        #expect(StillPlan.parse(#"{"stills":[]}"#) == nil)
    }

    private func follow(widths: [Int] = [640, 2560]) -> EditPlan {
        let cam = RecordingCamera(device: "d-example", channel: 0)
        return EditPlan(
            kind: .follow, startMs: 10_000, endMs: 14_000,
            sources: widths.enumerated().map { .init(source: $0.offset, camera: cam, name: "", file: "/x/\($0.offset).mp4", fileStartMs: 9_000, width: $0.element, height: $0.element * 9 / 16) },
            segments: [
                .init(startMs: 10_000, endMs: 12_000, source: 0, sourceInMs: 1_000),
                .init(startMs: 12_000, endMs: 14_000, source: 1, sourceInMs: 3_000),
            ])
    }

    @Test func outputSizeFitsAndIsEven() {
        #expect(ExportGeometry.outputSize(follow()) == (1920, 1080))
        #expect(ExportGeometry.outputSize(follow(widths: [640, 641])) == (640, 360))
        let grid = EditPlan(
            kind: .grid, startMs: 0, endMs: 1_000, sources: [], layout: "4up", canvas: .init(width: 3840, height: 2160))
        #expect(ExportGeometry.outputSize(grid) == (1920, 1080))
        #expect(ExportGeometry.fitted(width: 1000, height: 1001) == (1000, 1000))
        #expect(ExportGeometry.fitted(width: 0, height: 0) == (1280, 720))
    }

    @Test func framesCoverTheRangeAtTheRate() {
        let plan = follow()
        #expect(ExportGeometry.frameCount(plan, fps: 30) == 120)
        #expect(ExportGeometry.time(plan, frame: 0) == 10_000 && ExportGeometry.time(plan, frame: 30) == 11_000)
        #expect(ExportGeometry.time(plan, frame: 119) == 13_966 && ExportGeometry.time(plan, frame: 500) == 14_000)
        var short = plan
        short.endMs = 10_010
        #expect(ExportGeometry.frameCount(short, fps: 30) == 1)
    }

    @Test func followSoundCutsWithThePictureAndJoinsNeighbors() {
        var plan = follow()
        #expect(
            ExportGeometry.audioPieces(plan) == [
                .init(source: 0, fileStartMs: 1_000, fileEndMs: 3_000, atMs: 0),
                .init(source: 1, fileStartMs: 3_000, fileEndMs: 5_000, atMs: 2_000),
            ])
        // Two segments of one source that follow each other in file and time join.
        plan.segments = [
            .init(startMs: 10_000, endMs: 11_000, source: 0, sourceInMs: 1_000),
            .init(startMs: 11_000, endMs: 12_000, source: 0, sourceInMs: 2_000),
            .init(startMs: 13_000, endMs: 14_000, source: 0, sourceInMs: 5_000),
        ]
        #expect(
            ExportGeometry.audioPieces(plan) == [
                .init(source: 0, fileStartMs: 1_000, fileEndMs: 3_000, atMs: 0),
                .init(source: 0, fileStartMs: 5_000, fileEndMs: 6_000, atMs: 3_000),
            ])
    }

    @Test func gridSoundIsOneCellsWhereItHasVideo() {
        let cam = RecordingCamera(device: "d-example", channel: 0)
        let plan = EditPlan(
            kind: .grid, startMs: 10_000, endMs: 20_000,
            sources: [
                .init(source: 0, camera: cam, name: "", file: "a", fileStartMs: 9_000, width: 640, height: 360),
                .init(source: 1, camera: cam, name: "", file: "b", fileStartMs: 10_000, offsetMs: 500, width: 640, height: 360),
            ],
            layout: "2up", canvas: .init(width: 1920, height: 1080),
            cells: [
                .init(rect: .init(x: 0, y: 0, width: 960, height: 1080), source: 0, present: [[8_000, 12_000], [15_000, 30_000]]),
                .init(rect: .init(x: 960, y: 0, width: 960, height: 1080), source: 1, present: [[10_000, 20_000]]),
            ])
        #expect(
            ExportGeometry.audioPieces(plan) == [
                .init(source: 0, fileStartMs: 1_000, fileEndMs: 3_000, atMs: 0),
                .init(source: 0, fileStartMs: 6_000, fileEndMs: 11_000, atMs: 5_000),
            ])
        #expect(ExportGeometry.audioPieces(plan, gridCell: 1) == [.init(source: 1, fileStartMs: -500, fileEndMs: 9_500, atMs: 0)])
        #expect(ExportGeometry.audioPieces(plan, gridCell: 5).isEmpty)
    }
}

/// The writers over synthetic recordings: trims by keyframe copy and by
/// re-encoding, share-safe output, and a follow cut with sound.
struct ExportWriterTests {
    private func temporary() -> URL {
        FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-export-\(UUID().uuidString)")
    }

    /// A writer whose input is never finished and never asked for samples
    /// (the shape of the hang): the wait ends when the writer is canceled,
    /// and at its ceiling when nothing happens.
    @Test func aWriterWaitEndsWhenTheWriterStopsOrTheCeilingPasses() async throws {
        @Sendable func writer() throws -> AVAssetWriter {
            let w = try AVAssetWriter(url: FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-export-\(UUID().uuidString)").appendingPathExtension("mp4"), fileType: .mp4)
            let input = AVAssetWriterInput(
                mediaType: .video,
                outputSettings: [AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: 64, AVVideoHeightKey: 64])
            w.add(input)
            #expect(w.startWriting())
            w.startSession(atSourceTime: .zero)
            return w
        }
        // Under the encoder gate: other tests count keyframes of files made
        // with one encoder session at a time.
        try await EncoderGate.run { try await Self.waitCases(writer) }
    }

    private static func waitCases(_ writer: @Sendable () throws -> AVAssetWriter) async throws {
        let stuck = DispatchGroup()
        stuck.enter()
        let canceled = try writer()
        canceled.cancelWriting()
        #expect(await WriterWait.drain(stuck, writer: canceled, ceiling: 60) == .writerStopped)
        let idle = try writer()
        #expect(await WriterWait.drain(stuck, writer: idle, ceiling: 0.2) == .timedOut)
        idle.cancelWriting()
        stuck.leave()
        let done = DispatchGroup()
        #expect(await WriterWait.drain(done, writer: try writer(), ceiling: 60) == .drained)
    }

    /// The camera and frame number a file's picture at `ms` shows.
    private func look(_ url: URL, at ms: Int64) async throws -> (color: Int?, frame: Int) {
        let file = try await RecordedFileSource.open(url)
        defer { file.close() }
        file.show(ms)
        let picture = try #require(file.latestPicture())
        let image = try #require(
            await PictureRenderer().render(
                PictureRender(
                    picture: picture, color: file.info.color, source: CGRect(x: 0, y: 0, width: picture.width, height: picture.height),
                    width: 320, height: 180)))
        return SyntheticRecording.read(image, picture: CGRect(x: 0, y: 0, width: 320, height: 180))
    }

    private func output(_ file: URL, in inMs: Int64, out outMs: Int64, copy: Bool) -> TrimPlan.Output {
        let json = """
            {"version":1,"kind":"trim","download":"d","start_ms":\(inMs),"end_ms":\(outMs),"outputs":[{"source":0,
            "camera":{"device":"d-example","channel":0,"lens":0},"name":"Camera A","file":"\(file.path)",
            "source_in_ms":\(inMs),"source_out_ms":\(outMs),"copy_possible":\(copy),"suggested_file":""}]}
            """
        return TrimPlan.parse(json)!.outputs[0]
    }

    @Test func keyframeTrimCopiesTheSamples() async throws {
        let dir = temporary()
        defer { try? FileManager.default.removeItem(at: dir) }
        let source = dir.appendingPathComponent("a.mp4"), target = dir.appendingPathComponent("trim.mp4")
        // 10 fps, a keyframe every 2 s, 6 s with sound.
        try await SyntheticRecording.write(.init(frames: 60, color: 1, audioHz: 440), to: source)
        let outcome = await gatedTrim(
            output(source, in: 2_000, out: 4_500, copy: true), exact: false, to: target, shareSafe: false, control: ExportControl())
        #expect(outcome.result == .done && outcome.width == 640 && outcome.height == 360)
        let asset = AVURLAsset(url: target)
        let duration = try await asset.load(.duration).seconds
        #expect(abs(duration - 2.5) < 0.15, "duration \(duration)")
        #expect(try await asset.loadTracks(withMediaType: .audio).count == 1)
        // Starts on the keyframe (frame 20) and ends before the out point; nothing was re-encoded
        // (the compressed sample count is what the range holds).
        #expect(try await look(target, at: 0) == (1, 20))
        #expect(try await look(target, at: 2_400) == (1, 44))
        let file = try await RecordedFileSource.open(target)
        #expect(file.info.keyframesMs.first == 0 && file.info.codec == "h264")
        file.close()
        #expect(!FileManager.default.fileExists(atPath: dir.appendingPathComponent(".nvrview-export").path))
    }

    @Test func exactTrimStartsAtThePictureAsked() async throws {
        let dir = temporary()
        defer { try? FileManager.default.removeItem(at: dir) }
        let source = dir.appendingPathComponent("a.mp4"), target = dir.appendingPathComponent("exact.mp4")
        try await SyntheticRecording.write(.init(frames: 60, color: 3), to: source)
        let outcome = await gatedTrim(
            output(source, in: 2_300, out: 4_000, copy: false), exact: true, to: target, shareSafe: false, control: ExportControl())
        #expect(outcome.result == .done)
        let duration = try await AVURLAsset(url: target).load(.duration).seconds
        #expect(abs(duration - 1.7) < 0.15, "duration \(duration)")
        let first = try await look(target, at: 0)
        #expect(first.color == 3 && abs(first.frame - 23) <= 1, "\(first)")
        let file = try await RecordedFileSource.open(target)
        #expect(file.info.keyframesMs.first == 0, "keyframes \(file.info.keyframesMs) duration \(file.info.durationMs)")
        file.close()
    }

    @Test func shareSafeTrimHasNoTimesNoFrameworkNamesNoUserData() async throws {
        let dir = temporary()
        defer { try? FileManager.default.removeItem(at: dir) }
        let source = dir.appendingPathComponent("a.mp4"), plain = dir.appendingPathComponent("plain.mp4"), safe = dir.appendingPathComponent("safe.mp4")
        try await SyntheticRecording.write(.init(frames: 60, color: 0, audioHz: 300), to: source)
        let cut = output(source, in: 0, out: 3_000, copy: true)
        _ = await gatedTrim(cut, exact: false, to: plain, shareSafe: false, control: ExportControl())
        let outcome = await gatedTrim(cut, exact: false, to: safe, shareSafe: true, control: ExportControl())
        #expect(outcome.result == .done)
        let bytes = try Data(contentsOf: safe)
        #expect(bytes.range(of: Data("Core Media".utf8)) == nil, "a framework handler name remains")
        #expect(bytes.range(of: Data("VideoHandler".utf8)) != nil && bytes.range(of: Data("SoundHandler".utf8)) != nil)
        // The movie, track, and media headers carry no time.
        for type in ["mvhd", "tkhd", "mdhd"] {
            var from = bytes.startIndex
            while let r = bytes.range(of: Data(type.utf8), in: from..<bytes.endIndex) {
                let version = bytes[r.upperBound]
                let length = version == 1 ? 16 : 8
                let times = bytes[(r.upperBound + 4)..<(r.upperBound + 4 + length)]
                #expect(times.allSatisfy { $0 == 0 }, "\(type) has a time")
                from = r.upperBound
            }
        }
        for name in ["udta", "meta"] { #expect(bytes.range(of: Data(name.utf8)) == nil, "\(name) remains") }
        // It still plays, and the plain export did record a creation time (so the check means something).
        #expect(abs(try await AVURLAsset(url: safe).load(.duration).seconds - 3.0) < 0.15)
        let plainBytes = try Data(contentsOf: plain)
        let mvhd = try #require(plainBytes.range(of: Data("mvhd".utf8)))
        #expect(plainBytes[(mvhd.upperBound + 4)..<(mvhd.upperBound + 8)].contains { $0 != 0 })
    }

    @Test func canceledTrimLeavesNothing() async throws {
        let dir = temporary()
        defer { try? FileManager.default.removeItem(at: dir) }
        let source = dir.appendingPathComponent("a.mp4"), target = dir.appendingPathComponent("never.mp4")
        try await SyntheticRecording.write(.init(frames: 60, color: 0), to: source)
        let control = ExportControl()
        control.cancel()
        let outcome = await gatedTrim(output(source, in: 0, out: 3_000, copy: true), exact: false, to: target, shareSafe: false, control: control)
        #expect(outcome.result == .canceled)
        let left = try FileManager.default.contentsOfDirectory(atPath: dir.path)
        #expect(left == ["a.mp4"], "\(left)")
    }

    @Test func followCutExportsTheCutsAndTheShownCamerasSound() async throws {
        let dir = temporary()
        defer { try? FileManager.default.removeItem(at: dir) }
        let a = dir.appendingPathComponent("a.mp4"), b = dir.appendingPathComponent("b.mp4"), target = dir.appendingPathComponent("cut.mp4")
        try await SyntheticRecording.write(.init(width: 320, height: 180, frames: 60, color: 0, audioHz: 440), to: a)
        try await SyntheticRecording.write(.init(width: 320, height: 180, frames: 60, color: 2, audioHz: 880), to: b)
        let cam = RecordingCamera(device: "d-example", channel: 0)
        let plan = EditPlan(
            kind: .follow, startMs: 10_000, endMs: 14_000,
            sources: [
                .init(source: 0, camera: cam, name: "A", file: a.path, fileStartMs: 10_000, width: 320, height: 180),
                .init(source: 1, camera: cam, name: "B", file: b.path, fileStartMs: 9_000, width: 320, height: 180),
            ],
            segments: [
                .init(startMs: 10_000, endMs: 12_000, source: 0, sourceInMs: 0),
                .init(startMs: 12_000, endMs: 14_000, source: 1, sourceInMs: 3_000),
            ])
        let control = ExportControl()
        let outcome = await gatedEdit(plan: plan, to: target, settings: .init(codec: .hevc, fps: 10), control: control)
        #expect(outcome.result == .done && outcome.width == 320 && outcome.height == 180 && outcome.frames == 40, "\(outcome)")
        #expect(control.fraction == 1)
        let asset = AVURLAsset(url: target)
        #expect(abs(try await asset.load(.duration).seconds - 4.0) < 0.2)
        let video = try #require(try await asset.loadTracks(withMediaType: .video).first)
        let format = try #require(try await video.load(.formatDescriptions).first)
        #expect(CMFormatDescriptionGetMediaSubType(format) == kCMVideoCodecType_HEVC)
        #expect(try await asset.loadTracks(withMediaType: .audio).count == 1)
        // Each side of the cut shows its own camera and the frame the plan says.
        #expect(try await look(target, at: 500) == (0, 5))
        #expect(try await look(target, at: 1_900) == (0, 19))
        #expect(try await look(target, at: 2_000) == (2, 30))
        #expect(try await look(target, at: 3_500) == (2, 45))
        #expect(await dominantFrequency(of: target, from: 0.2, to: 1.8) == 440)
        #expect(await dominantFrequency(of: target, from: 2.2, to: 3.8) == 880)
    }

    @Test func followCutWithTheSoundOffWritesNoAudioTrack() async throws {
        let dir = temporary()
        defer { try? FileManager.default.removeItem(at: dir) }
        let a = dir.appendingPathComponent("a.mp4"), target = dir.appendingPathComponent("silent.mp4")
        try await SyntheticRecording.write(.init(width: 320, height: 180, frames: 60, color: 0, audioHz: 440), to: a)
        let cam = RecordingCamera(device: "d-example", channel: 0)
        let plan = EditPlan(
            kind: .follow, startMs: 10_000, endMs: 13_000,
            sources: [.init(source: 0, camera: cam, name: "A", file: a.path, fileStartMs: 10_000, width: 320, height: 180)],
            segments: [.init(startMs: 10_000, endMs: 13_000, source: 0, sourceInMs: 0)])
        let outcome = await gatedEdit(plan: plan, to: target, settings: .init(fps: 10, sound: false), control: ExportControl())
        #expect(outcome.result == .done, "\(outcome)")
        #expect(try await AVURLAsset(url: target).loadTracks(withMediaType: .audio).isEmpty)
        let loud = dir.appendingPathComponent("loud.mp4")
        _ = await gatedEdit(plan: plan, to: loud, settings: .init(fps: 10), control: ExportControl())
        #expect(try await AVURLAsset(url: loud).loadTracks(withMediaType: .audio).count == 1, "the same plan has sound when it is on")
    }

    @Test func canceledExportLeavesNoFile() async throws {
        let dir = temporary()
        defer { try? FileManager.default.removeItem(at: dir) }
        let a = dir.appendingPathComponent("a.mp4"), target = dir.appendingPathComponent("never.mp4")
        try await SyntheticRecording.write(.init(width: 320, height: 180, frames: 60, color: 0), to: a)
        let cam = RecordingCamera(device: "d-example", channel: 0)
        let plan = EditPlan(
            kind: .follow, startMs: 10_000, endMs: 13_000,
            sources: [.init(source: 0, camera: cam, name: "A", file: a.path, fileStartMs: 10_000, width: 320, height: 180)],
            segments: [.init(startMs: 10_000, endMs: 13_000, source: 0, sourceInMs: 0)])
        let control = ExportControl()
        control.cancel()
        let outcome = await gatedEdit(plan: plan, to: target, settings: .init(fps: 10), control: control)
        #expect(outcome.result == .canceled)
        #expect(try FileManager.default.contentsOfDirectory(atPath: dir.path) == ["a.mp4"])
    }

    /// The loudest of a few candidate tone frequencies in a file's sound over a span, by correlation.
    private func dominantFrequency(of url: URL, from: Double, to: Double) async -> Int? {
        guard let asset = Optional(AVURLAsset(url: url)), let track = try? await asset.loadTracks(withMediaType: .audio).first,
            let reader = try? AVAssetReader(asset: asset)
        else { return nil }
        reader.timeRange = CMTimeRange(start: CMTime(seconds: from, preferredTimescale: 1000), end: CMTime(seconds: to, preferredTimescale: 1000))
        let out = AVAssetReaderTrackOutput(
            track: track,
            outputSettings: [
                AVFormatIDKey: kAudioFormatLinearPCM, AVSampleRateKey: 44_100, AVNumberOfChannelsKey: 1, AVLinearPCMBitDepthKey: 16,
                AVLinearPCMIsFloatKey: false, AVLinearPCMIsBigEndianKey: false, AVLinearPCMIsNonInterleaved: false,
            ])
        reader.add(out)
        guard reader.startReading() else { return nil }
        var samples: [Double] = []
        while let buffer = out.copyNextSampleBuffer(), let block = CMSampleBufferGetDataBuffer(buffer) {
            var data = Data(count: CMBlockBufferGetDataLength(block))
            data.withUnsafeMutableBytes { _ = CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: $0.count, destination: $0.baseAddress!) }
            data.withUnsafeBytes { raw in samples += raw.bindMemory(to: Int16.self).map { Double($0) } }
        }
        guard samples.count > 1000 else { return nil }
        func power(_ hz: Double) -> Double {
            var re = 0.0, im = 0.0
            for (i, s) in samples.enumerated() {
                let w = 2 * Double.pi * hz * Double(i) / 44_100
                re += s * cos(w)
                im += s * sin(w)
            }
            return re * re + im * im
        }
        return [440, 880].max { power(Double($0)) < power(Double($1)) }
    }
}

private func gatedTrim(_ o: TrimPlan.Output, exact: Bool, to url: URL, shareSafe: Bool, control: ExportControl) async -> ExportOutcome {
    await EncoderGate.run { await TrimExporter.export(o, exact: exact, to: url, shareSafe: shareSafe, control: control) }
}

private func gatedEdit(plan: EditPlan, to url: URL, settings: EditExporter.Settings, control: ExportControl) async -> ExportOutcome {
    await EncoderGate.run { await EditExporter.export(plan: plan, to: url, settings: settings, control: control) }
}
