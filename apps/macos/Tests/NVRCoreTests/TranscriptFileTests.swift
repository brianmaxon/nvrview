import Foundation
import NVRCore
import Testing

/// Saved call transcripts (CallTranscriptFile). The vectors are the Windows
/// app's TranscriptFileTests (NvrView.Tiles.Tests), the same numbers in the
/// same order; keep the two copies in step.
@Suite struct TranscriptFileTests {
    /// 2023-11-14 22:13:20 UTC.
    let t0 = Date(timeIntervalSince1970: 1_700_000_000)
    let utc = TimeZone(secondsFromGMT: 0)!
    let minus4 = TimeZone(secondsFromGMT: -4 * 3600)!
    let plus530 = TimeZone(secondsFromGMT: 5 * 3600 + 1800)!

    @Test func fileNamesCarryTheStartAndTheCamera() {
        #expect(CallTranscriptFile.fileName(startedAt: t0, camera: "Front door", timeZone: utc) == "2023-11-14 22.13.20 Front door.txt")
        #expect(CallTranscriptFile.fileName(startedAt: t0, camera: "Front door", timeZone: minus4) == "2023-11-14 18.13.20 Front door.txt")
        #expect(CallTranscriptFile.fileName(startedAt: t0, camera: "a/b", timeZone: plus530) == "2023-11-15 03.43.20 a-b.txt")
    }

    @Test func cameraNamesAreMadeSafe() {
        let vectors: [(String, String)] = [
            ("Front door", "Front door"),
            ("  Back/yard: gate*?  ", "Back-yard- gate--"),
            ("Porch\t\nCam", "Porch Cam"),
            ("...", "Camera"),
            ("", "Camera"),
            ("Garage.", "Garage"),
            ("a\u{1}b", "a-b"),
            ("<Side> \"door\" | 2", "-Side- -door- - 2"),
            (String(repeating: "x", count: 70), String(repeating: "x", count: 60)),
        ]
        for (name, safe) in vectors {
            #expect(CallTranscriptFile.sanitized(name) == safe, "\(name)")
        }
    }

    @Test func namesDoNotCollide() {
        #expect(CallTranscriptFile.uniqueName("a.txt", taken: []) == "a.txt")
        #expect(CallTranscriptFile.uniqueName("a.txt", taken: ["A.TXT"]) == "a 2.txt")
        #expect(CallTranscriptFile.uniqueName("a.txt", taken: ["a.txt", "a 2.txt"]) == "a 3.txt")
    }

    @Test func theTextHasTheHeaderThenEachLineWithItsTimeAndSide() {
        let lines = [
            Transcript.Line(source: .visitor, text: "Hello there.", isFinal: true, at: t0.addingTimeInterval(7)),
            Transcript.Line(source: .you, text: "One moment\nplease.", isFinal: true, at: t0.addingTimeInterval(12)),
            Transcript.Line(source: .visitor, text: "Thanks", isFinal: true),
        ]
        let text = CallTranscriptFile.text(
            camera: "Front door", presses: 2, startedAt: t0, endedAt: t0.addingTimeInterval(72.4), lines: lines,
            timeZone: minus4)
        #expect(
            text == """
                NVRView call transcript
                Camera: Front door
                Call: doorbell, 2 presses
                Started: 2023-11-14 18:13:20 UTC-04:00
                Ended: 2023-11-14 18:14:32 (72 s)

                18:13:27 Visitor: Hello there.
                18:13:32 You: One moment please.
                --:--:-- Visitor: Thanks

                """)
        let conversation = CallTranscriptFile.text(
            camera: "Side", presses: 0, startedAt: t0, endedAt: t0.addingTimeInterval(-5), lines: [], timeZone: utc)
        #expect(
            conversation == """
                NVRView call transcript
                Camera: Side
                Call: conversation started in the app
                Started: 2023-11-14 22:13:20 UTC
                Ended: 2023-11-14 22:13:15 (0 s)


                """)
        let one = CallTranscriptFile.text(
            camera: "Side", presses: 1, startedAt: t0, endedAt: t0, lines: [], timeZone: plus530)
        #expect(one.contains("Call: doorbell, 1 press\n") && one.contains("Started: 2023-11-15 03:43:20 UTC+05:30\n"))
    }

    private func f(_ name: String, _ bytes: Int, _ t: Double) -> CallTranscriptFile.StoredFile {
        .init(name: name, bytes: bytes, modified: t0.addingTimeInterval(t))
    }

    @Test func theOldestGoFirstBeyondTheBounds() {
        let five = [f("a", 100, 1), f("b", 100, 2), f("c", 100, 3), f("d", 100, 4), f("e", 100, 5)]
        #expect(CallTranscriptFile.toDelete(five.shuffled(), maxFiles: 3, maxBytes: 1000) == ["a", "b"])
        #expect(CallTranscriptFile.toDelete(five, maxFiles: 10, maxBytes: 250) == ["a", "b", "c"])
        #expect(CallTranscriptFile.toDelete(five, maxFiles: 10, maxBytes: 1000) == [])
        // The newest is kept even when it alone is over the size bound.
        #expect(CallTranscriptFile.toDelete([f("x", 5000, 9)], maxFiles: 10, maxBytes: 1000) == [])
        #expect(CallTranscriptFile.toDelete([f("y", 10, 1), f("x", 5000, 9)], maxFiles: 10, maxBytes: 1000) == ["y"])
        // Same time: the later name is the newer.
        #expect(CallTranscriptFile.toDelete([f("B", 1, 1), f("A", 1, 1)], maxFiles: 1, maxBytes: 1000) == ["A"])
        // The defaults: 500 files, 50 MB.
        let many = (0..<501).map { f(String(format: "%03d", $0), 1, Double($0)) }
        #expect(CallTranscriptFile.toDelete(many) == ["000"])
        #expect(CallTranscriptFile.maxFiles == 500 && CallTranscriptFile.maxBytes == 52_428_800)
    }

    @Test func aCallGivesItsFileOnlyWhenSomethingWasSaid() {
        let door = CameraRef(device: "d-1", channel: 3)
        var call = DoorbellCall(camera: door, startedAt: t0, now: 0, audio: .overlay)
        #expect(call.transcriptFile(cameraName: "Front door", endedAt: t0, timeZone: utc) == nil)
        call.transcript.partial("Hel", source: .visitor)
        #expect(call.transcriptFile(cameraName: "Front door", endedAt: t0, timeZone: utc) == nil)
        call.transcript.final("Hello", source: .visitor, at: t0.addingTimeInterval(3))
        let file = call.transcriptFile(cameraName: "Front door", endedAt: t0.addingTimeInterval(10), timeZone: utc)
        #expect(file?.name == "2023-11-14 22.13.20 Front door.txt")
        #expect(file?.text.hasSuffix("\n22:13:23 Visitor: Hello\n") == true)
        #expect(file?.text.contains("Call: doorbell, 1 press\n") == true)
    }
}
