@testable import NVRViewApp
import AppKit

/// Self-test step `updates-notes-scroll`: the update prompt's notes view,
/// given the longest allowed notes (2000 characters), is a scrollable,
/// selectable text view whose height stays bounded while the text needs more,
/// and short notes do not get the full height. The alert itself is not shown
/// (self-test mode).
@MainActor
enum UpdateNotesSelfTest {
    static func run() -> [String] {
        var problems: [String] = []
        let long = String(String(repeating: "notes. ", count: 300).prefix(2000))
        let view = UpdateNotesView.make(notes: long)
        guard let text = view.documentView as? NSTextView else { return ["updates: the notes view has no text view"] }
        if text.string.count != 2000 { problems.append("updates: the notes view does not hold all 2000 characters") }
        if !text.isSelectable || text.isEditable { problems.append("updates: the notes are not selectable and read-only") }
        if !view.hasVerticalScroller { problems.append("updates: the notes view cannot scroll") }
        if view.frame.height > UpdateNotesView.maxHeight { problems.append("updates: the notes view is taller than its bound") }
        if UpdateNotesView.contentHeight(text) <= view.frame.height {
            problems.append("updates: 2000 characters do not overflow the notes view")
        }
        let short = UpdateNotesView.make(notes: "One line.")
        if short.frame.height >= UpdateNotesView.maxHeight { problems.append("updates: short notes take the full height") }
        return problems
    }
}
