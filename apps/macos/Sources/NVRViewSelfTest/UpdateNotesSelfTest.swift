@testable import NVRViewApp
import AppKit
import Foundation
import NVRCore
import AppUpdates

/// Self-test step `updates-notes-scroll`: the release notes dialog, given the
/// longest allowed synthetic notes (2000 characters of headings and bullets),
/// shows many lines at once (not three), scrolls the rest, is resizable with
/// the notes area growing with the window, keeps its buttons inside the
/// window at the minimum and a large size, styles headings and bullets, and
/// remembers its size. Short notes get a shorter window. The dialog is laid
/// out but never shown on screen.
@MainActor
enum UpdateNotesSelfTest {
    static func run() -> [String] {
        var problems: [String] = []
        let suite = "nvrview-notes-selftest-\(UUID().uuidString)"
        guard let defaults = UserDefaults(suiteName: suite), let shortDefaults = UserDefaults(suiteName: suite + "-short") else {
            return ["updates: no throwaway defaults"]
        }
        defer {
            defaults.removePersistentDomain(forName: suite)
            shortDefaults.removePersistentDomain(forName: suite + "-short")
        }

        var block = ""
        var n = 0
        while block.count < 2000 {
            n += 1
            block += "Section \(n)\n- first made-up point of section \(n)\n"
            block += "- second made-up point, long enough that it wraps onto another line in a narrow window\n\n"
        }
        let long = ReleaseNotesText.format(String(block.prefix(2000)))
        let dialog = NotesDialog(
            title: "NVRView 9.9.9 is available", line: "You have 9.9.8.", notes: long, buttonTitles: ["Download", "Not Now"],
            defaults: defaults)
        guard let scroll = dialog.scroll, let text = scroll.documentView as? NSTextView, let content = dialog.window.contentView else {
            return ["updates: the notes view has no text view"]
        }
        content.layoutSubtreeIfNeeded()

        // The text: all of it, selectable, read-only, scrollable.
        if text.string.replacingOccurrences(of: "\t", with: " ") != long { problems.append("updates: the notes view does not hold all the notes") }
        if !text.isSelectable || text.isEditable { problems.append("updates: the notes are not selectable and read-only") }
        if !scroll.hasVerticalScroller { problems.append("updates: the notes view cannot scroll") }

        // A useful amount at once, and more text than that.
        let lines = UpdateNotesView.visibleLines(scroll)
        if lines < 15 { problems.append("updates: the notes view shows only \(lines) lines at once") }
        if UpdateNotesView.contentHeight(text, width: scroll.frame.width) <= scroll.frame.height {
            problems.append("updates: 2000 characters do not overflow the notes view")
        }

        // Resizable, and the notes area follows the window; the buttons stay inside, below the notes.
        if !dialog.window.styleMask.contains(.resizable) { problems.append("updates: the dialog cannot be resized") }
        func buttonsVisible() -> Bool {
            dialog.buttons.allSatisfy { content.bounds.contains($0.convert($0.bounds, to: content)) }
                && dialog.buttonRow.frame.maxY <= scroll.frame.minY + 0.5
        }
        if !buttonsVisible() { problems.append("updates: the buttons are not below the notes at the default size") }
        let before = scroll.frame.height
        dialog.window.setContentSize(NSSize(width: content.frame.width + 200, height: content.frame.height + 240))
        content.layoutSubtreeIfNeeded()
        if scroll.frame.height < before + 200 || !buttonsVisible() {
            problems.append("updates: the notes area did not grow with the window")
        }
        dialog.window.setContentSize(NotesDialog.minSize)
        content.layoutSubtreeIfNeeded()
        if !buttonsVisible() || scroll.frame.height < 100 {
            problems.append("updates: the buttons or the notes are lost at the smallest size")
        }

        // The size is remembered.
        dialog.window.setContentSize(NSSize(width: 600, height: 500))
        dialog.remember()
        let again = NotesDialog(title: "t", line: nil, notes: long, buttonTitles: ["Close"], defaults: defaults)
        if again.savedSize() != NSSize(width: 600, height: 500) { problems.append("updates: the dialog size was not remembered") }

        // Headings are bold and bullets hang.
        if let storage = text.textStorage {
            let heading = (storage.string as NSString).range(of: "Section 1")
            if let f = storage.attribute(.font, at: heading.location, effectiveRange: nil) as? NSFont,
                !f.fontDescriptor.symbolicTraits.contains(.bold)
            {
                problems.append("updates: a heading is not bold")
            }
            let bullet = (storage.string as NSString).range(of: "\u{2022}")
            if let p = storage.attribute(.paragraphStyle, at: bullet.location, effectiveRange: nil) as? NSParagraphStyle,
                p.headIndent <= p.firstLineHeadIndent
            {
                problems.append("updates: a bullet does not hang")
            }
        }

        // Short notes get a shorter window than long ones.
        let short = NotesDialog(title: "t", line: nil, notes: "One line.", buttonTitles: ["Close"], defaults: shortDefaults)
        if short.window.contentView?.frame.height ?? 0 >= NotesDialog.defaultSize(line: nil, scroll: scroll).height {
            problems.append("updates: short notes take the full height")
        }
        return problems
    }
}
