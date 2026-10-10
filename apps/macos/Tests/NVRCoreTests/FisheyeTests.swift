import CoreGraphics
import Foundation
import NVRCore
import Testing
import VideoKit

private func close(_ a: Double, _ b: Double, _ tolerance: Double = 1e-6) -> Bool { abs(a - b) <= tolerance }

private func close(_ a: SIMD3<Double>, _ b: SIMD3<Double>, _ tolerance: Double = 1e-6) -> Bool {
    close(a.x, b.x, tolerance) && close(a.y, b.y, tolerance) && close(a.z, b.z, tolerance)
}

private func close(_ a: CGPoint?, _ b: CGPoint?, _ tolerance: Double = 1e-6) -> Bool {
    guard let a, let b else { return a == nil && b == nil }
    return close(Double(a.x), Double(b.x), tolerance) && close(Double(a.y), Double(b.y), tolerance)
}

private func normalized(_ v: SIMD3<Double>) -> SIMD3<Double> { v / (v * v).sum().squareRoot() }

@Suite struct FisheyeLensTests {
    let lens = FisheyeLens(circle: FisheyeCircle(cx: 0.49, cy: 0.505, r: 0.46), aspect: 1)

    @Test func pictureAndDirectionRoundTrip() {
        for u in stride(from: 0.05, through: 0.95, by: 0.05) {
            for v in stride(from: 0.05, through: 0.95, by: 0.05) {
                let p = CGPoint(x: u, y: v)
                guard let d = lens.direction(at: p) else {
                    // Outside the circle: no direction.
                    #expect(hypot(u - 0.49, v - 0.505) > 0.46 - 1e-9)
                    continue
                }
                #expect(close((d * d).sum(), 1))
                #expect(close(lens.point(for: d), p, 1e-9))
            }
        }
    }

    @Test func knownDirections() {
        // The optical axis is the circle's center; 90° off it is the rim.
        #expect(close(lens.point(for: SIMD3(0, 0, 1)), CGPoint(x: 0.49, y: 0.505)))
        #expect(close(lens.point(for: SIMD3(1, 0, 0)), CGPoint(x: 0.95, y: 0.505)))
        #expect(close(lens.point(for: SIMD3(0, -1, 0)), CGPoint(x: 0.49, y: 0.045)))
        // Equidistant: 45° off axis is half way to the rim.
        let d = SIMD3(sin(Double.pi / 4), 0, cos(Double.pi / 4))
        #expect(close(lens.point(for: d), CGPoint(x: 0.49 + 0.23, y: 0.505)))
        // Behind the lens: outside a 180° field of view.
        #expect(lens.point(for: SIMD3(0, 0.1, -1)) == nil)
        // A wider lens sees it, nearer the center.
        let wide = FisheyeLens(circle: lens.circle, fieldOfView: 200)
        #expect(wide.point(for: SIMD3(1, 0, 0)).map { abs(Double($0.x) - (0.49 + 0.46 * 90 / 100)) < 1e-9 } == true)
    }

    @Test func nonSquarePicturesScaleTheVerticalRadius() {
        // A 16:9 picture: the radius is a fraction of the width, so it spans
        // 16/9 as much of the height.
        let wide = FisheyeLens(circle: FisheyeCircle(cx: 0.5, cy: 0.5, r: 0.25), aspect: 16.0 / 9.0)
        #expect(close(wide.point(for: SIMD3(0, 1, 0)), CGPoint(x: 0.5, y: 0.5 + 0.25 * 16 / 9)))
        let p = CGPoint(x: 0.6, y: 0.4)
        #expect(close(wide.direction(at: p).flatMap(wide.point(for:)), p, 1e-9))
    }

    @Test func mountsRotateTheViewerFrame() {
        for mount in FisheyeMount.allCases {
            for v in [SIMD3(1.0, 0, 0), SIMD3(0, 1.0, 0), SIMD3(0, 0, 1.0), normalized(SIMD3(0.3, -0.5, 0.8))] {
                #expect(close(mount.fromLens(mount.toLens(v)), v))
            }
        }
        // Ceiling: straight down is the center; pan 0 at the horizon is the
        // top of the picture, pan 90° the right.
        let lens = FisheyeLens(circle: FisheyeCircle(cx: 0.5, cy: 0.5, r: 0.5))
        let at = { (m: FisheyeMount, pan: Double, tilt: Double) in
            lens.point(for: m.toLens(viewerDirection(pan: pan, tilt: tilt)))
        }
        #expect(close(at(.ceiling, 0, 90), CGPoint(x: 0.5, y: 0.5)))
        #expect(close(at(.ceiling, 0, 0), CGPoint(x: 0.5, y: 0)))
        #expect(close(at(.ceiling, 90, 0), CGPoint(x: 1, y: 0.5)))
        #expect(close(at(.ceiling, 180, 45), CGPoint(x: 0.5, y: 0.75)))
        // Desk: straight up is the center; pan 0 the bottom, pan 90° the right.
        #expect(close(at(.desk, 0, -90), CGPoint(x: 0.5, y: 0.5)))
        #expect(close(at(.desk, 0, 0), CGPoint(x: 0.5, y: 1)))
        #expect(close(at(.desk, 90, 0), CGPoint(x: 1, y: 0.5)))
        // Wall: ahead is the center, down is the bottom.
        #expect(close(at(.wall, 0, 0), CGPoint(x: 0.5, y: 0.5)))
        #expect(close(at(.wall, 0, 90), CGPoint(x: 0.5, y: 1)))
        #expect(close(at(.wall, -90, 0), CGPoint(x: 0, y: 0.5)))
    }

    @Test func panTiltRoundTrip() {
        for pan in stride(from: -170.0, through: 170, by: 34) {
            for tilt in stride(from: -80.0, through: 80, by: 20) {
                let (p, t) = panTilt(of: viewerDirection(pan: pan, tilt: tilt))
                #expect(close(p, pan, 1e-9) && close(t, tilt, 1e-9))
            }
        }
    }
}

@Suite struct FisheyeViewTests {
    let lens = FisheyeLens(circle: FisheyeCircle(cx: 0.5, cy: 0.5, r: 0.5))

    @Test func panoramaColumnsAreAzimuthsAndRowsTilts() {
        let size = CGSize(width: 1440, height: 240)
        let d = FisheyeDewarp(lens: lens, mount: .ceiling, mode: .panorama, aim: .default(for: .ceiling), tileSize: size)
        #expect(d.views.count == 1)
        // 360° over 1440 px: Δτ = 360·240/1440 = 60°, from the horizon down.
        guard case .cylindrical(let pan0, let panSpan, let tilt0, let tiltSpan) = d.views[0].projection else {
            Issue.record("not a strip")
            return
        }
        #expect(pan0 == -180 && panSpan == 360 && tilt0 == 0 && tiltSpan == 60)
        for (x, pan) in [(0.5, 0.0), (0.75, 90.0), (0.25, -90.0), (0.0, -180.0)] {
            for (y, tilt) in [(0.0, 0.0), (0.5, 30.0), (1.0, 60.0)] {
                let dir = d.direction(atTile: CGPoint(x: x, y: y))!
                #expect(close(normalized(dir), viewerDirection(pan: pan, tilt: tilt)))
            }
        }
        // The top edge is the rim (the horizon of a 180° lens), the centre column points up the picture.
        #expect(close(d.picturePoint(atTile: CGPoint(x: 0.5, y: 0)), CGPoint(x: 0.5, y: 0)))
        // Tall tiles stop at 90° (straight down), short ones at 30°.
        let tall = FisheyeView.views(mode: .panorama, mount: .ceiling, aim: .default(for: .ceiling), size: CGSize(width: 100, height: 100))
        let short = FisheyeView.views(mode: .panorama, mount: .ceiling, aim: .default(for: .ceiling), size: CGSize(width: 1000, height: 10))
        #expect(tall[0].projection == .cylindrical(pan0: -180, panSpan: 360, tilt0: 0, tiltSpan: 90))
        #expect(short[0].projection == .cylindrical(pan0: -180, panSpan: 360, tilt0: 0, tiltSpan: 30))
        // Desk: from above the horizon down to it; a pan turns the strip.
        let desk = FisheyeView.views(
            mode: .panorama, mount: .desk, aim: FisheyeAim(pan: 30, tilt: 0, fov: 90), size: size)
        #expect(desk[0].projection == .cylindrical(pan0: -150, panSpan: 360, tilt0: -60, tiltSpan: 60))
        // Wall: 180° across, centred on the horizon.
        let wall = FisheyeView.views(mode: .panorama, mount: .wall, aim: .default(for: .wall), size: CGSize(width: 360, height: 180))
        #expect(wall[0].projection == .cylindrical(pan0: -90, panSpan: 180, tilt0: -45, tiltSpan: 90))
    }

    @Test func doublePanoramaStacksTwoHalves() {
        let d = FisheyeDewarp(
            lens: lens, mount: .ceiling, mode: .doublePanorama, aim: FisheyeAim(pan: 10, tilt: 45, fov: 90),
            tileSize: CGSize(width: 720, height: 480))
        #expect(d.views.map(\.rect) == [CGRect(x: 0, y: 0, width: 1, height: 0.5), CGRect(x: 0, y: 0.5, width: 1, height: 0.5)])
        // 180° over 720 px with 240 px each: Δτ = 60°.
        #expect(close(normalized(d.direction(atTile: CGPoint(x: 0, y: 0))!), viewerDirection(pan: -170, tilt: 0)))
        #expect(close(normalized(d.direction(atTile: CGPoint(x: 1, y: 0.25))!), viewerDirection(pan: 10, tilt: 30)))
        #expect(close(normalized(d.direction(atTile: CGPoint(x: 0.5, y: 0.75))!), viewerDirection(pan: 100, tilt: 30)))
        // A wall camera shows the single 180° panorama instead.
        let wall = FisheyeView.views(mode: .doublePanorama, mount: .wall, aim: .default(for: .wall), size: CGSize(width: 720, height: 480))
        #expect(wall == FisheyeView.views(mode: .panorama, mount: .wall, aim: .default(for: .wall), size: CGSize(width: 720, height: 480)))
    }

    @Test func quadLooksFourWays() {
        let size = CGSize(width: 800, height: 600)
        let d = FisheyeDewarp(lens: lens, mount: .ceiling, mode: .quad, aim: .default(for: .ceiling), tileSize: size)
        #expect(d.views.count == 4)
        for (i, (center, pan)) in [(CGPoint(x: 0.25, y: 0.25), 0.0), (CGPoint(x: 0.75, y: 0.25), 90.0),
            (CGPoint(x: 0.25, y: 0.75), -180.0), (CGPoint(x: 0.75, y: 0.75), -90.0)].enumerated()
        {
            let dir = normalized(d.direction(atTile: center)!)
            #expect(close(dir, viewerDirection(pan: pan, tilt: 45)), "view \(i)")
        }
        // Each view spans 90° across: its left and right edges are 45° off its center.
        let left = normalized(d.direction(atTile: CGPoint(x: 0, y: 0.25))!)
        let right = normalized(d.direction(atTile: CGPoint(x: 0.5, y: 0.25))!)
        #expect(close(acos((left * right).sum()), .pi / 2, 1e-9))
    }

    @Test func virtualPTZIsARectilinearCamera() {
        let aim = FisheyeAim(pan: 30, tilt: 40, fov: 60)
        let size = CGSize(width: 640, height: 480)
        let d = FisheyeDewarp(lens: lens, mount: .ceiling, mode: .ptz, aim: aim, tileSize: size)
        #expect(close(normalized(d.direction(atTile: CGPoint(x: 0.5, y: 0.5))!), viewerDirection(pan: 30, tilt: 40)))
        // The horizontal field of view is 60°, the vertical one follows the aspect.
        let l = normalized(d.direction(atTile: CGPoint(x: 0, y: 0.5))!)
        let r = normalized(d.direction(atTile: CGPoint(x: 1, y: 0.5))!)
        #expect(close(acos((l * r).sum()), radians(60), 1e-9))
        let t = normalized(d.direction(atTile: CGPoint(x: 0.5, y: 0))!)
        let b = normalized(d.direction(atTile: CGPoint(x: 0.5, y: 1))!)
        #expect(close(acos((t * b).sum()), 2 * atan(tan(radians(30)) * 0.75), 1e-9))
        // Straight lines stay straight: points along a row of the view lie in one plane through the eye.
        let row = [0.0, 0.3, 0.7, 1.0].map { d.direction(atTile: CGPoint(x: $0, y: 0.2))! }
        let n = cross3(row[0], row[3])
        for p in row { #expect(abs((normalized(n) * normalized(p)).sum()) < 1e-9) }
        // The top of the view is further from the floor's center (looking up).
        let top = panTilt(of: t), bottom = panTilt(of: b)
        #expect(top.tilt < 40 && bottom.tilt > 40)
    }

    @Test func aimsAreKeptInRange() {
        #expect(FisheyeAim(pan: 190, tilt: 120, fov: 200).clamped(for: .ceiling) == FisheyeAim(pan: -170, tilt: 90, fov: 120))
        #expect(FisheyeAim(pan: -540, tilt: -5, fov: 5).clamped(for: .ceiling) == FisheyeAim(pan: -180, tilt: 0, fov: 20))
        #expect(FisheyeAim(pan: 10, tilt: 10, fov: 90).clamped(for: .desk) == FisheyeAim(pan: 10, tilt: 0, fov: 90))
        #expect(FisheyeAim(pan: 150, tilt: -85, fov: 90).clamped(for: .wall) == FisheyeAim(pan: 90, tilt: -80, fov: 90))
        #expect(FisheyeAim(pan: .nan, tilt: .infinity, fov: .nan).clamped(for: .wall) == FisheyeAim(pan: 0, tilt: 0, fov: 90))
    }

    @Test func draggingFollowsThePointer() {
        let aim = FisheyeAim.default(for: .ceiling)
        let view = CGSize(width: 900, height: 600)
        // A drag across the whole width turns a PTZ view by its field of view, the other way.
        let right = aim.dragged(by: CGVector(dx: 450, dy: 0), viewSize: view, mode: .ptz, mount: .ceiling)
        #expect(close(right.pan, -45) && right.tilt == 45)
        // Dragging down looks up (toward the horizon).
        let down = aim.dragged(by: CGVector(dx: 0, dy: 300), viewSize: view, mode: .ptz, mount: .ceiling)
        let vfov = degrees(2 * atan(tan(radians(45)) * 600 / 900))
        #expect(close(down.tilt, 45 - vfov / 2))
        // A panorama turns by its 360° per width; Quad by 90° per half width.
        #expect(close(aim.dragged(by: CGVector(dx: 90, dy: 50), viewSize: view, mode: .panorama, mount: .ceiling).pan, -36))
        #expect(close(aim.dragged(by: CGVector(dx: 90, dy: 0), viewSize: view, mode: .doublePanorama, mount: .ceiling).pan, -18))
        #expect(close(aim.dragged(by: CGVector(dx: 45, dy: 0), viewSize: view, mode: .quad, mount: .ceiling).pan, -9))
        #expect(aim.dragged(by: CGVector(dx: 45, dy: 0), viewSize: view, mode: .original, mount: .ceiling) == aim)
        // Zoom narrows the field of view, within its range.
        #expect(aim.zoomed(by: 2, mount: .ceiling).fov == 45)
        #expect(aim.zoomed(by: 100, mount: .ceiling).fov == FisheyeAim.minFOV)
        #expect(aim.zoomed(by: 0.1, mount: .ceiling).fov == FisheyeAim.maxFOV)
    }

    @Test func zoomEasingRunsOnTheClockAndEndsOnTheTarget() {
        var easing = FisheyeZoomEasing<Int>()
        var fov = 90.0
        easing.set(fov: 45, for: 1, now: 10)
        #expect(easing.isEasing)
        // A few refreshes at 60 Hz move part of the way, monotonically.
        var last = fov
        for i in 1...3 {
            for step in easing.tick(now: 10 + Double(i) / 60, current: { _ in fov }) { fov = step.fov }
            #expect(fov < last && fov > 45)
            last = fov
        }
        // One late refresh (a window behind others) finishes it exactly.
        for step in easing.tick(now: 12, current: { _ in fov }) { fov = step.fov }
        #expect(fov == 45 && !easing.isEasing)
        // Targets are kept within the field of view's range.
        easing.set(fov: 500, for: 2, now: 20)
        #expect(easing.target(for: 2) == FisheyeAim.maxFOV)
    }

    @Test func constantsDescribeTheSameMappingAsTheReference() {
        let lens = FisheyeLens(circle: FisheyeCircle(cx: 0.49, cy: 0.505, r: 0.46))
        let size = CGSize(width: 960, height: 540)
        for mount in FisheyeMount.allCases {
            for mode in FisheyeViewMode.allCases where mode.dewarps {
                let d = FisheyeDewarp(
                    lens: lens, mount: mount, mode: mode, aim: FisheyeAim(pan: 20, tilt: mount == .desk ? -30 : 30, fov: 70),
                    tileSize: size)
                let c = d.constants
                #expect(Int(c.params.y) == d.views.count)
                for x in stride(from: 0.01, through: 0.99, by: 0.07) {
                    for y in stride(from: 0.01, through: 0.99, by: 0.07) {
                        let p = CGPoint(x: x, y: y)
                        let want = d.picturePoint(atTile: p)
                        let got = FisheyeDewarp.shade(c, tile: p)
                        #expect(close(got, want, 1e-4), "\(mount) \(mode) at \(x),\(y)")
                    }
                }
            }
        }
    }

    @Test func shaderConstantsLayout() {
        // 22 rows of 16 bytes, as the Metal and HLSL declarations expect
        // (docs/RENDERING.md, "Shader constants").
        #expect(MemoryLayout<DewarpConstants>.size == 352)
        #expect(MemoryLayout<DewarpConstants>.stride == 352)
        #expect(MemoryLayout<DewarpConstants.View>.size == 80)
        #expect(MemoryLayout<DewarpConstants>.offset(of: \.params) == 16)
        #expect(MemoryLayout<DewarpConstants>.offset(of: \.view0) == 32)
        #expect(MemoryLayout<DewarpConstants>.offset(of: \.view3) == 32 + 3 * 80)
        #expect(MemoryLayout<DewarpConstants.View>.offset(of: \.axisY) == 32)
        #expect(MemoryLayout<DewarpConstants.View>.offset(of: \.extra) == 64)
    }
}

private func cross3(_ a: SIMD3<Double>, _ b: SIMD3<Double>) -> SIMD3<Double> {
    SIMD3(a.y * b.z - a.z * b.y, a.z * b.x - a.x * b.z, a.x * b.y - a.y * b.x)
}

private func radians(_ d: Double) -> Double { d * .pi / 180 }
private func degrees(_ r: Double) -> Double { r * 180 / .pi }

@Suite struct FisheyeSettingTests {
    @Test func savedWithTheLeafAndRoundTrips() throws {
        let camera = CameraRef(device: "d-1", channel: 3)
        let tree = LayoutNode.leaf(camera, extras: NodeExtras())
        #expect(tree.fisheye == .original)
        let ptz = FisheyeTileSetting(mode: .ptz, aim: FisheyeAim(pan: 12.5, tilt: 45, fov: 70))
        let set = try #require(tree.settingFisheye(ptz, at: []))
        #expect(set.fisheye == ptz)
        // Through JSON, as the layouts store keeps it.
        let data = try JSONEncoder().encode(set)
        let json = try #require(String(data: data, encoding: .utf8))
        #expect(json.contains("\"fisheye\""))
        #expect(json.contains("\"view\":\"ptz\""))
        let back = try JSONDecoder().decode(LayoutNode.self, from: data)
        #expect(back.fisheye == ptz)
        // Original without a moved view removes the member.
        let cleared = try #require(set.settingFisheye(.original, at: []))
        #expect(!String(decoding: try JSONEncoder().encode(cleared), as: UTF8.self).contains("fisheye"))
        // Unknown views read as Original; quality and fisheye live side by side.
        let other = try JSONDecoder().decode(
            LayoutNode.self,
            from: Data(#"{"camera":{"device":"d-1","channel":3},"quality":"clear","fisheye":{"view":"sphere"}}"#.utf8))
        #expect(other.fisheye.mode == .original && other.quality == .clear)
        // A split is never a fisheye tile.
        #expect(tree.settingFisheye(ptz, at: [.a]) == nil)
    }

    @Test func aimDefaultsFollowTheMount() {
        let s = FisheyeTileSetting(mode: .ptz)
        #expect(s.aim(for: .ceiling) == FisheyeAim(pan: 0, tilt: 45, fov: 90))
        #expect(s.aim(for: .desk) == FisheyeAim(pan: 0, tilt: -45, fov: 90))
        // A saved aim from a ceiling camera is kept in range after a mount change.
        let moved = FisheyeTileSetting(mode: .ptz, aim: FisheyeAim(pan: 10, tilt: 60, fov: 50))
        #expect(moved.aim(for: .desk) == FisheyeAim(pan: 10, tilt: 0, fov: 50))
    }
}

/// A synthetic fisheye picture: a circle of a checkerboard pattern on a
/// black border, with bright "overlay text" blocks in two corners.
struct SyntheticFisheye {
    let width: Int
    let height: Int
    let circle: FisheyeCircle
    var black: UInt8 = 16
    var overlay = true

    func luma(_ x: Int, _ y: Int) -> UInt8 {
        let w = Double(width), h = Double(height)
        let px = Double(x) + 0.5, py = Double(y) + 0.5
        let dx = px - circle.cx * w, dy = py - circle.cy * h
        let r = circle.r * w
        if dx * dx + dy * dy <= r * r {
            // Dark and bright cells, a soft fall-off near the rim like a real lens.
            let cell = (Int(px / 12) + Int(py / 12)) % 2 == 0
            let edge = min(1, (r - (dx * dx + dy * dy).squareRoot()) / (0.02 * r) + 0.35)
            return UInt8(Double(black) + (cell ? 170 : 60) * edge)
        }
        if overlay {
            // Text-like bars bottom left and bottom right, outside the circle.
            let bar = (x / 3) % 2 == 0
            if y > height - height / 20, y < height - height / 60, bar,
                x < width / 4 || x > width * 3 / 4
            {
                return 235
            }
        }
        return black
    }
}

@Suite struct FisheyeCircleTests {
    private func detect(_ s: SyntheticFisheye) -> FisheyeCircle? {
        FisheyeCircleDetector.detect(width: s.width, height: s.height, luma: s.luma)
    }

    @Test func findsOffCenterCirclesWithinOnePercent() throws {
        for (w, h, c) in [
            (384, 384, FisheyeCircle(cx: 0.49, cy: 0.505, r: 0.46)),
            (1024, 1024, FisheyeCircle(cx: 0.501, cy: 0.502, r: 0.471)),
            (640, 480, FisheyeCircle(cx: 0.52, cy: 0.48, r: 0.33)),
            (800, 800, FisheyeCircle(cx: 0.5, cy: 0.5, r: 0.40)),
        ] {
            let found = try #require(detect(SyntheticFisheye(width: w, height: h, circle: c)), "\(w)x\(h)")
            #expect(abs(found.cx - c.cx) < 0.01 && abs(found.cy - c.cy) < 0.01 && abs(found.r - c.r) < 0.01,
                "\(w)x\(h): \(found) vs \(c)")
        }
    }

    @Test func fullRangeAndNoOverlayWorkToo() throws {
        var s = SyntheticFisheye(width: 512, height: 512, circle: FisheyeCircle(cx: 0.5, cy: 0.5, r: 0.45))
        s.black = 0
        s.overlay = false
        let found = try #require(detect(s))
        #expect(abs(found.r - 0.45) < 0.01)
    }

    @Test func noCircleIsNoAnswer() {
        // All black (night, a lens cap), or a picture filled edge to edge.
        #expect(FisheyeCircleDetector.detect(width: 256, height: 256) { _, _ in 16 } == nil)
        #expect(FisheyeCircleDetector.detect(width: 256, height: 256) { x, y in UInt8(60 + (x ^ y) % 120) } == nil)
        #expect(FisheyeCircleDetector.detect(width: 8, height: 8) { _, _ in 200 } == nil)
    }

    @Test func trackerTriesOnTheClockThenFallsBack() {
        var t = FisheyeCircleTracker()
        #expect(t.wantsTry(now: 0))
        t.record(nil, now: 0, width: 640, height: 480)
        #expect(!t.wantsTry(now: 0.2) && t.wantsTry(now: 0.5))
        for i in 1..<FisheyeCircleTracker.maxTries {
            #expect(t.circle == nil)
            t.record(nil, now: Double(i), width: 640, height: 480)
        }
        #expect(t.fellBack && t.circle == FisheyeCircle.inscribed(width: 640, height: 480))
        #expect(t.circle == FisheyeCircle(cx: 0.5, cy: 0.5, r: 0.375))
        #expect(!t.wantsTry(now: 100))
        var found = FisheyeCircleTracker()
        found.record(FisheyeCircle(cx: 0.5, cy: 0.5, r: 0.4), now: 0, width: 384, height: 384)
        #expect(found.circle?.r == 0.4 && !found.fellBack && !found.wantsTry(now: 10))
    }
}
