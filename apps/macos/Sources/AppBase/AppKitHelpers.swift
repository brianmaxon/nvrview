import AppKit

/// A plain view with a top-left origin.
@MainActor
open class FlippedView: NSView {
    override open var isFlipped: Bool { true }
}

/// A menu item that runs a closure.
package final class ClosureMenuItem: NSMenuItem {
    private let run: () -> Void

    package init(title: String, action: @escaping () -> Void) {
        run = action
        super.init(title: title, action: #selector(fire), keyEquivalent: "")
        target = self
    }

    @available(*, unavailable)
    package required init(coder: NSCoder) { fatalError("not used") }

    @objc private func fire() { run() }
}

/// A transparent view whose own area lets clicks through to what is below;
/// only its subviews take them.
@MainActor
open class PassThroughView: NSView {
    override open var isFlipped: Bool { true }
    override open func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        return hit === self ? nil : hit
    }
}
