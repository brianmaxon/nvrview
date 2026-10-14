import NVRCore

/// What a window shows.
package enum WindowContent: Equatable {
    /// A layout the core stores; edits are saved back to it.
    case layout(id: String)
    /// A group's cameras as an even grid; edits stay in the window until
    /// saved as a layout.
    case group(id: String)
    /// An unsaved arrangement (the first window before any layout exists).
    case scratch(name: String, root: LayoutNode)
}
