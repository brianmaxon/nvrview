import AppKit
import AVFoundation
import QuartzCore
import Foundation
import FakeCore
import NVRCore
import VideoKit

// Fisheye cameras: the image circle, the dewarped views, the virtual camera: the self-test's "tiles" steps (scripts/macos-selftest-areas.txt).
// Part of SelfTest (SelfTest.swift has the order of the steps and the shared helpers).

/// The self-test's own `print` (elapsed seconds in front; see SelfTest.swift).
private func print(_ line: String) { selfTestPrint(line) }

extension SelfTest {
    /// The synthetic fisheye fixture's colors by azimuth quadrant, ceiling
    /// mount (fixtures/synthetic/make-fisheye.sh): 0-90° red, 90-180° green,
    /// 180-270° blue, 270-360° yellow; 15° checker cells, a white dot at
    /// tilt 20°. Nil when a pixel is none of them clearly.
    static func fixtureSector(_ c: SIMD3<Int>) -> Int? {
        let (r, g, b) = (c.x, c.y, c.z)
        if r > g + 40 && r > b + 40 { return 0 }
        if g > r + 30 && g > b + 30 { return 1 }
        if b > r + 40 && b > g + 40 { return 2 }
        if r > b + 50 && g > b + 50 { return 3 }
        return nil
    }

    /// With `--fake` and the fisheye fixture: a fisheye camera put in a tile
    /// finds its circle, and every view draws through the dewarp pipeline
    /// with pixels matching the fixture's colors where the reference mapping
    /// says they should; a drag and a wheel step move the virtual camera
    /// (eased on the clock), saved with the layout; 0 resets it.
    func fisheyeStep() {
        guard let model = app.model, let window else { finish(problems) }
        guard runs("tiles") else { return checkLog() }
        // parity: fisheye-detected
        guard let camera = model.allCameras.first(where: { model.isFisheye($0) }) else {
            problems.append("no fisheye camera (run with fixtures/synthetic/fisheye/h264-fisheye.h264 among the files)")
            return checkLog()
        }
        let stage = window.stage
        guard let path = window.state.tree.frames(in: stage.bounds).leaves.first?.path else {
            problems.append("no tile for the fisheye camera")
            return checkLog()
        }
        stage.assign(camera, at: path)
        when({ model.leases.tile(for: camera)?.latestPicture() != nil }, timeout: 3) { [self] in
            let grid = stage.grid
            // parity: fisheye-circle
            // The circle is measured from the first pictures, on the clock.
            stage.setFisheyeMode(.panorama, at: path)
            for _ in 0..<FisheyeCircleTracker.maxTries where stage.fisheyeDewarp(of: path) == nil {
                grid.prepare?(grid.renderer, grid.drawableSize)
                advance(FisheyeCircleTracker.interval)
            }
            guard let lens = stage.fisheyeDewarp(of: path)?.lens else {
                problems.append("the fisheye tile is not dewarped")
                return checkLog()
            }
            let c = lens.circle
            print(String(format: "fisheye circle: center %.4f, %.4f radius %.4f (fixture 0.49, 0.505, 0.46)", c.cx, c.cy, c.r))
            if abs(c.cx - 0.49) > 0.01 || abs(c.cy - 0.505) > 0.01 || abs(c.r - 0.46) > 0.01 {
                problems.append("the fisheye circle is more than 1% off the fixture's")
            }
            // parity: fisheye-view-pixels
            for mode in FisheyeViewMode.allCases where mode.dewarps {
                stage.setFisheyeMode(mode, at: path)
                checkFisheyePixels(path, mode)
            }
            // parity: fisheye-ptz-drag
            // Virtual PTZ: a drag turns it, a wheel step zooms it (eased).
            let size = window.state.tree.frames(in: stage.bounds).leaf(path)?.rect.size ?? .zero
            stage.turnFisheye(path, by: CGVector(dx: size.width / 4, dy: 0), size: size)
            // parity: fisheye-ptz-zoom
            stage.zoomFisheye(path, by: 2, eased: true)
            advance(0.03)
            let midway = window.state.tree[path]?.fisheye.aim?.fov ?? 0
            advance(1)
            let aim = window.state.tree[path]?.fisheye.aim
            print(String(format: "virtual PTZ: pan %.1f, field of view %.1f after a second (%.1f midway)",
                aim?.pan ?? .nan, aim?.fov ?? .nan, midway))
            if aim.map({ abs($0.pan + 22.5) < 0.01 }) != true { problems.append("a drag did not turn the virtual camera") }
            if aim?.fov != 45 || stage.isEasingFisheye || !(midway > 45 && midway < 90) {
                problems.append("the virtual camera did not ease its zoom to 45°")
            }
            checkFisheyePixels(path, .ptz)
            // parity: fisheye-reset-key
            stage.resetZoom(at: path)
            if window.state.tree[path]?.fisheye.aim != nil { problems.append("0 did not reset the virtual camera") }
            // parity: fisheye-original
            stage.setFisheyeMode(.original, at: path)
            grid.prepare?(grid.renderer, grid.drawableSize)
            if stage.fisheyeDewarp(of: path) != nil { problems.append("Original still dewarps") }
            fisheyePictureStep(path, camera)
        }
    }

    /// How many of the fisheye view's sample points show the fixture's color
    /// for the direction they look along (`pixel` reads the picture at
    /// whole-pixel positions of `rect`, where the view is drawn).
    static func fisheyeMatches(
        _ dewarp: FisheyeDewarp, in rect: CGRect, pixel: (Int, Int) -> SIMD3<Int>
    ) -> (checked: Int, matched: Int) {
        var checked = 0, matched = 0
        let step = max(rect.width, rect.height) / 40
        for y in stride(from: rect.minY + 1, to: rect.maxY - 1, by: step) {
            for x in stride(from: rect.minX + 1, to: rect.maxX - 1, by: step) {
                let t = CGPoint(x: (x + 0.5 - rect.minX) / rect.width, y: (y + 0.5 - rect.minY) / rect.height)
                guard let d = dewarp.direction(atTile: t), dewarp.picturePoint(atTile: t) != nil else { continue }
                let (pan, tilt) = panTilt(of: d)
                let psi = (pan + 360).truncatingRemainder(dividingBy: 360)
                func clear(_ a: Double) -> Bool {
                    let m = a.truncatingRemainder(dividingBy: 15)
                    return m > 3 && m < 12
                }
                guard tilt > 5, tilt < 80, abs(tilt - 20) > 7, clear(psi), clear(tilt) else { continue }
                checked += 1
                if fixtureSector(pixel(Int(x), Int(y))) == Int(psi / 90) % 4 { matched += 1 }
            }
        }
        return (checked, matched)
    }

    /// Draws the window offscreen and compares the fisheye tile's pixels
    /// with the fixture's color at the direction each one looks along.
    func checkFisheyePixels(_ path: NodePath, _ mode: FisheyeViewMode) {
        guard let window else { return }
        let stage = window.stage
        let grid = stage.grid
        grid.prepare?(grid.renderer, grid.drawableSize)
        guard let dewarp = stage.fisheyeDewarp(of: path), let rect = stage.renderedDestination(of: path),
            let image = grid.renderer.renderOffscreen(width: Int(grid.drawableSize.width), height: Int(grid.drawableSize.height))
        else {
            problems.append("\(mode.title): not drawn through the dewarp pipeline")
            return
        }
        let drawn = grid.renderer.dewarpedTilesDrawn
        let (checked, matched) = Self.fisheyeMatches(dewarp, in: rect) { image.rgb(x: $0, y: $1) }
        print("fisheye \(mode.title): dewarped tiles drawn \(drawn), pixels matching the fixture \(matched) of \(checked)")
        if drawn < 1 { problems.append("\(mode.title): the dewarp pipeline drew nothing") }
        if checked < 20 || Double(matched) < 0.9 * Double(checked) {
            problems.append("\(mode.title): \(matched) of \(checked) pixels match the fixture")
        }
    }
}
