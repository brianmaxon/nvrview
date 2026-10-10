import FakeCore
import Foundation
import NVRCore
import Testing

// Device discovery (docs/FFI.md "Device discovery", minor 1): event parsing,
// the list a "find devices" screen shows, and the fake core's made-up network.
// Addresses are documentation addresses.

@Suite struct DiscoveryEventTests {
    private func parse(_ s: String) -> CoreEvent? { CoreEvent.parse(Data(s.utf8)) }

    @Test func discoveredCarriesWhatTheAnswerSaid() {
        let e = parse(
            #"{"type":"discovered","search":2,"key":"d1","kind":"nvr","name":"NVR 1","address":"192.0.2.10","port":9000,"channels":16,"services":[{"kind":"native","port":9000}],"future":1}"#
        )
        #expect(
            e
                == .discovered(
                    DiscoveredDevice(
                        search: 2, key: "d1", kind: .nvr, name: "NVR 1", address: "192.0.2.10", port: 9000,
                        channels: 16, services: [DiscoveredService(kind: "native", port: 9000)])))
        let camera = parse(
            #"{"type":"discovered","search":2,"key":"d2","kind":"camera","model":"EXAMPLE-CAM","address":"192.0.2.20","battery_percent":80,"services":[{"kind":"onvif","port":8000,"url":"http://192.0.2.20:8000/onvif/device_service"}]}"#
        )
        guard case .discovered(let d)? = camera else { Issue.record("not discovered: \(String(describing: camera))"); return }
        #expect(d.batteryPercent == 80 && d.model == "EXAMPLE-CAM" && d.name == nil)
        #expect(!d.isNative && d.suggestedPort == 9000 && d.title == "Camera")
        #expect(parse(#"{"type":"discovered","search":2,"address":"192.0.2.20"}"#) == nil)
    }

    @Test func startedAndDone() {
        #expect(parse(#"{"type":"discovery_started","search":3}"#) == .discoveryStarted(search: 3))
        #expect(
            parse(#"{"type":"discovery_done","search":3,"count":2,"stopped":true,"errors":["en1: busy"]}"#)
                == .discoveryDone(DiscoveryEnd(search: 3, count: 2, stopped: true, errors: ["en1: busy"])))
    }

    @Test func addedMatchesAddressAndPort() {
        let d = DiscoveredDevice(search: 1, key: "d1", address: "192.0.2.10", port: 9000)
        #expect(d.isAdded(among: [(host: "192.0.2.10", port: 9000)]))
        #expect(!d.isAdded(among: [(host: "192.0.2.10", port: 9001)]))
        #expect(!d.isAdded(among: [(host: "192.0.2.11", port: 9000)]))
    }
}

@Suite struct DiscoveryListTests {
    private func device(_ search: UInt64, _ key: String, name: String? = nil, model: String? = nil) -> CoreEvent {
        .discovered(DiscoveredDevice(search: search, key: key, name: name, model: model, address: "192.0.2.\(key.count)"))
    }

    @Test func fillsAndFinishes() {
        var list = DiscoveryList()
        list.requested()
        #expect(list.isSearching && list.devices.isEmpty)
        // Events before the start is acknowledged are not ours yet.
        list.apply(.discoveryStarted(search: 5))
        list.apply(device(5, "d1", name: "NVR 1"))
        list.apply(device(5, "d2"))
        // The same device again with more: replaced, not repeated.
        list.apply(device(5, "d1", name: "NVR 1", model: "EXAMPLE-MODEL"))
        #expect(list.devices.map(\.key) == ["d1", "d2"])
        #expect(list.devices[0].model == "EXAMPLE-MODEL")
        list.apply(.discoveryDone(DiscoveryEnd(search: 5, count: 2, stopped: false)))
        #expect(list.phase == .finished(DiscoveryEnd(search: 5, count: 2, stopped: false)))
        #expect(!list.foundNothing && list.devices.count == 2)
    }

    @Test func nothingFoundSaysSo() {
        var list = DiscoveryList()
        list.requested()
        list.apply(.discoveryStarted(search: 1))
        list.apply(.discoveryDone(DiscoveryEnd(search: 1, count: 0, stopped: false, errors: ["en1: busy"])))
        #expect(list.foundNothing)
        #expect(list.errors == ["en1: busy"])
    }

    @Test func refusedSendsPointAtThePermission() {
        var list = DiscoveryList()
        list.requested()
        list.apply(.discoveryStarted(search: 1))
        list.apply(
            .discoveryDone(
                DiscoveryEnd(
                    search: 1, count: 0, stopped: false,
                    errors: ["ONVIF from 192.0.2.5: send failed: No route to host (os error 65)"])))
        #expect(list.foundNothing && list.permissionLikelyBlocked)
        // A busy port is not a permission problem.
        var other = DiscoveryList()
        other.requested()
        other.apply(.discoveryStarted(search: 1))
        other.apply(.discoveryDone(DiscoveryEnd(search: 1, count: 0, stopped: false, errors: ["en1: address in use"])))
        #expect(other.foundNothing && !other.permissionLikelyBlocked)
    }

    @Test func aRestartIgnoresTheStoppedSearch() {
        var list = DiscoveryList()
        list.requested()
        list.apply(.discoveryStarted(search: 1))
        list.apply(device(1, "d1"))
        // Search Again twice in a row: both starts are acknowledged, in order,
        // with the first one's end in between.
        list.requested()
        list.requested()
        #expect(list.devices.isEmpty)
        list.apply(device(1, "d9"))  // late answer of the first search
        list.apply(.discoveryDone(DiscoveryEnd(search: 1, count: 1, stopped: true)))
        list.apply(.discoveryStarted(search: 2))
        list.apply(device(2, "d1"))  // the second search, which is already stopped
        list.apply(.discoveryDone(DiscoveryEnd(search: 2, count: 1, stopped: true)))
        list.apply(.discoveryStarted(search: 3))
        list.apply(device(3, "d1"))
        list.apply(device(3, "d2"))
        #expect(list.devices.map(\.key) == ["d1", "d2"])
        #expect(list.devices.allSatisfy { $0.search == 3 })
        #expect(list.isSearching)
    }

    @Test func stoppingAndRefusal() {
        var list = DiscoveryList()
        list.requested()
        list.apply(.discoveryStarted(search: 1))
        list.stopped()
        list.apply(device(1, "d1"))
        list.apply(.discoveryDone(DiscoveryEnd(search: 1, count: 0, stopped: true)))
        #expect(list.phase == .idle && list.devices.isEmpty)
        list.failedToStart()
        #expect(list.unavailable && !list.isSearching)
    }
}

@Suite struct FakeDiscoveryTests {
    @Test func aSearchListsTheMadeUpNetworkAndEnds() throws {
        let clip = try Fixtures.clip("h264-basic.h264")
        let rig = try Rig(clip)
        try rig.bridge.discoverStart()
        #expect(rig.fake.isDiscovering)
        rig.fake.finishDiscovery()
        #expect(!rig.fake.isDiscovering)
        var list = DiscoveryList()
        list.requested()
        for e in rig.events.get() { list.apply(e) }
        #expect(list.devices.count == FakeDiscoverable.samples.count)
        #expect(list.phase == .finished(DiscoveryEnd(search: 1, count: FakeDiscoverable.samples.count, stopped: false)))
        // No identifiers: the made-up network has none to leak.
        #expect(rig.fake.violations.isEmpty, "\(rig.fake.violations)")
    }

    @Test func startWhileRunningStopsTheFirstAndBadRequestsAreRefused() throws {
        let clip = try Fixtures.clip("h264-basic.h264")
        let rig = try Rig(clip)
        try rig.bridge.discoverStart()
        try rig.bridge.discoverStart(seconds: 5)
        let done = rig.events.get().compactMap { e -> DiscoveryEnd? in
            if case .discoveryDone(let end) = e { return end }
            return nil
        }
        #expect(done.map(\.search) == [1] && done.map(\.stopped) == [true])
        #expect(throws: CoreStatus.self) { try rig.bridge.discoverStart(seconds: 0) }
        rig.bridge.discoverStop()
        rig.bridge.discoverStop()
        #expect(!rig.fake.isDiscovering)
        rig.bridge.shutdown()
        #expect(rig.fake.violations.isEmpty, "\(rig.fake.violations)")
    }
}
