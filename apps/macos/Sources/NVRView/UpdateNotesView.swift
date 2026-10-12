import AppKit

/// The release notes in the update prompt (docs/UX.md, "Updates"): a
/// scrollable, selectable, wrapping text view of bounded height, used as the
/// alert's accessory view so the message and the buttons stay in view however
/// long the notes are. Self-test step `updates-notes-scroll`.
@MainActor
enum UpdateNotesView {
    static let width: CGFloat = 380
    static let maxHeight: CGFloat = 220

    static func make(notes: String) -> NSScrollView {
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: width, height: maxHeight))
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder
        let text = NSTextView(frame: NSRect(x: 0, y: 0, width: width, height: maxHeight))
        text.isEditable = false
        text.isSelectable = true
        text.isRichText = false
        text.drawsBackground = false
        text.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        text.textContainerInset = NSSize(width: 4, height: 4)
        text.isVerticallyResizable = true
        text.isHorizontallyResizable = false
        text.autoresizingMask = [.width]
        text.textContainer?.widthTracksTextView = true
        text.string = notes
        scroll.documentView = text
        // Short notes get a short view; long ones stop at the bound and scroll.
        scroll.setFrameSize(NSSize(width: width, height: min(maxHeight, max(40, contentHeight(text)))))
        return scroll
    }

    /// The height the text needs at the view's width.
    static func contentHeight(_ text: NSTextView) -> CGFloat {
        guard let container = text.textContainer, let layout = text.layoutManager else { return maxHeight }
        container.containerSize = NSSize(width: width - 20, height: .greatestFiniteMagnitude)
        layout.ensureLayout(for: container)
        return layout.usedRect(for: container).height + 2 * text.textContainerInset.height + 4
    }
}
