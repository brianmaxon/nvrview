import Foundation
import NVRCore
import Testing

/// Sound in the Recordings window (RecordingsSound.swift); the same vectors
/// as the Windows app's RecordingsSoundTests.
@Suite struct RecordingsSoundTests {
    let d = "d-example"

    @Test func kinds() {
        #expect(SoundKinds.isSound(kind: "dog_bark", source: nil))
        #expect(SoundKinds.isSound(kind: "cough", source: "local_audio"))
        #expect(!SoundKinds.isSound(kind: "people", source: "device"))
        #expect(SoundKinds.name("glass_break") == "Glass breaking")
        #expect(SoundKinds.name("smoke_alarm") == "Smoke alarm")
        #expect(SoundKinds.name("loud_cough") == "Loud cough")
        // Sounds is a filter of its own; alarms belong to it.
        #expect(KindFilter(kind: "smoke_alarm") == .sound && KindFilter(kind: "dog_bark") == .sound)
        #expect(KindFilter(kind: "dog_cat") == .animal && KindFilter(kind: "people") == .people)
        #expect(KindFilter.passes(["siren"], [.sound]) && !KindFilter.passes(["people"], [.sound]))
    }

    @Test func soundFilterMatchesAnyLocalAudioEvent() throws {
        let json = #"""
            {"key":"k","start":100,"end":110,"kinds":["cough"],"cameras":[],
             "events":[{"camera":{"device":"d-example","channel":0},"kind":"cough","source":"local_audio","start":100,"end":110}]}
            """#
        let incident = try #require(Incident.parse(answer: json))
        #expect(KindFilter.passes(incident, [.sound]))
        #expect(!KindFilter.passes(incident, [.people]))
        #expect(SoundKinds.names(incident.events) == ["Cough"])
    }

    @Test func loudness() throws {
        #expect(LoudnessScale.bucket(visibleSeconds: 86_400, width: 720) == 300)
        #expect(LoudnessScale.bucket(visibleSeconds: 1800, width: 720) == 5)
        #expect(LoudnessScale.bucket(visibleSeconds: 86_400, width: 1) == 3600)
        #expect(LoudnessScale.level(-80) == 0 && LoudnessScale.level(0) == 1 && LoudnessScale.level(-40) == 0.5)
        #expect(LoudnessScale.level(-100) == 0 && LoudnessScale.level(5) == 1)
        let a = RecordingCamera(device: d, channel: 3), b = RecordingCamera(device: d, channel: 3, lens: 1)
        let c = RecordingCamera(device: d, channel: 4)
        #expect(LoudnessScale.microphones([a, b, c]) == [a, c])
        #expect(LoudnessScale.microphone(of: b) == a)
        #expect(
            LoudnessScale.query(start: 0, end: 86_399, bucket: 60, cameras: [a])
                == #"{"start":0,"end":86399,"bucket_s":60,"cameras":[{"device":"d-example","channel":3,"lens":0}]}"#)
        let answer = try #require(LoudnessAnswer.parse(answer: #"""
            {"revision":41,"lanes":[{"camera":{"device":"d-example","channel":3,"lens":0},"bucket_s":60,"start":0,
             "peak_db":[-48,null,-12],"rms_db":[-60,null,-30]}]}
            """#))
        #expect(answer.revision == 41 && answer.lanes.count == 1)
        #expect(answer.lanes[0].peak == [-48, nil, -12] && answer.lanes[0].rms == [-60, nil, -30])
        #expect(answer.lanes[0].bucket == 60 && answer.lanes[0].heard)
        #expect(!LoudnessLane(camera: a, bucket: 60, start: 0, peak: [nil], rms: [nil]).heard)
    }

    @Test func listening() {
        let four = (0..<4).map { RecordingCamera(device: d, channel: $0) }
        #expect(!SoundListening.canAdd(RecordingCamera(device: d, channel: 9), to: four))
        #expect(SoundListening.canAdd(four[0], to: four))
        #expect(SoundListening.canAdd(RecordingCamera(device: "d-other", channel: 0), to: four))
        #expect(SoundListening.describe(state: "refused", reason: "no_audio") == "No sound from this camera")
        #expect(SoundListening.describe(state: "watching", reason: nil) == "Listening")
    }
}
