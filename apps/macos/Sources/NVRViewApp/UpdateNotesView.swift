import AppKit
import NVRCore

/// The release notes (docs/UX.md, "Updates" and "Release notes"): a
/// scrollable, selectable, wrapping text view with styled headings and
/// indented bullets, shown inside `NotesDialog`, a resizable sheet whose notes
/// area grows with the window. Self-test step `updates-notes-scroll`.
@MainActor
enum UpdateNotesView {
    /// The notes view's size when the window opens: about 22 lines at a
    /// readable width.
    static let width: CGFloat = 480
    static let maxHeight: CGFloat = 340
    static let font = NSFont.systemFont(ofSize: NSFont.systemFontSize)

    /// The notes as styled text: headings bold and set apart, bullets with a
    /// hanging indent. Plain text in; the characters are the notes, except
    /// that a bullet's space becomes a tab.
    static func attributed(_ notes: String) -> NSAttributedString {
        let out = NSMutableAttributedString()
        let lines = notes.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        let kinds = ReleaseNotesText.lineKinds(notes)
        for (i, line) in lines.enumerated() {
            let style = NSMutableParagraphStyle()
            var attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: NSColor.labelColor]
            var shown = line
            switch kinds[i] {
            case .heading:
                attributes[.font] = NSFont.boldSystemFont(ofSize: NSFont.systemFontSize + 1)
                style.paragraphSpacingBefore = i == 0 ? 0 : 6
                style.paragraphSpacing = 2
            case .bullet(let level):
                let indent = CGFloat(level) * 16 + 8
                style.firstLineHeadIndent = indent
                style.headIndent = indent + 14
                style.tabStops = [NSTextTab(textAlignment: .left, location: indent + 14)]
                // "•<tab>text" so wrapped lines hang under the text.
                shown = line.drop { $0 == " " }.replacingOccurrences(of: "\u{2022} ", with: "\u{2022}\t")
            case .blank, .text:
                break
            }
            attributes[.paragraphStyle] = style
            out.append(NSAttributedString(string: shown + (i < lines.count - 1 ? "\n" : ""), attributes: attributes))
        }
        return out
    }

    /// A scroll view of the notes that follows its container's size.
    static func make(notes: String, width: CGFloat = width, height: CGFloat = maxHeight) -> NSScrollView {
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: width, height: height))
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder
        scroll.autoresizingMask = [.width, .height]
        let text = NSTextView(frame: NSRect(x: 0, y: 0, width: width, height: height))
        text.isEditable = false
        text.isSelectable = true
        text.isRichText = false
        text.drawsBackground = false
        text.textContainerInset = NSSize(width: 6, height: 6)
        text.isVerticallyResizable = true
        text.isHorizontallyResizable = false
        text.autoresizingMask = [.width]
        text.textContainer?.widthTracksTextView = true
        text.textStorage?.setAttributedString(attributed(notes))
        scroll.documentView = text
        return scroll
    }

    /// The height the text needs at `width` (the view's full width), measured
    /// on the styled string so it does not depend on the view's layout state.
    static func contentHeight(_ text: NSTextView, width: CGFloat = width) -> CGFloat {
        guard let storage = text.textStorage else { return maxHeight }
        let inner = width - 2 * text.textContainerInset.width - 4
        let box = storage.boundingRect(with: NSSize(width: inner, height: .greatestFiniteMagnitude), options: [.usesLineFragmentOrigin, .usesFontLeading])
        return ceil(box.height) + 2 * text.textContainerInset.height + 4
    }

    /// How many lines of plain text the view shows at its current height.
    static func visibleLines(_ scroll: NSScrollView) -> Int {
        Int(scroll.contentSize.height / font.boundingRectForFont.height)
    }
}

/// A resizable sheet (or modal window) for release notes and the update
/// prompt: a title, an optional line, the notes area that grows with the
/// window, and a button row that stays at the bottom at every size. The size
/// is remembered between runs.
@MainActor
final class NotesDialog: NSObject {
    static let sizeKey = "NotesDialogSize"
    static let minSize = NSSize(width: 360, height: 260)

    let window: NSWindow
    let scroll: NSScrollView?
    let buttonRow = NSStackView()
    private(set) var buttons: [NSButton] = []
    private let defaults: UserDefaults
    private var completion: ((Int) -> Void)?
    private var parent: NSWindow?
    private var keepAlive: NotesDialog?

    /// `notes` nil makes a dialog with just the title and line. The first
    /// button is the default; a later one named "Not Now", "Later", or
    /// "Close" answers Escape.
    init(title: String, line: String?, notes: String?, buttonTitles: [String], defaults: UserDefaults = .standard) {
        self.defaults = defaults
        let hasNotes = notes != nil
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: UpdateNotesView.width + 40, height: 120),
            styleMask: hasNotes ? [.titled, .resizable] : [.titled], backing: .buffered, defer: true)
        scroll = notes.map { UpdateNotesView.make(notes: $0) }
        super.init()
        let content = NSView()
        window.contentView = content

        let titleLabel = NSTextField(wrappingLabelWithString: title)
        titleLabel.font = .boldSystemFont(ofSize: NSFont.systemFontSize + 2)
        let lineLabel = NSTextField(wrappingLabelWithString: line ?? "")
        lineLabel.isHidden = line == nil
        lineLabel.isSelectable = true
        buttonRow.orientation = .horizontal
        buttonRow.spacing = 8

        for (i, name) in buttonTitles.enumerated() {
            let b = NSButton(title: name, target: self, action: #selector(pressed(_:)))
            b.tag = i
            if i == 0 {
                b.keyEquivalent = "\r"
            } else if ["Not Now", "Later", "Close"].contains(name) {
                b.keyEquivalent = "\u{1b}"
            }
            buttons.append(b)
        }
        if buttonTitles.count == 1 { buttons.first?.keyEquivalent = "\r" }
        // The default button sits at the right.
        for b in buttons.reversed() { buttonRow.addArrangedSubview(b) }

        let views: [NSView] = [titleLabel, lineLabel] + (scroll.map { [$0] } ?? []) + [buttonRow]
        for v in views {
            v.translatesAutoresizingMaskIntoConstraints = false
            content.addSubview(v)
        }
        let m: CGFloat = 20
        var constraints = [
            titleLabel.topAnchor.constraint(equalTo: content.topAnchor, constant: m),
            titleLabel.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: m),
            titleLabel.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -m),
            lineLabel.leadingAnchor.constraint(equalTo: titleLabel.leadingAnchor),
            lineLabel.trailingAnchor.constraint(equalTo: titleLabel.trailingAnchor),
            buttonRow.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -m),
            buttonRow.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -m),
        ]
        if line != nil {
            constraints.append(lineLabel.topAnchor.constraint(equalTo: titleLabel.bottomAnchor, constant: 6))
        }
        let above = line != nil ? lineLabel : titleLabel
        if let scroll {
            constraints += [
                scroll.topAnchor.constraint(equalTo: above.bottomAnchor, constant: 12),
                scroll.leadingAnchor.constraint(equalTo: titleLabel.leadingAnchor),
                scroll.trailingAnchor.constraint(equalTo: titleLabel.trailingAnchor),
                scroll.bottomAnchor.constraint(equalTo: buttonRow.topAnchor, constant: -14),
                scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 100),
            ]
        } else {
            constraints.append(buttonRow.topAnchor.constraint(equalTo: above.bottomAnchor, constant: 16))
        }
        NSLayoutConstraint.activate(constraints)
        // The first layout sizes the window to the content's fitting size; set ours after it.
        content.layoutSubtreeIfNeeded()

        if hasNotes {
            window.contentMinSize = Self.minSize
            let want = savedSize() ?? Self.defaultSize(line: line, scroll: scroll)
            window.setContentSize(want)
        } else {
            window.setContentSize(NSSize(width: 420, height: 140))
        }
        content.layoutSubtreeIfNeeded()
    }

    /// The saved size, if there is one, raised to the minimum.
    func savedSize() -> NSSize? {
        guard let a = defaults.array(forKey: Self.sizeKey) as? [Double], a.count == 2 else { return nil }
        return NSSize(width: max(a[0], Self.minSize.width), height: max(a[1], Self.minSize.height))
    }

    /// Wide enough to read, tall enough for about 22 lines; short notes get a
    /// shorter window.
    static func defaultSize(line: String?, scroll: NSScrollView?) -> NSSize {
        let chrome: CGFloat = 20 + 24 + (line == nil ? 0 : 50) + 12 + 14 + 32 + 20
        var notes = UpdateNotesView.maxHeight
        if let text = scroll?.documentView as? NSTextView {
            notes = min(notes, max(100, UpdateNotesView.contentHeight(text)))
        }
        return NSSize(width: UpdateNotesView.width + 40, height: notes + chrome)
    }

    @objc private func pressed(_ sender: NSButton) { finish(sender.tag) }

    /// Presses button `index` (used by the buttons and the self-test).
    func finish(_ index: Int) {
        remember()
        let done = completion
        completion = nil
        if let parent, window.sheetParent === parent {
            parent.endSheet(window)
        } else if NSApp.modalWindow === window {
            NSApp.stopModal()
        }
        window.orderOut(nil)
        keepAlive = nil
        done?(index)
    }

    /// Stores the content size when the notes area can be resized.
    func remember() {
        guard scroll != nil, let size = window.contentView?.frame.size else { return }
        defaults.set([Double(size.width), Double(size.height)], forKey: Self.sizeKey)
    }

    /// Shows the dialog as a sheet on `parent`, or modally when there is
    /// none. `completion` gets the index of the pressed button.
    func present(on parent: NSWindow?, completion: @escaping (Int) -> Void) {
        self.completion = completion
        self.parent = parent
        keepAlive = self
        if let parent {
            parent.beginSheet(window, completionHandler: nil)
        } else {
            window.center()
            NSApp.runModal(for: window)
        }
    }
}
