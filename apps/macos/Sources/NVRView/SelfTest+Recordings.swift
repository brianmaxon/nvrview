import AppKit
import AVFoundation
import QuartzCore
import Foundation
import FakeCore
import NVRCore
import VideoKit

// The recordings window, playback, edits, export, sound analysis (fake and real core): the self-test's "recordings" steps (scripts/macos-selftest-areas.txt).
// Part of SelfTest (SelfTest.swift has the order of the steps and the shared helpers).

/// The self-test's own `print` (elapsed seconds in front; see SelfTest.swift).
private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// What the recordings screens handed to the system (the Trash, the
    /// Finder) during the step: recorded, never done.
    final class SystemCalls {
        var trashed: [URL] = []
        var revealed: [[URL]] = []
    }

    /// The recordings window (docs/UX.md "Recordings") against the fake
    /// core's made-up recording store: choosing cameras, the day asked for
    /// and today held under the window's own hold, the calendar and the
    /// timeline lanes, the animal filter, a cover thumbnail decoded, shrunk
    /// and put back, the edits, a lock, the downloads panel, camera groups,
    /// and the hold released when the window closes. The fake finishes
    /// fetches and downloads only when told (`completeRecordingWork`).
    func recordingsStep() {
        guard let model = app.model, let fake = model.fake, let rec = model.recordings, rec.available,
            let device = model.devices.first?.key
        else { return checkLog() }
        let calls = SystemCalls()
        rec.trasher = { calls.trashed.append($0) }
        rec.revealer = { calls.revealed.append($0) }
        // parity: recordings-enable
        let cams = (0..<3).map { RecordingCamera(device: device, channel: $0) }
        for c in cams { rec.setEnabled(c, true) }
        if rec.enabledCameras != cams { problems.append("recordings: the chosen cameras are \(rec.enabledCameras)") }
        guard let controller = app.openRecordingsWindow() else {
            problems.append("recordings: no window opened")
            return checkLog()
        }
        let b = controller.browser
        // parity: recordings-hold-today
        when({ fake.recordingsHolds[b.holdID]?.count == 3 && b.lanes.count == 3 }, timeout: 3) { [self] in
            print("recordings: hold \(b.holdID) on \(fake.recordingsHolds[b.holdID]?.count ?? 0) cameras, \(b.lanes.count) lanes")
            if !b.holding || fake.recordingsHolds[b.holdID]?.count != 3 { problems.append("recordings: today is not held") }
            // parity: recordings-calendar-lanes
            when({ b.recordedDays.contains(b.day) && b.lanes.allSatisfy { !$0.spans.isEmpty } && b.dayIncidents.count == 7 },
                timeout: 3
            ) { [self] in
                let events = b.lanes.reduce(0) { $0 + $1.events.count }
                print("recordings: calendar \(b.recordedDays.count) days, \(events) events, \(b.dayIncidents.count) incidents")
                if !b.recordedDays.contains(b.day) { problems.append("recordings: the calendar does not mark today") }
                if events != 7 || b.dayIncidents.count != 7 {
                    problems.append("recordings: \(events) events and \(b.dayIncidents.count) incidents, expected 7 and 7")
                }
                if let lane = b.lanes.first, let e = lane.events.first {
                    b.select(event: e)
                    if b.selected?.involves(e.camera) != true { problems.append("recordings: a timeline click selected nothing") }
                }
                // parity: recordings-animal-filter
                b.kindFilters = [.animal]
                let animals = b.incidents.map(\.kinds)
                print("recordings: animal filter \(animals)")
                if animals != [["dog_cat"]] { problems.append("recordings: the animal filter shows \(animals)") }
                b.kindFilters = [.people, .vehicle]
                if b.incidents.count != 6 { problems.append("recordings: people and vehicles show \(b.incidents.count), expected 6") }
                b.kindFilters = []
                recordingsNavigate(b, fake, rec, calls)
            }
        }
    }

    /// A day before today is asked for but not held; back on today it is held again.
    func recordingsNavigate(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: recordings-navigate-day
        let today = b.day
        b.previousDay()
        if b.holding || fake.recordingsHolds[b.holdID] != nil { problems.append("recordings: yesterday is held") }
        when({ b.dayIncidents.count == 7 && b.recordedDays.contains(today - DeviceTime.day) }, timeout: 3) { [self] in
            print("recordings: yesterday has \(b.dayIncidents.count) incidents")
            b.nextDay()
            if b.day != today || fake.recordingsHolds[b.holdID]?.count != 3 { problems.append("recordings: today is not held again") }
            when({ b.dayIncidents.count == 7 }, timeout: 3) { [self] in recordingsThumbnail(b, fake, rec, calls) }
        }
    }

    /// A cover thumbnail: pending, its event, the keyframe decoded and shrunk
    /// off the main thread, and the small image put back into the core's cache.
    func recordingsThumbnail(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: recordings-thumbnail
        guard let incident = b.dayIncidents.first, let key = incident.coverKeys.first else {
            problems.append("recordings: no incident for a thumbnail")
            return recordingsEdits(b, fake, rec, calls)
        }
        rec.requestThumbnail(key)
        if rec.thumbnails[key] != .pending { problems.append("recordings: a thumbnail was not pending first") }
        fake.completeRecordingWork()
        when({
            if case .image = rec.thumbnails[key] { return true }
            if case .failed = rec.thumbnails[key] { return true }
            return false
        }, timeout: 5) { [self] in
            guard case .image(let jpeg) = rec.thumbnails[key] else {
                problems.append("recordings: the thumbnail is \(String(describing: rec.thumbnails[key]))")
                return recordingsEdits(b, fake, rec, calls)
            }
            let image = NSImage(data: jpeg)
            let cached = try? fake.thumbnailGet(key: key.json)
            var putBack = false
            if case .ready(let t)? = cached, t.kind == .image, t.data == jpeg { putBack = true }
            let size = image.map { "\(Int($0.size.width))x\(Int($0.size.height))" } ?? "none"
            print("recordings: thumbnail \(jpeg.count) bytes, \(size), put back \(putBack)")
            if image == nil || (image?.size.width ?? 0) > 320 { problems.append("recordings: the thumbnail image is \(size)") }
            if !putBack { problems.append("recordings: the small image was not put back in the core's cache") }
            recordingsEdits(b, fake, rec, calls)
        }
    }

    /// Rename, merge, split, add and remove a camera, and revert, through
    /// the browser as the window's controls call them.
    func recordingsEdits(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: recordings-edits
        let people = b.dayIncidents.filter { $0.kinds == ["people"] }.sorted { $0.start < $1.start }
        guard people.count == 3 else {
            problems.append("recordings: \(people.count) people incidents, expected 3")
            return recordingsLock(b, fake, rec, calls)
        }
        b.rename(people[0].key, "Delivery")
        when({ b.dayIncidents.contains { $0.name == "Delivery" } }, timeout: 2) { [self] in
            b.selection = [people[0].key, people[1].key]
            b.merge()
            when({ b.dayIncidents.count == 6 }, timeout: 2) { [self] in
                guard let merged = b.selected, merged.cameras.count == 2 else {
                    problems.append("recordings: the merge did not make one incident of two cameras")
                    return recordingsLock(b, fake, rec, calls)
                }
                print("recordings: merged into \(merged.cameras.count) cameras, edited \(merged.edited)")
                b.split(merged.key, at: people[1].start)
                when({ b.dayIncidents.count == 7 }, timeout: 2) { [self] in
                    let first = people[0].key
                    b.add(RecordingCamera(device: people[2].cameras[0].camera.device, channel: 2), to: first)
                    when({ b.dayIncidents.first { $0.key == first }?.cameras.count == 2 }, timeout: 2) { [self] in
                        b.remove(RecordingCamera(device: people[2].cameras[0].camera.device, channel: 2), from: first)
                        when({ b.dayIncidents.first { $0.key == first }?.cameras.count == 1 }, timeout: 2) { [self] in
                            b.revert(first)
                            when({ b.dayIncidents.allSatisfy { !$0.edited } && b.dayIncidents.count == 7 }, timeout: 2) {
                                [self] in
                                print("recordings: rename, merge, split, add, remove, revert done")
                                if b.dayIncidents.contains(where: \.edited) { problems.append("recordings: revert left an edit") }
                                recordingsLock(b, fake, rec, calls)
                            }
                        }
                    }
                }
            }
        }
    }

    /// Locking queues the incident's download and says so; unlocking keeps it.
    func recordingsLock(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: recordings-lock
        guard let vehicle = b.dayIncidents.first(where: { $0.kinds == ["vehicle"] }) else {
            problems.append("recordings: no vehicle incident to lock")
            return recordingsDownloads(b, fake, rec, calls)
        }
        b.setLocked(vehicle.key, true)
        when({ b.dayIncidents.first { $0.key == vehicle.key }?.locked == true && rec.downloads.count == 1 }, timeout: 2) {
            [self] in
            print("recordings: locked; \(rec.notice ?? "no message"); downloads \(rec.downloads.count)")
            if rec.notice?.contains("downloading") != true { problems.append("recordings: locking did not say the video is kept") }
            b.lockedOnly = true
            if b.incidents.map(\.key) != [vehicle.key] { problems.append("recordings: the Locked filter shows \(b.incidents.count)") }
            b.lockedOnly = false
            b.setLocked(vehicle.key, false)
            when({ b.dayIncidents.first { $0.key == vehicle.key }?.locked == false }, timeout: 2) { [self] in
                if rec.downloads.count != 1 { problems.append("recordings: unlocking touched the download") }
                recordingsDownloads(b, fake, rec, calls)
            }
        }
    }

    /// The downloads panel: pause, resume, done, Show in Finder, forget,
    /// and the Trash once the core has let go of the folder.
    func recordingsDownloads(
        _ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls
    ) {
        // parity: recordings-downloads
        guard let first = rec.downloads.first, let other = b.dayIncidents.first(where: { $0.kinds == ["dog_cat"] }) else {
            problems.append("recordings: nothing to download")
            return recordingsGroups(b, fake, rec, calls)
        }
        b.center = .downloads
        rec.pause(first.download, true)
        when({ rec.downloads.first?.paused == true }, timeout: 2) { [self] in
            fake.completeRecordingWork()
            if rec.downloads.first?.state == "done" { problems.append("recordings: a paused download finished") }
            rec.pause(first.download, false)
            when({ rec.downloads.first?.paused == false }, timeout: 2) { [self] in
                fake.completeRecordingWork()
                when({ rec.downloads.first?.state == "done" }, timeout: 2) { [self] in
                    guard let done = rec.downloads.first else { return }
                    rec.reveal(done)
                    if calls.revealed.last?.map(\.path) != done.outputs { problems.append("recordings: Show in Finder showed \(calls.revealed)") }
                    rec.forget(done.download)
                    b.download(other.key)
                    when({ rec.downloads.count == 1 && rec.downloads.first?.download != done.download }, timeout: 2) { [self] in
                        guard let queued = rec.downloads.first, let folder = queued.folder else { return }
                        rec.trash(queued.download)
                        when({ !calls.trashed.isEmpty }, timeout: 2) { [self] in
                            print("recordings: downloads done, forgotten, trashed \(calls.trashed.count)")
                            if calls.trashed.map(\.path) != [folder] { problems.append("recordings: the Trash got \(calls.trashed)") }
                            if !rec.downloads.isEmpty { problems.append("recordings: \(rec.downloads.count) downloads left") }
                            b.center = .player
                            recordingsGroups(b, fake, rec, calls)
                        }
                    }
                }
            }
        }
    }

    /// A camera group joins its cameras' overlapping events; the window
    /// closing releases its hold.
    func recordingsGroups(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: recordings-groups
        let two = Array(b.cameras.prefix(2))
        rec.putGroups(IncidentGroups(groups: [.init(name: "Front", cameras: two)]))
        when({ rec.groups.groups.count == 1 && b.dayIncidents.count == 5 }, timeout: 2) { [self] in
            print("recordings: with a group of two, \(b.dayIncidents.count) incidents")
            if b.dayIncidents.count != 5 { problems.append("recordings: a group of two gave \(b.dayIncidents.count) incidents") }
            b.scope = .group("Front")
            if b.incidents.count != 2 { problems.append("recordings: the group scope shows \(b.incidents.count)") }
            b.scope = .all
            rec.putGroups(IncidentGroups())
            when({ rec.groups.groups.isEmpty && b.dayIncidents.count == 7 }, timeout: 2) { [self] in
                recordingsMonth(b, fake, rec)
            }
        }
    }

    /// Paging the calendar to the previous month asks for that month's
    /// calendar alone (minor 7) and marks its days; camera names are the
    /// camera list's.
    func recordingsMonth(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: recordings-calendar-month
        let asked = fake.recordingsMonthRequests
        let shown = b.month
        b.showMonth(adding: -1)
        let previous = b.month
        when({ fake.recordingsMonthRequests > asked && !b.recordedDays.isEmpty }, timeout: 2) { [self] in
            let inMonth = b.recordedDays.allSatisfy { DeviceTime.month(containing: $0).start == previous }
            print("recordings: previous month asked \(fake.recordingsMonthRequests - asked) time(s), \(b.recordedDays.count) days marked")
            if fake.recordingsMonthRequests != asked + 1 { problems.append("recordings: paging asked \(fake.recordingsMonthRequests - asked) months") }
            if b.recordedDays.isEmpty || !inMonth { problems.append("recordings: the previous month shows \(b.recordedDays.count) days") }
            // Paging back and forth asks nothing again.
            b.showMonth(adding: 1)
            b.showMonth(adding: -1)
            if fake.recordingsMonthRequests != asked + 1 { problems.append("recordings: a month already asked was asked again") }
            b.showMonth(adding: 1)
            if b.month != shown { problems.append("recordings: the calendar did not come back to the shown day's month") }
            // parity: recordings-camera-names
            let names = b.lanes.map(\.name)
            let listed = b.cameras.map { c in rec.app?.name(of: CameraRef(device: c.device, channel: c.channel, lens: c.lens)) ?? "" }
            print("recordings: camera names \(names)")
            if names != listed { problems.append("recordings: lanes are named \(names), the camera list \(listed)") }
            recordingsPlayback(b, fake, rec)
        }
    }

    /// The player (milestone P, M2): selecting an incident plays its cameras
    /// from its start less the margin; the pictures decode and reach the
    /// screen; pause, skip, speed (keyframes only and no sound from 2×), the
    /// grid's large and single views with the sound following, a lane click
    /// that seeks, Remove from Incident and Split Here at the playhead
    /// without reopening the playback, and the playback closed with the
    /// window.
    func recordingsPlayback(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: playback-open
        let p = b.player
        // The three cameras' people incidents (a second apart) as one.
        let people = b.dayIncidents.filter { $0.kinds == ["people"] }.map(\.key)
        guard people.count == 3 else {
            problems.append("recordings: \(people.count) people incidents to play, expected 3")
            return recordingsClose(b, fake)
        }
        b.selection = Set(people)
        b.merge()
        let opens = p.opens
        whenPlayback(fake, {
            p.playback != nil && p.cells.count == 3 && p.cells.allSatisfy { $0.tile.latestPicture() != nil }
                && (self.app.recordingsWindows.first?.stage?.framesDrawn ?? 0) > 0
        }, timeout: 5) { [self] in
            guard let incident = b.selected else { return failPlayback("the merge selected nothing", b, fake) }
            let start = (incident.start - incident.margin) * 1000
            print("recordings: playback \(p.playback ?? 0) of \(p.cells.count) cameras, \(p.state.map { "\($0)" } ?? "no state"), opens \(p.opens - opens + 1), frames drawn \(app.recordingsWindows.first?.stage?.framesDrawn ?? 0)")
            if p.playback == nil || p.cells.count != 3 { return failPlayback("the incident did not play its three cameras", b, fake) }
            if p.cells.contains(where: { $0.tile.latestPicture() == nil }) {
                let detail = p.cells.map { "\($0.camera.channel): \($0.state) \($0.tile.snapshot.status) configures \($0.tile.snapshot.configures)" }
                problems.append("recordings: a playback tile decoded no picture (\(detail))")
            }
            if (app.recordingsWindows.first?.stage?.framesDrawn ?? 0) == 0 { problems.append("recordings: the player drew no frame") }
            if p.positionMs < start || p.positionMs > start + 5_000 { problems.append("recordings: the playback starts at \(p.positionMs), not \(start)") }
            if !p.transport.isPlaying { problems.append("recordings: the selected incident is not playing") }
            if p.incidentKey != incident.key { problems.append("recordings: the player plays another incident") }
            playbackTransport(b, fake, rec)
        }
    }

    /// `when` for the playback steps: the fake core sends its queued
    /// playback events and starts its tiles' streams only when told
    /// (`Options.recordingWorkIsManual`), so each poll tells it.
    func whenPlayback(
        _ fake: FakeCore, _ condition: @escaping @MainActor () -> Bool, timeout: Double,
        _ body: @escaping @MainActor () -> Void
    ) {
        when({
            fake.completePlaybackWork()
            return condition()
        }, timeout: timeout, body)
    }

    func failPlayback(_ why: String, _ b: RecordingsBrowser, _ fake: FakeCore) {
        problems.append("recordings: " + why)
        recordingsClose(b, fake)
    }

    func playbackTransport(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: playback-transport
        let p = b.player
        let opens = p.opens
        p.pause()
        whenPlayback(fake, { p.state == .paused }, timeout: 2) { [self] in
            let before = p.positionMs
            p.skip(by: TransportState.skipMs)
            if p.positionMs != min(before + TransportState.skipMs, p.endMs) { problems.append("recordings: +10 s moved to \(p.positionMs) from \(before)") }
            p.setRate(4)
            let sound = p.cells.compactMap { fake.audioSetting(tile: $0.tile.id)?.audible }
            if PlaybackSpeed.hint(p.rate) != "Keyframes only, no sound" || sound.contains(true) {
                problems.append("recordings: at 4× the hint is \(PlaybackSpeed.hint(p.rate) ?? "none") and sound \(sound)")
            }
            p.setRate(1)
            p.play()
            whenPlayback(fake, { p.state == .playing && p.rate == 1 }, timeout: 2) { [self] in
                let audible = p.cells.map { fake.audioSetting(tile: $0.tile.id)?.audible ?? false }
                print("recordings: paused, skipped 10 s, 4× and back, playing; audible \(audible)")
                if audible != [true, false, false] { problems.append("recordings: at 1× the sound is \(audible), expected the first camera") }
                if p.opens != opens { problems.append("recordings: the transport reopened the playback") }
                playbackGrid(b, fake, rec)
            }
        }
    }

    func playbackGrid(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: playback-grid
        let p = b.player
        let opens = p.opens
        p.click(1)
        let emphasis = p.mode
        let afterClick = p.cells.map { fake.audioSetting(tile: $0.tile.id)?.audible ?? false }
        p.doubleClick(1)
        let single = p.mode
        p.doubleClick(1)
        print("recordings: grid click \(emphasis), double click \(single), back \(p.mode); sound \(afterClick)")
        if emphasis != .emphasis(1) || single != .single(1) || p.mode != .grid {
            problems.append("recordings: the grid went \(emphasis), \(single), \(p.mode)")
        }
        if afterClick != [false, true, false] { problems.append("recordings: the large camera's sound did not play (\(afterClick))") }
        p.muted = true
        if p.cells.contains(where: { fake.audioSetting(tile: $0.tile.id)?.audible == true }) { problems.append("recordings: muted, a camera is audible") }
        p.muted = false
        // parity: playback-scrub
        guard let lane = b.lanes.first(where: { p.cameras.contains($0.camera) }), let incident = b.playingIncident else {
            return failPlayback("no lane of a playing camera", b, fake)
        }
        let target = incident.end + 2
        let seeks = p.seeks
        b.laneClick(lane.camera, at: target)
        if p.seeks != seeks + 1 || p.positionMs != target * 1000 || p.opens != opens {
            problems.append("recordings: a lane click went to \(p.positionMs) (seeks \(p.seeks - seeks), opens \(p.opens - opens))")
        }
        whenPlayback(fake, { p.state == .playing && p.positionMs >= target * 1000 }, timeout: 2) { [self] in
            print("recordings: a lane click seeked to \(TransportState.clock(p.positionMs)) and plays")
            playbackEdits(b, fake, rec)
        }
    }

    func playbackEdits(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: playback-remove-camera
        let p = b.player
        let opens = p.opens
        guard let incident = b.playingIncident, incident.cameras.count == 3, let third = p.cameras.last else {
            return failPlayback("the playing incident does not have three cameras", b, fake)
        }
        b.remove(third, from: incident.key)
        whenPlayback(fake, { b.playingIncident?.cameras.count == 2 && p.cells.count == 2 }, timeout: 2) { [self] in
            print("recordings: removed a camera; the player has \(p.cells.count), opens \(p.opens - opens)")
            if p.cells.count != 2 || p.opens != opens || p.cameras.contains(third) {
                problems.append("recordings: Remove from Incident left \(p.cells.count) cameras, \(p.opens - opens) opens")
            }
            // parity: playback-split-here
            guard let playing = b.playingIncident else { return failPlayback("nothing plays after the removal", b, fake) }
            p.seek(to: (playing.start + 1) * 1000)
            let at = b.splitSecond(playing)
            if at != playing.start + 1 { problems.append("recordings: Split Here would split at \(String(describing: at))") }
            let before = b.dayIncidents.count
            b.splitHere(playing)
            whenPlayback(fake, { b.dayIncidents.count == before + 1 && p.cells.count == 1 }, timeout: 2) { [self] in
                print("recordings: split at the playhead; \(b.dayIncidents.count - before) more incident, the player has \(p.cells.count), opens \(p.opens - opens)")
                if b.dayIncidents.count != before + 1 { problems.append("recordings: Split Here made \(b.dayIncidents.count - before) more incidents") }
                if p.opens != opens || b.playingIncident == nil || b.playingIncident?.key != b.selected?.key {
                    problems.append("recordings: after Split Here the player reopened or lost its incident")
                }
                editViews(b, fake, rec)
            }
        }
    }

    // MARK: Follow cut and grids (milestone P, M3)

    /// The edit player's simulated host clock (seconds).
    final class ManualClock {
        var now: Double = 1000
    }

    /// Synthetic files being written.
    final class Writing {
        var done = false
        var failure: String?
    }

    /// The follow cut and the 4-up grid of a downloaded incident, on the fake
    /// core's plans (`editCompute`) over synthetic files written where its
    /// download says they are: each camera's pictures are its own color with
    /// the frame number in a strip (`SyntheticRecording`). Frames of each are
    /// drawn with Metal from the VideoToolbox-decoded files and checked at
    /// sample times for which camera is shown and which of its frames; then
    /// the cuts, the sound following the shown camera, the transport on a
    /// simulated clock, the picture on screen, and the way back to the
    /// cameras.
    func editViews(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel) {
        // parity: edit-download
        let vehicles = b.dayIncidents.filter { $0.kinds == ["vehicle"] }.map(\.key)
        guard vehicles.count == 3 else {
            problems.append("edits: \(vehicles.count) vehicle incidents, expected 3")
            return recordingsLayout(b, fake)
        }
        b.selection = Set(vehicles)
        b.merge()
        when({ b.selected?.cameras.count == 3 }, timeout: 2) { [self] in
            guard let incident = b.selected, incident.cameras.count == 3 else {
                problems.append("edits: the vehicle incidents did not merge")
                return recordingsLayout(b, fake)
            }
            if b.editable != nil { problems.append("edits: an incident not downloaded offers a follow cut") }
            b.show(view: .follow)
            if b.playerView != .cameras { problems.append("edits: Follow Cut showed without a download") }
            b.message = nil
            b.download(incident.key)
            when({
                fake.completeRecordingWork()
                return rec.finishedDownload(of: incident.key) != nil
            }, timeout: 3) { [self] in
                guard let d = rec.finishedDownload(of: incident.key),
                    let raw = try? fake.editCompute(json: #"{"kind":"follow","download":"\#(d.download)"}"#),
                    let object = try? JSONSerialization.jsonObject(with: Data(raw.utf8)) as? [String: Any],
                    let sources = object["sources"] as? [[String: Any]]
                else {
                    problems.append("edits: the incident did not download, or the fake planned nothing")
                    return recordingsLayout(b, fake)
                }
                let files = sources.compactMap { s -> (URL, SyntheticRecording.Spec)? in
                    guard let file = s["file"] as? String, let n = s["source"] as? Int,
                        let ms = (s["duration_ms"] as? NSNumber)?.intValue
                    else { return nil }
                    return (URL(fileURLWithPath: file), SyntheticRecording.Spec(frames: ms / 100 + 1, color: n))
                }
                let writing = Writing()
                Task { @MainActor in
                    do {
                        for (url, spec) in files {
                            let t0 = CACurrentMediaTime()
                            try await SyntheticRecording.write(spec, to: url)
                            print(String(format: "edits: wrote %d frames in %.2f s", spec.frames, CACurrentMediaTime() - t0))
                        }
                    } catch {
                        writing.failure = String(describing: error)
                    }
                    writing.done = true
                }
                when({ writing.done }, timeout: 30) { [self] in
                    print("edits: downloaded \(d.download), \(files.count) synthetic files written\(writing.failure.map { ", failed: \($0)" } ?? "")")
                    if let f = writing.failure { problems.append("edits: the synthetic files were not written (\(f))") }
                    if files.count != 3 { problems.append("edits: the download has \(files.count) files, expected 3") }
                    editFollow(b, fake, rec, incident, folder: d.folder)
                }
            }
        }
    }

    /// Renders `samples` of a plan offscreen and returns what did not match.
    func editMismatches(
        _ session: EditSession, _ renderer: GridRenderer, samples: [Int64], width: Int, height: Int
    ) -> [String] {
        let plan = session.plan
        let bounds = CGRect(x: 0, y: 0, width: width, height: height)
        var out: [String] = []
        for t in samples {
            guard let image = session.render(at: t, width: width, height: height, with: renderer) else {
                out.append("\(t): nothing rendered")
                continue
            }
            switch plan.kind {
            case .follow:
                guard let shot = EditTimeline.shot(plan, at: t) else { continue }
                let got = SyntheticRecording.read(image, picture: bounds)
                if got.color != shot.source || got.frame != Int(shot.filePositionMs / 100) {
                    out.append("\(t - plan.startMs) ms: camera \(got.color.map(String.init) ?? "none") frame \(got.frame), expected \(shot.source) frame \(shot.filePositionMs / 100)")
                }
            case .grid:
                let rects = EditComposition.cellRects(plan, in: bounds)
                for i in 0..<4 {
                    let cell = i < rects.count ? rects[i] : CGRect(x: width / 2, y: height / 2, width: width / 2, height: height / 2)
                    if i < plan.cells.count, let p = EditTimeline.cellPosition(plan, cell: i, at: t) {
                        let picture = PictureFit.fit(picture: CGSize(width: 640, height: 360), in: cell, fill: false).destination
                        let got = SyntheticRecording.read(image, picture: picture)
                        if got.color != plan.cells[i].source || got.frame != Int(p / 100) {
                            out.append("\(t - plan.startMs) ms cell \(i): camera \(got.color.map(String.init) ?? "none") frame \(got.frame), expected \(plan.cells[i].source) frame \(p / 100)")
                        }
                    } else {
                        let c = image.rgb(x: Int(cell.midX), y: Int(cell.midY))
                        if max(c.x, c.y, c.z) > 40 { out.append("\(t - plan.startMs) ms cell \(i): \(c), expected black") }
                    }
                }
            }
        }
        return out
    }

    func editFollow(_ b: RecordingsBrowser, _ fake: FakeCore, _ rec: RecordingsModel, _ incident: Incident, folder: String?) {
        // parity: edit-follow-cut
        let ep = b.editPlayer
        let clock = ManualClock()
        ep.now = { clock.now }
        b.show(view: .follow)
        // The device playback's pause arrives as a core event, so it is
        // waited for too (the edit can draw its first frame before it).
        when({ ep.session != nil && (b.editStage?.framesDrawn ?? 0) > 0 && !b.player.transport.isPlaying }, timeout: 5) { [self] in
            guard let session = ep.session, let plan = ep.plan, let renderer = try? GridRenderer() else {
                problems.append("edits: the follow cut did not open (\(ep.message ?? b.message ?? "no message"))")
                return editBack(b, fake, folder: folder)
            }
            let s = incident.start * 1000
            print("edits: follow cut \(plan.segments.map { "\($0.source)@\($0.startMs - s)" }), cuts \(ep.cuts.map { $0 - s }), frames drawn \(b.editStage?.framesDrawn ?? 0)")
            if b.playerView != .follow { problems.append("edits: the follow cut is not shown") }
            if !session.failures.isEmpty { problems.append("edits: files not opened: \(session.failures)") }
            if ep.cuts != [s + 1000, s + 2000] || plan.segments.map(\.source) != [0, 1, 2] {
                problems.append("edits: the follow cut cuts at \(ep.cuts.map { $0 - s }) to \(plan.segments.map(\.source))")
            }
            let n: Int64 = 24
            let samples = (0..<n).map { plan.startMs + $0 * (plan.endMs - plan.startMs) / (n - 1) }
                + [s + 500, s + 999, s + 1000, s + 1500, s + 1999, s + 2000, s + 2100, s + 700]
            let bad = editMismatches(session, renderer, samples: samples, width: 320, height: 180)
            print("edits: follow cut rendered \(samples.count) frames, \(bad.count) wrong")
            if !bad.isEmpty { problems.append("edits: follow cut pictures: \(bad.prefix(4).joined(separator: "; "))") }
            // The transport on the simulated clock, and the sound following the picture.
            ep.pause()
            ep.setRate(1)
            ep.seek(to: s + 500)
            ep.play()
            var heard: [Int?] = [ep.playingSoundSource]
            clock.now += 1
            ep.tick()
            heard.append(ep.playingSoundSource)
            clock.now += 1
            ep.tick()
            heard.append(ep.playingSoundSource)
            let position = ep.positionMs
            ep.setRate(4)
            heard.append(ep.playingSoundSource)
            ep.setRate(1)
            ep.muted = true
            heard.append(ep.playingSoundSource)
            ep.muted = false
            ep.pause()
            print("edits: played 2 s on the simulated clock to \(position - s) ms; sound from \(heard)")
            if position != s + 2500 { problems.append("edits: 2 s of play moved to \(position - s) ms, expected 2500") }
            if heard != [0, 1, 2, nil, nil] { problems.append("edits: the sound came from \(heard), expected [0, 1, 2, nil, nil]") }
            // On screen: the third camera's picture at 2.5 s.
            when({ session.files[2]?.shownPositionMs == 5500 }, timeout: 3) { [self] in
                var seen: SIMD3<Int>?
                b.editStage?.grid?.captureNextFrame { image in seen = image.rgb(x: image.width / 2, y: image.height / 2) }
                when({ seen != nil }, timeout: 2) { [self] in
                    let want = SyntheticRecording.palette[2]
                    print("edits: on screen \(seen.map { "\($0)" } ?? "nothing"), expected \(want)")
                    if let c = seen, abs(c.x - want.x) > 40 || abs(c.y - want.y) > 40 || abs(c.z - want.z) > 40 {
                        problems.append("edits: the screen shows \(c) at 2.5 s, expected the third camera's \(want)")
                    } else if seen == nil {
                        problems.append("edits: the screen drew no frame")
                    }
                    editGrid(b, fake, folder: folder)
                }
            }
        }
    }

    func editGrid(_ b: RecordingsBrowser, _ fake: FakeCore, folder: String?) {
        // parity: edit-grid
        let ep = b.editPlayer
        // The core's pause event arrives some moments after the request, so this waits until here.
        if b.player.transport.isPlaying { problems.append("edits: the device playback still plays under the follow cut") }
        b.show(view: .grid4)
        when({ ep.session != nil && ep.plan?.kind == .grid }, timeout: 5) { [self] in
            guard let session = ep.session, let plan = ep.plan, let renderer = try? GridRenderer() else {
                problems.append("edits: the grid did not open (\(ep.message ?? "no message"))")
                return editBack(b, fake, folder: folder)
            }
            print("edits: grid \(plan.layout ?? "?") of \(plan.cells.count) cells on \(plan.canvas.map { "\($0.width)×\($0.height)" } ?? "no canvas")")
            if plan.layout != "4up" || plan.cells.count != 3 || plan.canvas != .init(width: 1920, height: 1080) {
                problems.append("edits: the 4-up grid is \(plan.layout ?? "none") with \(plan.cells.count) cells")
            }
            let n: Int64 = 16
            let samples = (0..<n).map { plan.startMs + $0 * (plan.endMs - plan.startMs) / (n - 1) } + [plan.startMs + 500, plan.startMs + 1500]
            let bad = editMismatches(session, renderer, samples: samples, width: 640, height: 360)
            print("edits: grid rendered \(samples.count) frames, \(bad.count) wrong")
            if !bad.isEmpty { problems.append("edits: grid pictures: \(bad.prefix(4).joined(separator: "; "))") }
            ep.seek(to: plan.startMs + 6000)
            ep.play()
            let first = ep.playingSoundSource
            ep.gridSound = 1
            let second = ep.playingSoundSource
            ep.pause()
            ep.gridSound = 0
            if first != 0 || second != 1 { problems.append("edits: the grid's sound came from \(String(describing: first)), then \(String(describing: second))") }
            exportTrim(b, fake, folder: folder)
        }
    }

    // MARK: Export

    /// The files an export step writes and the switches it restores.
    final class ExportRun {
        let dir: URL
        let shareSafe: Bool
        let folder: ExportFolder
        init(shareSafe: Bool, folder: ExportFolder) {
            dir = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-export-selftest-\(UUID().uuidString)")
            self.shareSafe = shareSafe
            self.folder = folder
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        }
    }

    final class Box<T>: @unchecked Sendable {
        var value: T?
    }

    /// Runs async work and continues on the main actor when it is done.
    func awaiting<T: Sendable>(_ work: @escaping @Sendable () async -> T, then next: @escaping @MainActor (T) -> Void) {
        let box = Box<T>()
        Task { box.value = await work() }
        when({ box.value != nil }, timeout: 60) { next(box.value!) }
    }

    /// What a picture of a synthetic export at `ms` shows.
    private nonisolated static func lookFile(_ url: URL, at ms: Int64) async -> (color: Int?, frame: Int)? {
        guard let file = try? await RecordedFileSource.open(url) else { return nil }
        defer { file.close() }
        file.show(ms)
        guard let picture = file.latestPicture(),
            let image = await PictureRenderer().render(
                PictureRender(
                    picture: picture, color: file.info.color, source: CGRect(x: 0, y: 0, width: picture.width, height: picture.height),
                    width: 320, height: 180))
        else { return nil }
        return SyntheticRecording.read(image, picture: CGRect(x: 0, y: 0, width: 320, height: 180))
    }

    func exportTrim(_ b: RecordingsBrowser, _ fake: FakeCore, folder: String?) {
        // parity: export-trim
        guard let plan = b.editPlayer.plan, let source = plan.sources.first else { return editBack(b, fake, folder: folder) }
        let suite = UserDefaults(suiteName: "nvrview.selftest.export") ?? .standard
        suite.removePersistentDomain(forName: "nvrview.selftest.export")
        let run = ExportRun(shareSafe: b.export.shareSafe, folder: b.export.folder)
        b.export.folder = ExportFolder(defaults: suite)
        b.export.saveURL = { name, _, _ in run.dir.appendingPathComponent(name) }
        b.export.chooseFolder = { _, _ in run.dir }
        b.export.shareSafe = false
        b.export.trimExact = false
        let s = plan.startMs
        b.export.points = InOutPoints(inMs: s + 2_500, outMs: s + 5_000)
        b.trim(cameras: [source.camera])
        when({ b.export.job?.running == false }, timeout: 60) { [self] in
            guard let job = b.export.job, job.files.count == 1, let url = job.files.first else {
                problems.append("export: the trim wrote \(b.export.job?.files.count ?? 0) files (\(b.export.job?.result ?? b.message ?? "no message"))")
                return exportFinish(b, fake, run, folder: folder)
            }
            // The core's own plan for the same trim says where it starts and ends.
            let request = ExportRequest.trim(
                download: plan.download, cameras: [source.camera], inMs: s + 2_500, outMs: s + 5_000, exact: false, options: .init())
            guard let trim = (try? fake.editCompute(json: request)).flatMap(TrimPlan.parse), let out = trim.outputs.first else {
                problems.append("export: the fake planned no trim")
                return exportFinish(b, fake, run, folder: folder)
            }
            awaiting({ () async -> (Double, Int, Int, (color: Int?, frame: Int)?)? in
                let asset = AVURLAsset(url: url)
                guard let d = try? await asset.load(.duration).seconds,
                    let track = try? await asset.loadTracks(withMediaType: .video).first,
                    let size = try? await track.load(.naturalSize)
                else { return nil }
                return (d, Int(size.width), Int(size.height), await Self.lookFile(url, at: 0))
            }) { [self] got in
                let want = Double(out.sourceOutMs - out.sourceInMs) / 1000
                print("export: trim \(url.lastPathComponent) \(got.map { "\($0.0) s, \($0.1)×\($0.2), first \(String(describing: $0.3))" } ?? "unreadable"), expected \(want) s from file ms \(out.sourceInMs)")
                if let g = got {
                    if abs(g.0 - want) > 0.25 { problems.append("export: the trim lasts \(g.0) s, expected \(want)") }
                    if g.1 != 640 || g.2 != 360 { problems.append("export: the trim is \(g.1)×\(g.2), expected 640×360") }
                    let expectedSource = plan.sources.first { $0.file == out.file }?.source
                    if g.3?.color != expectedSource || g.3?.frame != Int(out.sourceInMs / 100) {
                        problems.append("export: the trim starts at \(String(describing: g.3)), expected camera \(String(describing: expectedSource)) frame \(out.sourceInMs / 100)")
                    }
                } else {
                    problems.append("export: the trimmed file could not be read")
                }
                if !out.copyPossible { problems.append("export: a keyframe trim should be a copy") }
                exportShareSafe(b, fake, run, plan: plan, source: source, folder: folder)
            }
        }
    }

    func exportShareSafe(
        _ b: RecordingsBrowser, _ fake: FakeCore, _ run: ExportRun, plan: EditPlan, source: EditPlan.Source, folder: String?
    ) {
        // parity: export-share-safe
        b.export.shareSafe = true
        b.export.dismissJob()
        let s = plan.startMs
        b.export.points = InOutPoints(inMs: s + 2_500, outMs: s + 5_000)
        b.trim(cameras: [source.camera])
        when({ b.export.job?.running == false }, timeout: 60) { [self] in
            guard let url = b.export.job?.files.first else {
                problems.append("export: the share-safe trim wrote nothing (\(b.export.job?.result ?? "no message"))")
                return exportFinish(b, fake, run, folder: folder)
            }
            let bytes = (try? Data(contentsOf: url)) ?? Data()
            let leaks = ["Core Media", "udta", "meta"].filter { bytes.range(of: Data($0.utf8)) != nil }
            print("export: share-safe trim \(url.lastPathComponent), \(bytes.count) bytes, leaks \(leaks)")
            if bytes.isEmpty || !leaks.isEmpty { problems.append("export: the share-safe trim holds \(leaks)") }
            if url.lastPathComponent.contains("Camera 1") || url.lastPathComponent.contains("Camera 2") || url.lastPathComponent.contains("Camera 3") {
                problems.append("export: the share-safe file is named \(url.lastPathComponent)")
            }
            b.export.shareSafe = false
            b.export.dismissJob()
            exportFollow(b, fake, run, folder: folder)
        }
    }

    func exportFollow(_ b: RecordingsBrowser, _ fake: FakeCore, _ run: ExportRun, folder: String?) {
        // parity: export-follow-cut
        guard let plan = b.editPlayer.plan, let incident = b.editable?.incident else { return exportFinish(b, fake, run, folder: folder) }
        let s = incident.start * 1000
        b.export.points = InOutPoints(inMs: s + 500, outMs: s + 2_500)
        b.export.codec = .hevc
        b.exportEdit(.follow)
        when({ b.export.job?.running == false }, timeout: 90) { [self] in
            guard let url = b.export.job?.files.first else {
                problems.append("export: the follow cut wrote nothing (\(b.export.job?.result ?? b.message ?? "no message"))")
                return exportFinish(b, fake, run, folder: folder)
            }
            let request = ExportRequest.edit(grid: nil, download: plan.download, window: (s + 500, s + 2_500), options: .init())
            guard let cut = (try? fake.editCompute(json: request)).flatMap(EditPlan.parse) else {
                problems.append("export: the fake planned no follow cut")
                return exportFinish(b, fake, run, folder: folder)
            }
            let samples: [Int64] = [0, 400, 600, 1_100, 1_400, 1_900]
            awaiting({ () async -> (Double, Int, Int, [(color: Int?, frame: Int)?])? in
                let asset = AVURLAsset(url: url)
                guard let d = try? await asset.load(.duration).seconds,
                    let track = try? await asset.loadTracks(withMediaType: .video).first,
                    let size = try? await track.load(.naturalSize)
                else { return nil }
                var looks: [(color: Int?, frame: Int)?] = []
                for t in samples { looks.append(await Self.lookFile(url, at: t)) }
                return (d, Int(size.width), Int(size.height), looks)
            }) { [self] got in
                guard let g = got else {
                    problems.append("export: the follow cut file could not be read")
                    return exportFinish(b, fake, run, folder: folder)
                }
                let length = Double(cut.endMs - cut.startMs) / 1000
                print("export: follow cut \(url.lastPathComponent) \(g.0) s of \(length), \(g.1)×\(g.2), cuts \(EditTimeline.cuts(cut).map { $0 - s })")
                if abs(g.0 - length) > 0.25 { problems.append("export: the follow cut lasts \(g.0) s, expected \(length)") }
                if g.1 != 640 || g.2 != 360 { problems.append("export: the follow cut is \(g.1)×\(g.2), expected 640×360") }
                for (t, look) in zip(samples, g.3) {
                    guard let shot = EditTimeline.shot(cut, at: cut.startMs + t) else { continue }
                    let frame = Int(shot.filePositionMs / 100)
                    if look?.color != shot.source || abs((look?.frame ?? -9) - frame) > 1 {
                        problems.append("export: the follow cut at \(t) ms shows \(String(describing: look)), expected camera \(shot.source) frame \(frame)")
                    }
                }
                b.export.dismissJob()
                exportGrid(b, fake, run, folder: folder)
            }
        }
    }

    func exportGrid(_ b: RecordingsBrowser, _ fake: FakeCore, _ run: ExportRun, folder: String?) {
        // parity: export-grid
        guard let plan = b.editPlayer.plan, let incident = b.editable?.incident else { return exportFinish(b, fake, run, folder: folder) }
        let s = incident.start * 1000
        b.export.points = InOutPoints(inMs: s, outMs: s + 2_000)
        b.export.codec = .h264
        b.exportEdit(.grid4)
        when({ b.export.job?.running == false }, timeout: 90) { [self] in
            guard let url = b.export.job?.files.first else {
                problems.append("export: the grid wrote nothing (\(b.export.job?.result ?? b.message ?? "no message"))")
                return exportFinish(b, fake, run, folder: folder)
            }
            let request = ExportRequest.edit(grid: "4up", download: plan.download, window: (s, s + 2_000), options: .init())
            guard let grid = (try? fake.editCompute(json: request)).flatMap(EditPlan.parse) else {
                problems.append("export: the fake planned no grid")
                return exportFinish(b, fake, run, folder: folder)
            }
            let samples: [Int64] = [200, 1_000, 1_800]
            let bounds = CGRect(x: 0, y: 0, width: 1920, height: 1080)
            awaiting({ () async -> (Double, Int, Int, [[(color: Int?, frame: Int)]])? in
                let asset = AVURLAsset(url: url)
                guard let d = try? await asset.load(.duration).seconds,
                    let track = try? await asset.loadTracks(withMediaType: .video).first,
                    let size = try? await track.load(.naturalSize),
                    let file = try? await RecordedFileSource.open(url)
                else { return nil }
                defer { file.close() }
                var all: [[(color: Int?, frame: Int)]] = []
                for t in samples {
                    file.show(t)
                    guard let picture = file.latestPicture(),
                        let image = await PictureRenderer().render(
                            PictureRender(
                                picture: picture, color: file.info.color,
                                source: CGRect(x: 0, y: 0, width: picture.width, height: picture.height), width: 1920, height: 1080))
                    else { return nil }
                    all.append(
                        EditComposition.cellRects(grid, in: bounds).map { cell in
                            let fit = PictureFit.fit(picture: CGSize(width: 640, height: 360), in: cell, fill: false).destination
                            return SyntheticRecording.read(image, picture: fit)
                        })
                }
                return (d, Int(size.width), Int(size.height), all)
            }) { [self] got in
                guard let g = got else {
                    problems.append("export: the grid file could not be read")
                    return exportFinish(b, fake, run, folder: folder)
                }
                print("export: grid \(url.lastPathComponent) \(g.0) s, \(g.1)×\(g.2), \(grid.cells.count) cells")
                if abs(g.0 - 2.0) > 0.25 { problems.append("export: the grid lasts \(g.0) s, expected 2") }
                if g.1 != 1920 || g.2 != 1080 { problems.append("export: the grid is \(g.1)×\(g.2), expected 1920×1080") }
                for (t, cells) in zip(samples, g.3) {
                    for (i, look) in cells.enumerated() where i < grid.cells.count {
                        guard let p = EditTimeline.cellPosition(grid, cell: i, at: s + t) else { continue }
                        if look.color != grid.cells[i].source || abs(look.frame - Int(p / 100)) > 1 {
                            problems.append("export: the grid at \(t) ms cell \(i) shows camera \(String(describing: look.color)) frame \(look.frame), expected \(grid.cells[i].source) frame \(p / 100)")
                        }
                    }
                }
                b.export.dismissJob()
                exportRange(b, fake, run, folder: folder)
            }
        }
    }

    func exportRange(_ b: RecordingsBrowser, _ fake: FakeCore, _ run: ExportRun, folder: String?) {
        // parity: export-range-download
        guard let plan = b.editPlayer.plan, let rec = Optional(b.model) else { return exportFinish(b, fake, run, folder: folder) }
        let s = plan.startMs
        b.export.points = InOutPoints(inMs: s + 1_000, outMs: s + 4_200)
        b.beginRangeDownload()
        guard var request = b.export.rangeRequest else {
            problems.append("export: the range sheet did not open")
            return exportFinish(b, fake, run, folder: folder)
        }
        if request.cameras.count != plan.sources.count { problems.append("export: the range sheet offers \(request.cameras.count) cameras, expected \(plan.sources.count)") }
        request.cameras = [plan.sources[0].camera, plan.sources[1].camera]
        b.downloadRange(request)
        let wantStart = Int64((Double(s + 1_000) / 1000).rounded(.down)), wantEnd = Int64((Double(s + 4_200) / 1000).rounded(.up))
        when({
            fake.completeRecordingWork()
            return rec.downloads.contains { $0.range != nil && $0.state == "done" }
        }, timeout: 5) { [self] in
            let d = rec.downloads.last { $0.range != nil }
            print("export: range download \(d?.rangeText ?? "none"), \(d?.cameras.count ?? 0) cameras, \(d?.state ?? "?")")
            if d?.range != DownloadRange(start: wantStart, end: wantEnd) || d?.cameras.count != 2 || d?.state != "done" {
                problems.append("export: the range download is \(String(describing: d?.range)) with \(d?.cameras.count ?? 0) cameras (\(d?.state ?? "none"), \(rec.notice ?? "")), expected \(wantStart)–\(wantEnd) with 2")
            }
            if b.center != .downloads { problems.append("export: a range download did not show the downloads") }
            b.center = .player
            exportStills(b, fake, run, folder: folder)
        }
    }

    func exportStills(_ b: RecordingsBrowser, _ fake: FakeCore, _ run: ExportRun, folder: String?) {
        // parity: export-stills
        guard let plan = b.editPlayer.plan else { return exportFinish(b, fake, run, folder: folder) }
        b.editPlayer.pause()
        b.editPlayer.seek(to: plan.startMs + 2_500)
        b.saveStills(eachCamera: false)
        when({ b.export.job?.running == false }, timeout: 30) { [self] in
            let one = b.export.job?.files ?? []
            print("export: current picture \(one.map(\.lastPathComponent))")
            if one.count != 1 {
                problems.append("export: the current picture wrote \(one.count) files (\(b.export.job?.result ?? "no message"))")
            } else if let data = try? Data(contentsOf: one[0]), let image = PictureEncoder.decode(data) {
                let size = ExportGeometry.outputSize(plan)
                // The grid's four cells: the first camera's color in its cell.
                let cell = EditComposition.cellRects(plan, in: CGRect(x: 0, y: 0, width: image.width, height: image.height)).first ?? .zero
                let got = SyntheticRecording.read(image, picture: PictureFit.fit(picture: CGSize(width: 640, height: 360), in: cell, fill: false).destination)
                if image.width != size.width || image.height != size.height { problems.append("export: the picture is \(image.width)×\(image.height), expected \(size)") }
                if got.color != plan.cells.first?.source { problems.append("export: the picture's first cell shows camera \(String(describing: got.color))") }
            } else {
                problems.append("export: the picture could not be read back")
            }
            b.export.dismissJob()
            b.saveStills(eachCamera: true)
            when({ b.export.job?.running == false }, timeout: 30) { [self] in
                let many = b.export.job?.files ?? []
                print("export: a picture per camera \(many.map(\.lastPathComponent))")
                if many.count != plan.sources.count || Set(many.map(\.lastPathComponent)).count != many.count {
                    problems.append("export: a picture per camera wrote \(many.count) files, expected \(plan.sources.count) (\(b.export.job?.result ?? "no message"))")
                }
                for url in many {
                    if let data = try? Data(contentsOf: url), let image = PictureEncoder.decode(data) {
                        if image.width != 640 || image.height != 360 { problems.append("export: \(url.lastPathComponent) is \(image.width)×\(image.height)") }
                    } else {
                        problems.append("export: \(url.lastPathComponent) could not be read back")
                    }
                }
                b.export.dismissJob()
                exportFinish(b, fake, run, folder: folder)
            }
        }
    }

    func exportFinish(_ b: RecordingsBrowser, _ fake: FakeCore, _ run: ExportRun, folder: String?) {
        b.export.points.clear()
        b.export.shareSafe = run.shareSafe
        b.export.folder = run.folder
        b.export.saveURL = ExportPanels.save
        b.export.chooseFolder = ExportPanels.folder
        try? FileManager.default.removeItem(at: run.dir)
        b.export.dismissJob()
        editBack(b, fake, folder: folder)
    }

    /// Back to the cameras: the edit closes, the device playback continues from its position.
    func editBack(_ b: RecordingsBrowser, _ fake: FakeCore, folder: String?) {
        // parity: edit-back-to-cameras
        let at = b.editPlayer.positionMs
        b.show(view: .cameras)
        print("edits: back to the cameras at \(TransportState.clock(b.player.positionMs))")
        if b.playerView != .cameras || b.editPlayer.session != nil { problems.append("edits: the edit did not close") }
        if b.player.playback != nil, b.player.positionMs != at {
            problems.append("edits: the cameras continue at \(b.player.positionMs), not the edit's \(at)")
        }
        b.editPlayer.now = { CACurrentMediaTime() }
        if let folder { try? FileManager.default.removeItem(at: URL(fileURLWithPath: folder)) }
        // The edit's download is forgotten, so the analysis steps start
        // with nothing downloaded for the day.
        let rec = b.model
        for d in rec.downloads { rec.forget(d.download) }
        when({ rec.downloads.isEmpty }, timeout: 2) { [self] in
            if !rec.downloads.isEmpty { problems.append("edits: the edit's download was not forgotten") }
            recordingsLayout(b, fake)
        }
    }

    /// The window at its smallest and at a large size: the side panes give
    /// way in a narrow window (inspector first) and come back in a wide
    /// one, and the window keeps the size it was given. With
    /// NVRVIEW_RECORDINGS_SNAPSHOTS=<folder>, writes what it looked like.
    func recordingsLayout(_ b: RecordingsBrowser, _ fake: FakeCore) {
        // parity: recordings-layout
        guard let window = app.recordingsWindows.first?.window else { return recordingsClose(b, fake) }
        let saved = window.frame
        let sizes = [RecordingsPanes.windowMin, CGSize(width: 900, height: 640), CGSize(width: 1400, height: 900)]
        var results: [String] = []
        func next(_ i: Int) {
            guard i < sizes.count else {
                window.setFrame(saved, display: false)
                print("recordings: layout \(results.joined(separator: "; "))")
                return recordingsWheelZoom(b, fake)
            }
            let size = sizes[i]
            let want = RecordingsPanes.shown(width: size.width, sidebar: true, inspector: true)
            window.setContentSize(size)
            // The panes follow the width SwiftUI lays out (on its next pass).
            when({ b.shownPanes?.width == size.width }, timeout: 2) { [self] in
                let got = window.contentLayoutRect.size
                let panes = b.shownPanes
                results.append(
                    "\(Int(size.width))×\(Int(size.height)): \(Int(got.width))×\(Int(got.height)), sidebar \(panes?.sidebar == true), inspector \(panes?.inspector == true)")
                if abs(got.width - size.width) > 1 || abs(got.height - size.height) > 1 {
                    problems.append("recordings: the window is \(got) when given \(size)")
                }
                if panes?.width != size.width || panes?.sidebar != want.sidebar || panes?.inspector != want.inspector {
                    problems.append("recordings: at \(size) the panes are \(String(describing: panes)), expected \(want)")
                }
                if let dir = ProcessInfo.processInfo.environment["NVRVIEW_RECORDINGS_SNAPSHOTS"] {
                    // The window as on screen (pictures included); needs the
                    // screen-recording permission, so only on request.
                    let path = URL(fileURLWithPath: dir).appendingPathComponent("recordings-\(Int(size.width))x\(Int(size.height)).png").path
                    let shot = Process()
                    shot.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
                    shot.arguments = ["-x", "-o", "-l", String(window.windowNumber), path]
                    try? shot.run()
                    shot.waitUntilExit()
                }
                next(i + 1)
            }
        }
        next(0)
    }

    /// The timeline lanes' wheel and trackpad, through the lanes' own event
    /// catcher with real scroll events: a mouse wheel notch zooms around the
    /// pointer (its time stays under it), many clamp at 48x, a horizontal
    /// two-finger scroll pans, a vertical one is left to the lanes'
    /// scrolling, and notches the other way go back to the whole day.
    func recordingsWheelZoom(_ b: RecordingsBrowser, _ fake: FakeCore) {
        // parity: recordings-timeline-wheel-zoom
        func catcher(in view: NSView?) -> TimelineInputCatcher.CatcherView? {
            guard let view else { return nil }
            if let c = view as? TimelineInputCatcher.CatcherView { return c }
            for v in view.subviews { if let c = catcher(in: v) { return c } }
            return nil
        }
        let window = app.recordingsWindows.first?.window
        // The lanes are laid out again after the layout step's last resize.
        when({ catcher(in: window?.contentView).map { !$0.visibleRect.isEmpty } == true }, timeout: 2) { [self] in
            guard let c = catcher(in: window?.contentView), !c.visibleRect.isEmpty else {
                problems.append("recordings: no timeline lanes to scroll")
                return recordingsClose(b, fake)
            }
            recordingsWheelZoom(b, c)
            recordingsListen(b, fake)
        }
    }

    /// Listening for sounds: a camera's switch reaches `nv_sound_watch` and
    /// its `sound_watch` state comes back; a fifth camera of one device is
    /// refused; switching off takes it off the core's list.
    func recordingsListen(_ b: RecordingsBrowser, _ fake: FakeCore) {
        // parity: recordings-listen-for-sounds
        let rec = b.model
        guard let device = b.cameras.first?.device else { return recordingsClose(b, fake) }
        let cams = (0..<5).map { RecordingCamera(device: device, channel: $0) }
        for c in cams.prefix(4) where !rec.setListening(c, true) {
            problems.append("recordings: listening to \(c) was refused (\(rec.notice ?? ""))")
        }
        if rec.setListening(cams[4], true) { problems.append("recordings: a fifth camera of a device listens") }
        if fake.soundWatched != Array(cams.prefix(4)) { problems.append("recordings: the core listens to \(fake.soundWatched)") }
        when({ rec.listenStates[cams[0]]?.state == "watching" }, timeout: 2) { [self] in
            print("recordings: listening to \(rec.listeningCameras.count) cameras, the fifth refused: \(rec.notice ?? "")")
            if rec.listenStates[cams[0]]?.state != "watching" { problems.append("recordings: no sound_watch state for a listening camera") }
            for c in cams.dropFirst() { rec.setListening(c, false) }
            if fake.soundWatched != [cams[0]] { problems.append("recordings: switching off left \(fake.soundWatched) listening") }
            recordingsLoudness(b, fake, listening: cams[0])
        }
    }

    /// The loudness strips: a listening camera's lane has loudness (the fake
    /// answers a steady level), the others are blank; zooming in asks again
    /// with a finer bucket.
    func recordingsLoudness(_ b: RecordingsBrowser, _ fake: FakeCore, listening: RecordingCamera) {
        // parity: recordings-loudness-lanes
        b.timeline = TimelineViewport()
        b.reloadLoudness(force: true)
        let whole = b.loudnessBucket
        let heard = b.lanes.filter { b.loudness(of: $0.camera)?.heard == true }.map(\.camera)
        print("recordings: loudness at \(whole) s buckets, heard on \(heard)")
        if heard != [listening] { problems.append("recordings: loudness heard on \(heard), expected \(listening)") }
        if b.loudness(of: listening)?.bucket != whole { problems.append("recordings: the loudness lane is not at the bucket asked") }
        let reads = b.loudnessReads
        b.timeline = TimelineViewport(zoom: 48)
        if b.loudnessReads != reads + 1 || b.loudness(of: listening)?.bucket != b.loudnessBucket || b.loudnessBucket >= whole {
            problems.append("recordings: zooming in did not ask for finer loudness (\(b.loudnessBucket) s)")
        }
        b.timeline = TimelineViewport()
        recordingsSoundLabels(b, fake, camera: listening)
    }

    /// Sound labels (as the app's classifier would put them): a label becomes
    /// an event from sound on its camera's lane and an incident of its own,
    /// with the sound badge, which the Sounds filter alone shows.
    func recordingsSoundLabels(_ b: RecordingsBrowser, _ fake: FakeCore, camera: RecordingCamera) {
        // parity: recordings-sound-labels
        let at = b.day + 10 * 3600
        let json = #"{"camera":\#(camera.json),"analyzer":"selftest-v1","start":\#(at - 60),"end":\#(at + 60),"labels":[{"kind":"dog_bark","start":\#(at),"end":\#(at + 4),"confidence":0.9,"label":"dog_bark"}]}"#
        do throws(CoreStatus) {
            try fake.soundLabelsPut(json: json)
        } catch {
            problems.append("recordings: the sound labels were refused (status \(error.code))")
            return recordingsClose(b, fake)
        }
        when({ b.dayIncidents.contains { $0.kinds == ["dog_bark"] } }, timeout: 2) { [self] in
            let lane = b.lanes.first { $0.camera == camera }
            let onLane = lane?.events.contains { $0.kind == "dog_bark" && $0.source == SoundKinds.localAudio } == true
            b.kindFilters = [.sound]
            let shown = b.incidents.map(\.kinds)
            let badge = b.incidents.first.map { SoundKinds.names($0.events) } ?? []
            print("recordings: a sound label on the lane \(onLane), the Sounds filter shows \(shown), badge \(badge)")
            if !onLane { problems.append("recordings: the sound label is not on its camera's lane") }
            if shown != [["dog_bark"]] { problems.append("recordings: the Sounds filter shows \(shown)") }
            if badge != ["Dog barking"] { problems.append("recordings: the sound badge says \(badge)") }
            b.kindFilters = []
            b.model.setListening(camera, false)
            recordingsAnalyzeNoAudio(b, fake)
        }
    }

    /// Analyze Sounds with nothing downloaded for the day: the analysis
    /// fails `no_audio` with its message, the lane shows no progress and no
    /// loudness.
    func recordingsAnalyzeNoAudio(_ b: RecordingsBrowser, _ fake: FakeCore) {
        // parity: recordings-analyze-no-audio
        let rec = b.model
        guard let camera = b.lanes.first?.camera else { return recordingsClose(b, fake) }
        if !rec.downloads.isEmpty { problems.append("recordings: \(rec.downloads.count) downloads before the analysis steps") }
        b.analyzeSound(camera)
        if b.analysis(of: camera)?.state != .queued { problems.append("recordings: Analyze Sounds queued nothing on the lane") }
        when({ fake.completeRecordingWork(); return rec.analysis(camera, day: b.day)?.state == .failed }, timeout: 2) { [self] in
            let message = SoundAnalysis.failure(reason: "no_audio")
            print("recordings: analyze with nothing downloaded: \(rec.analysis(camera, day: b.day)?.reason ?? "no result"), \(rec.notice ?? "no message")")
            if rec.analysis(camera, day: b.day)?.reason != "no_audio" { problems.append("recordings: the analysis did not fail no_audio") }
            if rec.notice != message { problems.append("recordings: the no_audio message was \(rec.notice ?? "missing")") }
            if b.analysis(of: camera) != nil { problems.append("recordings: a failed analysis still shows progress") }
            b.reloadLoudness(force: true)
            if b.loudness(of: camera)?.heard == true { problems.append("recordings: loudness appeared without audio") }
            recordingsAnalyzeDay(b, fake)
        }
    }

    /// Analyze Sounds after a download: progress, then loudness and a sound
    /// label on the lane and the done message.
    func recordingsAnalyzeDay(_ b: RecordingsBrowser, _ fake: FakeCore) {
        // parity: recordings-analyze-day
        let rec = b.model
        guard let incident = b.dayIncidents.first(where: { $0.kinds == ["vehicle"] }) ?? b.dayIncidents.first(where: { !$0.cameras.isEmpty }),
            let camera = incident.cameras.first?.camera
        else {
            problems.append("recordings: no incident to download for the analysis")
            return recordingsClose(b, fake)
        }
        b.download(incident.key)
        when({ fake.completeRecordingWork(); return rec.downloads.contains { $0.state == "done" } }, timeout: 2) { [self] in
            b.analyzeSound(camera)
            // The day header's action skips the camera that is already being analyzed.
            if !rec.isAnalyzing(camera, day: b.day) { problems.append("recordings: Analyze Sounds shows no progress") }
            when({ fake.completeRecordingWork(); return rec.analysis(camera, day: b.day)?.state == .done }, timeout: 2) { [self] in
                let entry = rec.analysis(camera, day: b.day)
                print("recordings: analyzed \(entry?.seconds ?? 0) s, \(entry?.labels ?? 0) labels; \(rec.notice ?? "no message")")
                if (entry?.seconds ?? 0) <= 0 || entry?.labels != 1 { problems.append("recordings: the analysis found \(String(describing: entry))") }
                if b.analysis(of: camera) != nil { problems.append("recordings: a finished analysis still shows progress") }
                if rec.notice != entry.flatMap(SoundAnalysis.outcome) { problems.append("recordings: the done message was \(rec.notice ?? "missing")") }
                when({ b.loudness(of: camera)?.heard == true && b.lanes.first { $0.camera == camera }?.events.contains { $0.kind == "smoke_alarm" && $0.source == SoundKinds.localAudio } == true }, timeout: 2) { [self] in
                    if b.loudness(of: camera)?.heard != true { problems.append("recordings: no loudness after the analysis") }
                    let onLane = b.lanes.first { $0.camera == camera }?.events.contains { $0.kind == "smoke_alarm" && $0.source == SoundKinds.localAudio } == true
                    if !onLane { problems.append("recordings: no sound label on the lane after the analysis") }
                    recordingsAnalyzeCancel(b, fake, camera: camera)
                }
            }
        }
    }

    /// Canceling an analysis before it runs: it ends canceled, shows no
    /// progress, and does not come back when the core's work is done.
    func recordingsAnalyzeCancel(_ b: RecordingsBrowser, _ fake: FakeCore, camera: RecordingCamera) {
        // parity: recordings-analyze-cancel
        let rec = b.model
        // Another camera, so the finished analysis above does not stand in for this one.
        guard let other = b.lanes.map(\.camera).first(where: { $0 != camera }) else { return recordingsClose(b, fake) }
        b.analyzeSound(other)
        if b.analysis(of: other) == nil { problems.append("recordings: nothing to cancel") }
        b.cancelAnalysis(other)
        when({ rec.analysis(other, day: b.day)?.state == .canceled }, timeout: 2) { [self] in
            fake.completeRecordingWork()
            print("recordings: analysis canceled: \(rec.notice ?? "no message")")
            if rec.analysis(other, day: b.day)?.state != .canceled { problems.append("recordings: the analysis did not end canceled") }
            if b.analysis(of: other) != nil { problems.append("recordings: a canceled analysis still shows progress") }
            if rec.notice != SoundAnalysis.outcome(SoundAnalysis.Entry(request: 0, state: .canceled)) {
                problems.append("recordings: the cancel message was \(rec.notice ?? "missing")")
            }
            if b.loudness(of: other)?.heard == true { problems.append("recordings: a canceled analysis left loudness") }
            for d in rec.downloads { rec.forget(d.download) }
            recordingsClose(b, fake)
        }
    }

    func recordingsWheelZoom(_ b: RecordingsBrowser, _ c: TimelineInputCatcher.CatcherView) {
        func scroll(_ units: CGScrollEventUnit, dy: Int32, dx: Int32 = 0) -> NSEvent? {
            CGEvent(scrollWheelEvent2Source: nil, units: units, wheelCount: 2, wheel1: dy, wheel2: dx, wheel3: 0)
                .flatMap(NSEvent.init(cgEvent:))
        }
        b.timeline = TimelineViewport()
        let at = NSPoint(x: c.bounds.width * 0.25, y: c.visibleRect.midY)
        let before = b.timeline.fraction(atPointer: 0.25)
        guard let notch = scroll(.line, dy: 1), let input = TimelineInput(notch), c.handle(notch, at: at) else {
            problems.append("recordings: a wheel notch over the lanes was not taken")
            return
        }
        let one = b.timeline
        print("recordings: a wheel notch (\(notch.scrollingDeltaY) lines) zooms to \(one.zoom)x, input \(input)")
        if case .zoom(_, true) = input {} else { problems.append("recordings: a wheel notch did not zoom eased (\(input))") }
        if one.zoom <= 1 || abs(one.fraction(atPointer: 0.25) - before) > 1e-9 {
            problems.append("recordings: a wheel notch made \(one), the time under the pointer moved")
        }
        for _ in 0..<40 { if let e = scroll(.line, dy: 3) { _ = c.handle(e, at: at) } }
        if b.timeline.zoom != TimelineViewport.maxZoom { problems.append("recordings: the wheel zoomed to \(b.timeline.zoom)x, not 48x") }
        let zoomed = b.timeline
        if let pan = scroll(.pixel, dy: 0, dx: -40), c.handle(pan, at: at) {
            if b.timeline.start <= zoomed.start { problems.append("recordings: a two-finger scroll left did not pan later") }
        } else {
            problems.append("recordings: a horizontal two-finger scroll was not taken")
        }
        if let vertical = scroll(.pixel, dy: -40), c.handle(vertical, at: at) {
            problems.append("recordings: a vertical two-finger scroll was taken from the lanes' scrolling")
        }
        for _ in 0..<40 { if let e = scroll(.line, dy: -3) { _ = c.handle(e, at: at) } }
        print("recordings: wheel zoom to 48x panned from \(zoomed.start) and back to \(b.timeline)")
        if b.timeline != TimelineViewport() { problems.append("recordings: wheeling back left \(b.timeline)") }
    }

    func recordingsClose(_ b: RecordingsBrowser, _ fake: FakeCore) {
        // parity: recordings-window-close
        let hold = b.holdID
        app.recordingsWindows.first?.window?.close()
        if fake.recordingsHolds[hold] != nil { problems.append("recordings: closing the window kept its hold") }
        if !app.recordingsWindows.isEmpty { problems.append("recordings: the window is still listed") }
        // parity: playback-close
        if !fake.openPlaybacks.isEmpty || b.player.playback != nil {
            problems.append("recordings: closing the window left playbacks \(fake.openPlaybacks) open")
        }
        checkLog()
    }

    /// The recordings window on the real core, against a device with
    /// recordings (tools/mock-nvr `--recordings 3`; never a real NVR in a
    /// self-test): every camera of the first device chosen, today asked for
    /// and held, a lane per camera with spans and the day's incidents from
    /// the device's search, a cover keyframe decoded with VideoToolbox and
    /// put back, yesterday asked for and not held, a lock that downloads an
    /// incident to the end, the Trash hand-over (recorded, not done), and the
    /// hold released when the window closes. Skipped for a device without
    /// recordings.
    func recordingsRealStep() {
        // Only against a device on this computer (the mock): locking
        // downloads, and a self-test pointed at a real NVR must not.
        guard let model = app.model, let rec = model.recordings, rec.available,
            let device = model.devices.first(where: { $0.link.state == .connected && !$0.channels.isEmpty }),
            ["127.0.0.1", "localhost", "::1"].contains(device.config.host)
        else { return checkLog() }
        // parity: recordings-real-core
        let calls = SystemCalls()
        rec.trasher = { calls.trashed.append($0) }
        rec.revealer = { calls.revealed.append($0) }
        let cams = device.channels.map { RecordingCamera(device: device.key, channel: $0.channel) }
        rec.setEnabled(device: device.key, cameras: cams)
        guard let controller = app.openRecordingsWindow() else {
            problems.append("recordings: no window opened")
            return checkLog()
        }
        let b = controller.browser
        let ready = { b.lanes.count == cams.count && b.lanes.allSatisfy { !$0.spans.isEmpty } && !b.dayIncidents.isEmpty }
        when({ ready() && !rec.searching.busy }, timeout: 30) { [self] in
            print("recordings (real core): \(b.lanes.map(\.spans.count)) spans, \(b.dayIncidents.count) incidents, holding \(b.holding)")
            guard ready() else {
                problems.append("recordings: the device's day did not arrive (\(b.lanes.map(\.spans.count)) spans)")
                return recordingsRealClose(b)
            }
            if !b.holding { problems.append("recordings: today is not held") }
            guard let key = b.dayIncidents.first?.coverKeys.first else { return recordingsRealClose(b) }
            rec.requestThumbnail(key)
            when({
                switch rec.thumbnails[key] {
                case .image, .failed, .empty: true
                default: false
                }
            }, timeout: 30) { [self] in
                var putBack = false
                if case .ready(let t)? = try? model.bridge.recordings().thumbnailGet(key: key.json), t.kind == .image {
                    putBack = true
                }
                var bytes = 0
                if case .image(let jpeg) = rec.thumbnails[key] { bytes = jpeg.count }
                print("recordings (real core): thumbnail image \(bytes) bytes, put back \(putBack)")
                if !putBack { problems.append("recordings: the cover keyframe was not decoded and put back") }
                recordingsRealPlayback(b, rec, calls)
            }
        }
    }

    /// The player on the real core: the day's first finished incident plays
    /// through the playback engine against the mock, its pictures decode
    /// on the tile pipeline, the clock moves, and pausing holds it.
    func recordingsRealPlayback(_ b: RecordingsBrowser, _ rec: RecordingsModel, _ calls: SystemCalls) {
        // parity: playback-real-core
        let p = b.player
        guard let incident = b.dayIncidents.first(where: { !$0.growing }) ?? b.dayIncidents.first else {
            problems.append("recordings: no incident to play on the real core")
            return recordingsRealDownload(b, rec, calls)
        }
        b.click(incident.key, extend: false)
        var first: Int64?
        when({
            guard p.playback != nil, p.cells.allSatisfy({ $0.tile.latestPicture() != nil }) else { return false }
            if first == nil { first = p.positionMs }
            return p.positionMs > (first ?? 0) + 500
        }, timeout: 30) { [self] in
            print("recordings (real core): playback of \(p.cells.count) camera(s), \(p.cells.map(\.state)), at \(TransportState.clock(p.positionMs)), moved \(p.positionMs - (first ?? p.positionMs)) ms")
            if p.playback == nil || p.cells.contains(where: { $0.tile.latestPicture() == nil }) {
                problems.append("recordings: the real core's playback showed no picture (\(p.message ?? "no message"))")
            } else if p.positionMs <= (first ?? 0) + 500 {
                problems.append("recordings: the real core's playback clock did not move")
            }
            p.pause()
            when({ p.state == .paused }, timeout: 10) { [self] in
                if p.state != .paused { problems.append("recordings: the real core's playback did not pause") }
                recordingsRealPacing(b, rec) { [self] in recordingsRealDownload(b, rec, calls) }
            }
        }
    }

    /// Playback pacing (docs/RENDERING.md "Playback pacing"), on the path a
    /// viewer takes: a finished one-camera incident selected on the day's
    /// timeline plays at 1x from its start less its margin. The tile notes
    /// when each picture is released to the app, decoded, and first drawn by
    /// the player's renderer. The verdict (`PacingVerdict`) is given on a
    /// simulated 60 Hz display fed with the real decode times
    /// (`PacingRefresh`), so it does not depend on the refreshes the real
    /// display missed; the real display must still draw most of the
    /// released pictures (a wiring check). The waits are counts of pictures:
    /// past the play's opening burst, then until enough pairs are decoded.
    /// What this cannot show on its own is proven on simulated time: the
    /// release pacing in the core's tests (tests/playback.rs, a wrong UTC
    /// offset), the bounds and the old bugs injected in PacingStatsTests.
    func recordingsRealPacing(
        _ b: RecordingsBrowser, _ rec: RecordingsModel, then next: @escaping @MainActor () -> Void
    ) {
        // parity: playback-pacing
        func oneCamera() -> Incident? { b.dayIncidents.first(where: { !$0.growing && $0.cameras.count == 1 }) }
        if let incident = oneCamera() { return recordingsRealPacing(b, incident, then: next) }
        // The mock records days by UTC date: early in the day (or late, west
        // of UTC) today's incidents still grow, and yesterday has finished ones.
        b.previousDay()
        when({ oneCamera() != nil && !rec.searching.busy }, timeout: 30) { [self] in
            recordingsRealPacing(b, oneCamera()) { [self] in
                // Back on the day the other real-core steps work on, listed again.
                b.nextDay()
                when({ !b.dayIncidents.isEmpty && !rec.searching.busy }, timeout: 30) { next() }
            }
        }
    }

    func recordingsRealPacing(_ b: RecordingsBrowser, _ found: Incident?, then next: @escaping @MainActor () -> Void) {
        let p = b.player
        guard let incident = found, let renderer = app.recordingsWindows.first?.stage?.grid?.renderer else {
            let counts = b.dayIncidents.map { "\($0.cameras.count)\($0.growing ? "+" : "")" }.joined(separator: ",")
            problems.append(
                "playback pacing: no one-camera incident (cameras per incident: \(counts)) or player stage "
                    + "(\(app.recordingsWindows.first?.stage == nil ? "none" : "present"))")
            return next()
        }
        b.click(incident.key, extend: false)
        var recorder: PacingRecorder?
        renderer.onNewPicture = { tile, pts, at in
            if tile == 0 { recorder?.shown(pts, at: at) }
        }
        when({
            if recorder == nil, p.cells.count == 1 {
                recorder = p.cells[0].tile.pacing
                recorder?.enabled = true
            }
            // Past the opening burst: the device sends its first pictures faster than their time.
            return p.state == .playing && (recorder?.report().decoded ?? 0) >= 20
        }, timeout: 30) { [self] in
            guard let recorder, p.state == .playing, recorder.report().decoded >= 20 else {
                renderer.onNewPicture = nil
                problems.append("playback pacing: the incident's playback did not start (\(p.message ?? "no message"))")
                return next()
            }
            recorder.reset()
            let before = p.positionMs, began = CACurrentMediaTime()
            when({ recorder.report().decoded > Self.playbackPacingPairs + 2 }, timeout: 30) { [self] in
                renderer.onNewPicture = nil
                recorder.enabled = false
                let r = recorder.report()
                let drawn = r.released.pictures > 0 ? Double(r.shown.pictures) / Double(r.released.pictures) : 0
                print(
                    "playback pacing: incident \(incident.start)..\(incident.end) margin \(incident.margin) s, position moved "
                        + "\(p.positionMs - before) ms in \(String(format: "%.2f", CACurrentMediaTime() - began)) s, "
                        + "real display drew \(String(format: "%.2f", drawn)); \(r)")
                problems.append(contentsOf: r.verdict(minPairs: Self.playbackPacingPairs).map { "playback pacing: \($0)" })
                if drawn < Self.playbackDrawnRatio {
                    problems.append("playback pacing: the real display drew \(r.shown.pictures) of \(r.released.pictures) released pictures")
                }
                next()
            }
        }
    }

    func recordingsRealDownload(_ b: RecordingsBrowser, _ rec: RecordingsModel, _ calls: SystemCalls) {
        b.previousDay()
        if b.player.playback != nil { problems.append("recordings: another day kept the playback open") }
        if b.holding { problems.append("recordings: yesterday is held") }
        when({ b.dayIncidents.contains { !$0.growing } && !rec.searching.busy }, timeout: 30) { [self] in
            guard let incident = b.dayIncidents.first(where: { !$0.growing }) else {
                problems.append("recordings: yesterday has no incidents")
                return recordingsRealClose(b)
            }
            b.setLocked(incident.key, true)
            when({ rec.downloads.contains { $0.state == "done" || $0.state == "failed" } }, timeout: 60) { [self] in
                guard let d = rec.downloads.first(where: { $0.incident == incident.key }) else {
                    problems.append("recordings: locking downloaded nothing")
                    return recordingsRealClose(b)
                }
                print("recordings (real core): lock \(rec.notice ?? ""), download \(d.stateText), \(d.outputs.count) files")
                if d.state != "done" || d.outputs.isEmpty { problems.append("recordings: the download ended \(d.stateText)") }
                recordingsRealAnalyze(b, rec, camera: d.cameras.first?.camera) { [self] in
                    recordingsRealExport(rec, incident: incident, download: d) { [self] in
                        rec.trash(d.download)
                        when({ !calls.trashed.isEmpty }, timeout: 10) { [self] in
                            if calls.trashed.map(\.path) != [d.folder ?? ""] { problems.append("recordings: the Trash got \(calls.trashed)") }
                            b.setLocked(incident.key, false)
                            recordingsRealClose(b)
                        }
                    }
                }
            }
        }
    }

    /// Export on the real core: a keyframe trim of the finished download's
    /// first camera (planned by the core, copied by `TrimExporter`, read back),
    /// then a range download of that camera, planned and finished by the core
    /// against the mock, whose `download` event carries its range.
    func recordingsRealExport(
        _ rec: RecordingsModel, incident: Incident, download d: DownloadProgress, then next: @escaping @MainActor () -> Void
    ) {
        // parity: export-real-core
        guard let camera = d.cameras.first?.camera else { return next() }
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("nvrview-export-real-\(UUID().uuidString)")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let from = incident.start * 1000 + 500, to = from + 2_000
        let request = ExportRequest.trim(download: d.download, cameras: [camera], inMs: from, outMs: to, exact: false, options: .init())
        switch rec.trimPlan(json: request) {
        case .failure(let refusal):
            problems.append("export: the real core planned no trim (\(refusal.text))")
            try? FileManager.default.removeItem(at: dir)
            return next()
        case .success(let plan):
            let out = plan.outputs[0]
            let target = dir.appendingPathComponent("trim.mp4")
            awaiting({ () async -> (ExportOutcome, Double)? in
                let outcome = await TrimExporter.export(out, exact: false, to: target, shareSafe: true, control: ExportControl())
                guard let seconds = try? await AVURLAsset(url: target).load(.duration).seconds else { return nil }
                return (outcome, seconds)
            }) { [self] got in
                let want = Double(out.sourceOutMs - out.sourceInMs) / 1000
                print("export (real core): trim \(out.sourceInMs)–\(out.sourceOutMs) ms, copy \(out.copyPossible), wrote \(got.map { "\($0.0.result) \($0.1) s" } ?? "nothing"), expected \(want) s")
                if got?.0.result != .done || abs((got?.1 ?? 0) - want) > 0.5 || !out.copyPossible {
                    problems.append("export: the real core's trim wrote \(String(describing: got?.0.result)) lasting \(got?.1 ?? 0) s, expected \(want) s")
                }
                try? FileManager.default.removeItem(at: dir)
                let queued = rec.downloadRange(cameras: [camera], startMs: from, endMs: to, name: nil, options: .init())
                guard let id = queued?.download else {
                    problems.append("export: the real core refused the range download (\(rec.notice ?? "no message"))")
                    return next()
                }
                when({ rec.downloads.first { $0.download == id }.map { $0.state == "done" || $0.state == "failed" } == true }, timeout: 60) { [self] in
                    let r = rec.downloads.first { $0.download == id }
                    print("export (real core): range download \(r?.rangeText ?? "none"), \(r?.stateText ?? "?"), \(r?.outputs.count ?? 0) files")
                    if r?.state != "done" || r?.range != DownloadRange(start: from / 1000, end: (to + 999) / 1000) || r?.outputs.count != 1 {
                        problems.append("export: the real core's range download is \(String(describing: r?.range)) \(r?.stateText ?? "missing")")
                    }
                    next()
                }
            }
        }
    }

    // MARK: Recording on the real core

    /// Analyze Sounds on the real core over the day just downloaded from the
    /// mock (which records the same synthetic audio on every channel): the
    /// core reads the files' audio, and the lane has loudness. Skipped (and
    /// said so) when the mock was started without audio.
    func recordingsRealAnalyze(
        _ b: RecordingsBrowser, _ rec: RecordingsModel, camera: RecordingCamera?, then next: @escaping @MainActor () -> Void
    ) {
        // parity: recordings-analyze-real-core
        guard let camera, b.lanes.contains(where: { $0.camera == camera }) else {
            problems.append("recordings: no lane for the downloaded camera")
            return next()
        }
        b.analyzeSound(camera)
        when({ !rec.isAnalyzing(camera, day: b.day) && rec.analysis(camera, day: b.day)?.active == false }, timeout: 60) { [self] in
            let entry = rec.analysis(camera, day: b.day)
            print("recordings (real core): analyzed \(entry?.seconds ?? 0) s, \(entry?.labels ?? 0) labels, \(entry?.state.rawValue ?? "no result"): \(rec.notice ?? "no message")")
            if entry?.state != .done {
                problems.append("recordings: the real core's analysis ended \(String(describing: entry))")
                return next()
            }
            if (entry?.seconds ?? 0) <= 0 { problems.append("recordings: the real core heard no seconds of the download") }
            b.reloadLoudness(force: true)
            if b.loudness(of: camera)?.heard != true { problems.append("recordings: no loudness on the lane after the real analysis") }
            next()
        }
    }

    func recordingsRealClose(_ b: RecordingsBrowser) {
        app.recordingsWindows.first?.window?.close()
        if b.holding { problems.append("recordings: closing the window kept its hold") }
        checkLog()
    }
}
