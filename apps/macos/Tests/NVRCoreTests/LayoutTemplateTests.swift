import CoreGraphics
import Foundation
import NVRCore
import Testing

/// Layout templates and icon geometry. Pure; no drawing.
@Suite struct LayoutTemplateTests {
    private func cam(_ i: Int) -> CameraRef { CameraRef(device: "dev-a", channel: i) }

    @Test(arguments: LayoutTemplate.allCases)
    func eachTemplateHasItsTileCount(_ t: LayoutTemplate) {
        #expect(t.shape.leafCount == t.tileCount, "\(t)")
        #expect(t.title.hasPrefix("\(t.tileCount) "), "\(t)")
        #expect(t.shape.depth <= LayoutNode.maxDepth)
        #expect(t.shape.leafCount <= LayoutNode.maxLeaves)
    }

    @Test(arguments: LayoutTemplate.allCases)
    func theTilesCoverTheUnitSquareWithoutOverlap(_ t: LayoutTemplate) {
        let rects = t.shape.unitRects
        #expect(rects.count == t.tileCount)
        let area = rects.reduce(0) { $0 + $1.width * $1.height }
        #expect(abs(area - 1) < 1e-9, "\(t) area \(area)")
        let unit = CGRect(x: 0, y: 0, width: 1, height: 1).insetBy(dx: -1e-9, dy: -1e-9)
        for r in rects { #expect(unit.contains(r), "\(t) \(r)") }
        for (i, a) in rects.enumerated() {
            for b in rects[(i + 1)...] {
                let i = a.intersection(b)
                #expect(i.isNull || i.width * i.height < 1e-9, "\(t) \(a) \(b)")
            }
        }
        // Every ratio is one the layout model accepts.
        func ratios(_ n: LayoutNode) -> [Double] {
            if case .split(_, let r, let a, let b, _) = n { return [r] + ratios(a) + ratios(b) }
            return []
        }
        for r in ratios(t.shape) { #expect(r >= LayoutNode.minRatio && r <= LayoutNode.maxRatio, "\(t) \(r)") }
    }

    @Test func theTemplatesAreStorableLayouts() throws {
        for t in LayoutTemplate.allCases {
            let layout = SavedLayout(name: t.title, root: t.tree(cameras: (0..<5).map(cam)))
            let back = try #require(SavedLayout.list(fromJSON: "[\(layout.json)]").first)
            #expect(back.root == layout.root, "\(t)")
        }
    }

    @Test func theLargeTilesAreTheSizeTheyAreNamedFor() {
        func big(_ t: LayoutTemplate) -> CGRect { t.shape.unitRects[0] }
        #expect(abs(big(.big1Small5).width - 2.0 / 3) < 1e-9 && abs(big(.big1Small5).height - 2.0 / 3) < 1e-9)
        #expect(abs(big(.big1Small7).width - 0.75) < 1e-9 && abs(big(.big1Small7).height - 0.75) < 1e-9)
        #expect(abs(big(.big1Small12).width - 0.5) < 1e-9 && abs(big(.big1Small12).height - 0.5) < 1e-9)
        // Two large tiles across the top half.
        #expect(LayoutTemplate.big2Small8.shape.unitRects[1] == CGRect(x: 0.5, y: 0, width: 0.5, height: 0.5))
        // 7 x 5 units: the large tile is 2 x 2 of them.
        let b = big(.big1Small31)
        #expect(abs(b.width - 2.0 / 7) < 1e-9 && abs(b.height - 2.0 / 5) < 1e-9)
        // The even grids are even.
        for t in [LayoutTemplate.grid4, .grid9, .grid16, .grid25, .grid36] {
            let n = Double(t.tileCount).squareRoot()
            for r in t.shape.unitRects { #expect(abs(r.width - 1 / n) < 1e-9 && abs(r.height - 1 / n) < 1e-9, "\(t)") }
        }
    }

    @Test func camerasFillInReadingOrderAndTheRestStayEmpty() {
        let few = LayoutTemplate.grid4.tree(cameras: [cam(0), cam(1)])
        #expect(few.cameraList == [cam(0), cam(1), nil, nil])
        let many = LayoutTemplate.sideBySide.tree(cameras: (0..<5).map(cam))
        #expect(many.cameraList == [cam(0), cam(1)])
        #expect(LayoutTemplate.single.tree().cameraList == [nil])
    }

    @Test func templatesAreOrderedBySize() {
        let counts = LayoutTemplate.ordered.map(\.tileCount)
        #expect(counts == counts.sorted())
        #expect(counts.count == LayoutTemplate.allCases.count)
    }

    // MARK: Icons

    @Test func iconTilesStayInsideWithAGapBetweenThem() {
        let size = CGSize(width: 18, height: 12)
        for t in LayoutTemplate.allCases {
            let rects = t.shape.iconRects(size: size, gap: 1)
            #expect(rects.count == t.tileCount)
            let bounds = CGRect(origin: .zero, size: size)
            for r in rects { #expect(bounds.contains(r) && r.width >= 1 && r.height >= 1, "\(t) \(r)") }
            for (i, a) in rects.enumerated() {
                for b in rects[(i + 1)...] { #expect(!a.intersects(b), "\(t) \(a) \(b)") }
            }
        }
        // Two tiles side by side are one gap apart.
        let two = LayoutTemplate.sideBySide.shape.iconRects(size: size, gap: 2)
        #expect(abs((two[1].minX - two[0].maxX) - 2) < 1e-9)
    }

    @Test func aSavedLayoutDrawsFromItsOwnTree() {
        let tree = LayoutNode.split(.row, ratio: 0.25, a: .leaf(cam(0)), b: .split(.col, ratio: 0.5, a: .empty, b: .empty))
        let rects = tree.unitRects
        #expect(rects == [
            CGRect(x: 0, y: 0, width: 0.25, height: 1),
            CGRect(x: 0.25, y: 0, width: 0.75, height: 0.5),
            CGRect(x: 0.25, y: 0.5, width: 0.75, height: 0.5),
        ])
        // A group or All Cameras icon is the balanced grid for its count.
        #expect(LayoutNode.balanced((0..<4).map(cam)).unitRects.count == 4)
        #expect(LayoutNode.balanced([]).unitRects == [CGRect(x: 0, y: 0, width: 1, height: 1)])
    }
}
