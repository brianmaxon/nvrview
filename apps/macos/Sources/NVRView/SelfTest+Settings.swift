import AppKit
import AVFoundation
import QuartzCore
import Foundation
import FakeCore
import NVRCore
import VideoKit

// Settings: the device search: the self-test's "settings" steps (scripts/macos-selftest-areas.txt).
// Part of SelfTest (SelfTest.swift has the order of the steps and the shared helpers).

/// The self-test's own `print` (elapsed seconds in front; see SelfTest.swift).
private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// The add-device screen's "Find Devices on This Network" list against the
    /// fake core's made-up devices (docs/UX.md, "Finding devices").
    func discoveryStep() {
        guard let model = app.model, let fake = model.fake, let settings = app.settingsWindowForTest() else {
            problems.append("no settings window")
            return checkLogNow()
        }
        let form = model.deviceForm
        // The add form is what Settings shows with no device selected; its
        // list searches when it appears.
        // parity: discovery-list
        form.startNew()
        when({ model.discovery.devices.count == FakeDiscoverable.samples.count }, timeout: 5) { [self] in
            let list = model.discovery
            print("discovery: \(list.devices.count) devices, searching \(list.isSearching)")
            if list.devices.count != FakeDiscoverable.samples.count || !list.isSearching {
                problems.append("the list did not fill while searching")
                return finishDiscovery(settings)
            }
            let byAddress = Dictionary(uniqueKeysWithValues: list.devices.map { ($0.address, $0) })
            guard let known = byAddress["192.0.2.10"], let fresh = byAddress["192.0.2.12"],
                let battery = byAddress["192.0.2.30"], let onvif = byAddress["192.0.2.40"]
            else {
                problems.append("a made-up device is missing from the list")
                return finishDiscovery(settings)
            }
            if !model.isAdded(known) || model.isAdded(fresh) { problems.append("added devices were not marked") }
            if fresh.channels != 16 || fresh.kind != .nvr || battery.batteryPercent != 80 {
                problems.append("device details did not arrive")
            }
            // parity: discovery-choose
            // A click on a row.
            model.chooseDiscovered(fresh)
            if form.draft.host != "192.0.2.12" || form.port != "9000" || form.draft.name != "NVR 3" {
                problems.append("choosing a device did not fill the form: \(form.draft.host) \(form.port) \(form.draft.name)")
            }
            // A device without a port in its answer gets the usual one; a name the user
            // changed stays; one the viewer cannot add is not chosen.
            form.draft.name = "My NVR"
            model.chooseDiscovered(battery)
            if form.draft.host != "192.0.2.30" || form.port != "9000" || form.draft.name != "My NVR" {
                problems.append("choosing the second device went wrong: \(form.draft.host) \(form.port) \(form.draft.name)")
            }
            model.chooseDiscovered(onvif)
            if form.draft.host != "192.0.2.30" { problems.append("a device the viewer cannot add was chosen") }
            model.chooseDiscovered(known)
            if form.draft.host != "192.0.2.30" { problems.append("an added device was chosen") }

            // The search's time is up; the list stays.
            fake.finishDiscovery()
            when({ !model.discovery.isSearching }, timeout: 5) { [self] in
                if model.discovery.devices.count != FakeDiscoverable.samples.count || model.discovery.foundNothing {
                    problems.append("the list changed when the search ended")
                }
                // parity: discovery-search-again
                // Search Again twice in a row: the list shows each device once.
                model.startDiscovery()
                model.startDiscovery()
                when({ model.discovery.isSearching && model.discovery.devices.count == FakeDiscoverable.samples.count },
                    timeout: 5
                ) { [self] in
                    let n = model.discovery.devices.count
                    print("discovery after Search Again twice: \(n) devices")
                    if n != FakeDiscoverable.samples.count { problems.append("a restarted search listed \(n) devices") }
                    // parity: discovery-stop-on-close
                    // Closing the window stops the search.
                    settings.window?.close()
                    if fake.isDiscovering || model.discovery.isSearching {
                        problems.append("closing Settings did not stop the search")
                    }
                    finishDiscovery(settings)
                }
            }
        }
    }

    func finishDiscovery(_ settings: SettingsWindowController) {
        settings.window?.close()
        app.model?.stopDiscovery()
        if let fake = app.model?.fake { problems += fake.violations.map { "core contract: \($0)" } }
        checkLogNow()
    }
}
