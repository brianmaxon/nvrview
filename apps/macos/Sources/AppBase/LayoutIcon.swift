import AppKit
import NVRCore
import SwiftUI

/// A layout's shape as a small monochrome picture: each tile a rounded
/// rectangle, a gap between them. A template image, so the system tints it
/// for dark mode, selection, and menus. The geometry is
/// `LayoutNode.iconRects` (tested); this only draws it.
package enum LayoutIcon {
    package static let size = NSSize(width: 18, height: 12)

    package static func image(for tree: LayoutNode) -> NSImage {
        let rects = tree.iconRects(size: size, gap: tree.leafCount > 16 ? 0.75 : 1.5)
        let image = NSImage(size: size, flipped: true) { _ in
            NSColor.black.setFill()
            for r in rects {
                let radius = min(1.5, min(r.width, r.height) / 2)
                NSBezierPath(roundedRect: r, xRadius: radius, yRadius: radius).fill()
            }
            return true
        }
        image.isTemplate = true
        return image
    }

    /// The icon for `count` cameras in an even grid (groups, All Cameras).
    package static func image(gridOf count: Int) -> NSImage {
        image(for: LayoutNode.balanced(Array(repeating: nil, count: max(count, 1))))
    }
}

/// The icon in a sidebar row, in the row's secondary tint.
package struct LayoutIconView: View {
    package let image: NSImage

    package var body: some View {
        Image(nsImage: image)
            .renderingMode(.template)
            .foregroundStyle(.secondary)
            .accessibilityHidden(true)
    }

    package init(image: NSImage) {
        self.image = image
    }
}
