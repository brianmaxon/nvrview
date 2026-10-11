import CoreGraphics
import Foundation
import NVRCore
import Testing

@Suite struct WindowRestoreTests {
    private let screen = CGRect(x: 0, y: 0, width: 1440, height: 875)
    private let min = CGSize(width: 640, height: 400)

    @Test func roundTripsThroughJSON() throws {
        let windows = [
            SavedWindow(content: .layout(id: "layout-1"), frame: CGRect(x: 10, y: 20, width: 1180, height: 700)),
            SavedWindow(content: .group(id: "group-1"), frame: CGRect(x: 0, y: 0, width: 800, height: 500), fullScreen: true),
            SavedWindow(
                content: .scratch(
                    name: "Unsaved",
                    root: .split(.row, ratio: 0.5, a: .leaf(CameraRef(device: "dev-a", channel: 2)), b: .leaf(nil))),
                frame: CGRect(x: -1200, y: 300, width: 900, height: 600)),
        ]
        let data = try JSONEncoder().encode(windows)
        #expect(try JSONDecoder().decode([SavedWindow].self, from: data) == windows)
        // Nothing but ids, names, trees and frames.
        let text = String(decoding: data, as: UTF8.self)
        #expect(!text.contains("password") && !text.contains("host"))
    }

    @Test func sidebarStateRoundTripsAndOldFilesLoadOpen() throws {
        let w = SavedWindow(
            content: .layout(id: "layout-1"), frame: CGRect(x: 1, y: 2, width: 900, height: 600), sidebarCollapsed: true)
        let back = try JSONDecoder().decode([SavedWindow].self, from: JSONEncoder().encode([w]))
        #expect(back == [w])
        #expect(back[0].sidebarCollapsed)
        // A file from before the field: take a fresh encoding and strip the key.
        let sample = try JSONEncoder().encode([SavedWindow(content: .layout(id: "layout-1"), frame: .zero)])
        var obj = try #require(JSONSerialization.jsonObject(with: sample) as? [[String: Any]])
        obj[0].removeValue(forKey: "sidebarCollapsed")
        let stripped = try JSONSerialization.data(withJSONObject: obj)
        let loaded = try JSONDecoder().decode([SavedWindow].self, from: stripped)
        #expect(loaded.count == 1 && !loaded[0].sidebarCollapsed)
    }

    @Test func insetAnchorRoundTripsAndIsOptional() throws {
        let w = SavedWindow(
            content: .layout(id: "layout-1"), frame: CGRect(x: 1, y: 2, width: 900, height: 600),
            insetAnchor: InsetAnchor(x: 0.25, y: 1))
        let back = try JSONDecoder().decode([SavedWindow].self, from: JSONEncoder().encode([w]))
        #expect(back == [w])
        #expect(back[0].insetAnchor == InsetAnchor(x: 0.25, y: 1))
        // Nil is left out of the file; old files and unreadable values load as nil.
        let plain = try JSONEncoder().encode([SavedWindow(content: .layout(id: "layout-1"), frame: .zero)])
        #expect(!String(decoding: plain, as: UTF8.self).contains("insetAnchor"))
        #expect(try JSONDecoder().decode([SavedWindow].self, from: plain)[0].insetAnchor == nil)
        var obj = try #require(JSONSerialization.jsonObject(with: plain) as? [[String: Any]])
        obj[0]["insetAnchor"] = "top"
        let bad = try JSONSerialization.data(withJSONObject: obj)
        #expect(try JSONDecoder().decode([SavedWindow].self, from: bad)[0].insetAnchor == nil)
        // Out of range values are clamped.
        obj[0]["insetAnchor"] = ["x": 3.0, "y": -1.0]
        let wide = try JSONSerialization.data(withJSONObject: obj)
        #expect(try JSONDecoder().decode([SavedWindow].self, from: wide)[0].insetAnchor == InsetAnchor(x: 1, y: 0))
    }

    @Test func keepsAFrameWhoseTitleBarIsOnAScreen() {
        let f = CGRect(x: 100, y: 100, width: 900, height: 600)
        #expect(SavedWindow.placement(of: f, screens: [screen], minSize: min) == f)
        // On a second display to the left.
        let left = CGRect(x: -1920, y: 0, width: 1920, height: 1080)
        let there = CGRect(x: -1500, y: 200, width: 900, height: 600)
        #expect(SavedWindow.placement(of: there, screens: [screen, left], minSize: min) == there)
        // Mostly off screen, but enough of the title bar to grab.
        let edge = CGRect(x: 1300, y: 100, width: 900, height: 600)
        #expect(SavedWindow.placement(of: edge, screens: [screen], minSize: min) == edge)
    }

    @Test func movesAWindowLeftOffScreenOntoTheFirstScreen() {
        // The display it was on is gone.
        let gone = CGRect(x: -1500, y: 200, width: 900, height: 600)
        #expect(
            SavedWindow.placement(of: gone, screens: [screen], minSize: min)
                == CGRect(x: 270, y: 138, width: 900, height: 600))
        // Title bar above the top of the screen.
        let above = CGRect(x: 100, y: 870, width: 900, height: 600)
        #expect(SavedWindow.placement(of: above, screens: [screen], minSize: min)?.maxY ?? .infinity <= screen.maxY)
        // Larger than the screen: shrunk to it.
        let huge = CGRect(x: -9000, y: 0, width: 3000, height: 2000)
        #expect(SavedWindow.placement(of: huge, screens: [screen], minSize: min) == screen)
    }

    @Test func neverSmallerThanTheMinimumAndNilWithoutScreens() {
        let tiny = CGRect(x: 100, y: 100, width: 10, height: 10)
        #expect(SavedWindow.placement(of: tiny, screens: [screen], minSize: min)?.size == min)
        #expect(SavedWindow.placement(of: tiny, screens: [], minSize: min) == nil)
    }
}
