import AppKit
import NVRCore

// What the tile header's items do before capture and camera motion exist
// (docs/CAPTURE.md). Their buttons, menu items and keys are all in place,
// in the header model's order and wording, so the layout is the final one;
// an action that is not built yet is drawn dimmed and says "coming soon"
// in a short message over the tile instead of doing something.

/// The header items whose action lands later.
enum TileActionStub {
    /// Save picture, recording, and PTZ: not built yet.
    static func isPending(_ item: TileHeaderItem) -> Bool {
        switch item {
        case .record, .savePicture, .panTilt, .presets, .lens, .savePictureAs, .saveZoomedArea,
            .saveOriginalFisheye, .recordMain, .recordSub:
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

/// A short message over a stage ("Save Picture is coming soon"), gone
/// after a few seconds on the stage's clock.
@MainActor
final class ToastView: NSView {
    static let duration: Double = 2.5
    private let label = NSTextField(labelWithString: "")

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
        isHidden = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("not used") }

    var text: String? { isHidden ? nil : label.stringValue }

    func show(_ text: String) {
        label.stringValue = text
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
        let width = min(max(ceil(label.intrinsicContentSize.width) + 28, 120), max(bounds.width - 24, 60))
        frame = CGRect(x: (bounds.width - width) / 2, y: 22, width: width, height: 30)
        label.frame = CGRect(x: 10, y: 7, width: width - 20, height: 16)
    }

    // Clicks go through to the stage.
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}
