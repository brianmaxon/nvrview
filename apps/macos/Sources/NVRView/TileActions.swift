import AppKit
import NVRCore

// What the tile header's items do before capture and camera motion exist
// (docs/CAPTURE.md). Their buttons, menu items and keys are all in place,
// in the header model's order and wording, so the layout is the final one;
// an action that is not built yet is drawn dimmed and says "coming soon"
// in a short message over the tile instead of doing something.

/// The header items whose action lands later.
enum TileActionStub {
    /// PTZ: not built yet (Save picture and recording are built).
    static func isPending(_ item: TileHeaderItem) -> Bool {
        switch item {
        case .panTilt, .presets, .lens:
            true
        default:
            false
        }
    }

    /// "Save Picture is coming soon".
    static func message(_ label: String) -> String {
        let name = label.hasSuffix("…") ? String(label.dropLast()) : label
        return "\(name) is coming soon"
    }
}

/// The macOS key hint of a header menu entry as a menu key equivalent, for
/// the hints that are one key (the others, such as Option with the arrows,
/// are listed in the Camera menu only once they work).
enum MenuKeyHint {
    static func keyEquivalent(_ hint: String) -> (key: String, modifiers: NSEvent.ModifierFlags)? {
        var mods: NSEvent.ModifierFlags = []
        var rest = Substring(hint)
        while let first = rest.first, "⇧⌘⌥⌃".contains(first) {
            switch first {
            case "⇧": mods.insert(.shift)
            case "⌘": mods.insert(.command)
            case "⌥": mods.insert(.option)
            default: mods.insert(.control)
            }
            rest = rest.dropFirst()
        }
        guard rest.count == 1, let c = rest.first else { return nil }
        if c == "↩" { return ("\r", mods) }
        guard c.isLetter || c.isNumber else { return nil }
        return (String(c).lowercased(), mods)
    }

    static func apply(_ hint: String, to item: NSMenuItem) {
        guard let (key, mods) = keyEquivalent(hint) else { return }
        item.keyEquivalent = key
        item.keyEquivalentModifierMask = mods
    }
}

/// A short message over a stage ("Picture saved"), gone after a few seconds
/// on the stage's clock, with an optional button ("Show in Finder").
@MainActor
final class ToastView: NSView {
    static let duration: Double = 2.5
    /// A toast with a button stays longer, to be reached.
    static let actionDuration: Double = 6
    private let label = NSTextField(labelWithString: "")
    private let button = NSButton(title: "", target: nil, action: nil)
    private var handler: (() -> Void)?

    override var isFlipped: Bool { true }

    init() {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = NSColor(calibratedWhite: 0.1, alpha: 0.85).cgColor
        layer?.cornerRadius = 8
        label.font = .systemFont(ofSize: 12, weight: .medium)
        label.textColor = .white
        label.alignment = .center
        label.lineBreakMode = .byTruncatingTail
        addSubview(label)
        button.isBordered = false
        button.target = self
        button.action = #selector(actionPressed)
        button.isHidden = true
        addSubview(button)
        isHidden = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    var text: String? { isHidden ? nil : label.stringValue }
    /// The button's title while one shows (self-test).
    var actionTitle: String? { isHidden || button.isHidden ? nil : button.title }

    func show(_ text: String, action: (title: String, handler: () -> Void)? = nil) {
        label.stringValue = text
        handler = action?.handler
        button.isHidden = action == nil
        if let action {
            button.attributedTitle = NSAttributedString(
                string: action.title,
                attributes: [.foregroundColor: NSColor.systemYellow, .font: NSFont.systemFont(ofSize: 12, weight: .semibold)])
        }
        setAccessibilityLabel(text)
        isHidden = false
        NSAccessibility.post(element: self, notification: .announcementRequested, userInfo: [
            .announcement: text, .priority: NSAccessibilityPriorityLevel.medium.rawValue,
        ])
        superview?.needsLayout = true
    }

    /// Where it goes in a superview `bounds` big (bottom-left origin):
    /// centered near the bottom.
    func place(in bounds: CGRect) {
        let buttonWidth = button.isHidden ? 0 : ceil(button.attributedTitle.size().width) + 14
        let width = min(
            max(ceil(label.intrinsicContentSize.width) + 28 + buttonWidth, 120), max(bounds.width - 24, 60))
        frame = CGRect(x: (bounds.width - width) / 2, y: 22, width: width, height: 30)
        label.frame = CGRect(x: 10, y: 7, width: width - 20 - buttonWidth, height: 16)
        button.frame = CGRect(x: width - buttonWidth - 8, y: 4, width: buttonWidth, height: 22)
    }

    /// Presses the button (self-test).
    func pressAction() { actionPressed() }

    @objc private func actionPressed() {
        let run = handler
        handler = nil
        isHidden = true
        run?()
    }

    // Clicks go through to the stage, except on the button.
    override func hitTest(_ point: NSPoint) -> NSView? {
        guard !isHidden, !button.isHidden else { return nil }
        let local = convert(point, from: superview)
        return button.frame.contains(local) ? button : nil
    }
}
