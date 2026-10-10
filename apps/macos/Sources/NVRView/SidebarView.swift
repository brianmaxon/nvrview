import AppKit
import NVRCore
import SwiftUI

/// A window's sidebar: layouts, groups, and devices with their cameras
/// (docs/UX.md, the approved mockups). Clicking a layout or group shows it in
/// the window; clicking a camera puts it in the selected tile; cameras can be
/// dragged onto any tile.
struct SidebarView: View {
    let model: AppModel
    weak var window: ViewerWindowController?
    let state: SidebarState
    @State private var collapsedGroups: Set<String> = []

    /// A row of the Devices or Groups section.
    enum Item: Hashable, Identifiable {
        case device(String)
        case group(String)
        /// A camera under its device or group (the owner's key or id).
        case camera(String, CameraRef)
        var id: Self { self }
    }

    private var deviceItems: [Item] {
        model.devices.flatMap { d in
            [Item.device(d.key)] + d.channels.map { Item.camera(d.key, CameraRef(device: d.key, channel: $0.channel)) }
        }
    }

    private var groupItems: [Item] {
        model.groups.flatMap { g in
            [Item.group(g.id)] + (collapsedGroups.contains(g.id) ? [] : g.cameras.map { Item.camera(g.id, $0) })
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            // A stack in a scroll view, not a `List`: the list's table view
            // applies one batched update per change, and when rows appear in
            // more than one place at once (two devices reporting their
            // channels together) AppKit's row-height cache re-enters itself
            // and logs a reentrancy warning it says will become an assert.
            // The rows here are buttons, so the list added nothing else.
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 1) {
                    header("Layouts")
                    ForEach(model.layouts) { layout in
                        row(
                            layout.name, detail: "\(layout.root.leafCount)",
                            selected: state.current == .layout(id: layout.id)
                        ) { window?.show(.layout(id: layout.id)) }
                        .contextMenu { layoutMenu(layout) }
                    }
                    addButton("New Layout…") { window?.newLayout(nil) }

                    header("Groups")
                    ForEach(groupItems) { item in
                        switch item {
                        case .group(let id):
                            if let group = model.group(id) { groupRow(group) }
                        case .camera(let id, let camera):
                            cameraRow(camera, indent: true)
                                .contextMenu {
                                    Button("Remove from Group") { model.removeCamera(camera, fromGroup: id) }
                                }
                        case .device:
                            EmptyView()
                        }
                    }
                    addButton("New Group…") {
                        window?.askName("New Group", initial: "Group") { name in
                            model.createGroup(name: name, cameras: [])
                        }
                    }

                    header("Devices")
                    ForEach(deviceItems) { item in
                        switch item {
                        case .device(let key):
                            if let device = model.runtime(key) { deviceRow(device) }
                        case .camera(_, let camera):
                            cameraRow(camera, indent: true)
                        case .group:
                            EmptyView()
                        }
                    }
                    if model.devices.isEmpty {
                        Button("Add a Device in Settings…") {
                            NSApp.sendAction(#selector(AppDelegate.showSettings(_:)), to: nil, from: nil)
                        }
                        .buttonStyle(.link)
                        .padding(.horizontal, Self.rowInset)
                        .padding(.vertical, 3)
                    }
                }
                .padding(.horizontal, 10)
                .padding(.bottom, 10)
            }
            if let notice = model.notice {
                HStack {
                    Text(notice).font(.caption).foregroundStyle(.secondary)
                    Spacer()
                    Button {
                        model.notice = nil
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("Dismiss")
                }
                .padding(8)
            }
        }
    }

    /// The horizontal inset of a row's content inside its highlight.
    static let rowInset: CGFloat = 8

    private func header(_ title: String) -> some View {
        Text(title)
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(.secondary)
            .padding(.horizontal, Self.rowInset)
            .padding(.top, 12)
            .padding(.bottom, 3)
            .accessibilityAddTraits(.isHeader)
    }

    private func addButton(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: "plus").foregroundStyle(Color.accentColor)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.horizontal, Self.rowInset)
        .padding(.vertical, 3)
    }

    // MARK: Rows

    private func row(_ title: String, detail: String, selected: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack {
                Text(title).lineLimit(1)
                Spacer()
                Text(detail).font(.caption).foregroundStyle(.secondary)
            }
            .padding(.horizontal, Self.rowInset)
            .padding(.vertical, 4)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .fontWeight(selected ? .semibold : .regular)
        .background(
            RoundedRectangle(cornerRadius: 5).fill(selected ? Color.accentColor.opacity(0.25) : Color.clear))
    }

    private func groupRow(_ group: CameraGroup) -> some View {
        HStack(spacing: 4) {
            Button {
                if collapsedGroups.contains(group.id) { collapsedGroups.remove(group.id) } else { collapsedGroups.insert(group.id) }
            } label: {
                Image(systemName: collapsedGroups.contains(group.id) ? "chevron.right" : "chevron.down")
                    .font(.caption2).frame(width: 12)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(collapsedGroups.contains(group.id) ? "Expand" : "Collapse")
            row(group.name, detail: "\(group.cameras.count)", selected: state.current == .group(id: group.id)) {
                window?.show(.group(id: group.id))
            }
        }
        .contextMenu { groupMenu(group) }
    }

    private func deviceRow(_ device: AppModel.DeviceRuntime) -> some View {
        HStack {
            Text(device.config.name).fontWeight(.semibold).lineLimit(1)
            Spacer()
            Text(Self.stateText(device)).font(.caption).foregroundStyle(Self.stateColor(device.link.state))
        }
        .contextMenu {
            Button("Reconnect") { model.reconnect(device.key) }
            Button("New Group from This Device") {
                model.createGroup(
                    name: device.config.name,
                    cameras: device.channels.map { CameraRef(device: device.key, channel: $0.channel) })
            }
            Button("Show All Cameras in New Window") {
                model.openWindow(
                    .scratch(
                        name: device.config.name,
                        root: LayoutNode.balanced(device.channels.map { CameraRef(device: device.key, channel: $0.channel) })))
            }
        }
    }

    private func cameraRow(_ camera: CameraRef, indent: Bool) -> some View {
        let detections = model.detections.active(for: camera)
        let offline = model.channelInfo(camera)?.connected == false
        return Button {
            window?.cameraPicked(camera)
        } label: {
            HStack(spacing: 6) {
                Circle()
                    .fill(offline ? Color.orange : (detections.isEmpty ? Color.green : Color(StageView.color(for: detections[0].kind))))
                    .frame(width: 7, height: 7)
                Text(model.name(of: camera)).lineLimit(1)
                if model.audible.contains(camera) {
                    Image(systemName: "speaker.wave.2.fill").font(.caption2).foregroundStyle(.secondary)
                        .accessibilityLabel("Listening")
                }
                Spacer()
                if model.hasCameraAI(camera) {
                    Text("AI").font(.system(size: 9, weight: .semibold)).foregroundStyle(.secondary)
                        .padding(.horizontal, 3)
                        .overlay(RoundedRectangle(cornerRadius: 3).stroke(Color.secondary.opacity(0.6)))
                        .help("Detection by the camera's AI")
                }
            }
            .padding(.leading, indent ? 12 : 0)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .draggable(StageView.dragString(camera))
        .contextMenu {
            Button(state.hasSelection ? "Show in Selected Tile" : "Show") { window?.cameraPicked(camera) }
            Button("Open in New Window") {
                model.openWindow(.scratch(name: model.name(of: camera), root: .leaf(camera)))
            }
            Button(model.audible.contains(camera) ? "Stop Listening" : "Listen") {
                model.toggleListening(camera, additive: false)
            }
            Menu("Add to Group") {
                ForEach(model.groups) { g in
                    Button(g.name) { model.addCamera(camera, toGroup: g.id) }
                }
                Divider()
                Button("New Group…") {
                    window?.askName("New Group", initial: model.name(of: camera)) { name in
                        model.createGroup(name: name, cameras: [camera])
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func layoutMenu(_ layout: SavedLayout) -> some View {
        Button("Open in New Window") { model.openWindow(.layout(id: layout.id)) }
        Button("Rename…") {
            window?.askName("Rename Layout", initial: layout.name) { model.renameLayout(layout.id, to: $0) }
        }
        Button("Duplicate") {
            model.createLayout(name: model.uniqueLayoutName(layout.name), root: layout.root)
        }
        Divider()
        Button("Delete", role: .destructive) { model.deleteLayout(layout.id) }
    }

    @ViewBuilder
    private func groupMenu(_ group: CameraGroup) -> some View {
        Button("Open in New Window") { model.openWindow(.group(id: group.id)) }
        Button("Rename…") {
            window?.askName("Rename Group", initial: group.name) { model.renameGroup(group.id, to: $0) }
        }
        Button("Save as Layout") {
            model.createLayout(name: model.uniqueLayoutName(group.name), root: LayoutNode.balanced(group.cameras))
        }
        Divider()
        Button("Delete", role: .destructive) { model.deleteGroup(group.id) }
    }

    // MARK: Words

    static func stateText(_ d: AppModel.DeviceRuntime) -> String {
        switch d.link.state {
        case nil: "Not connected"
        case .connecting?: "Connecting"
        case .connected?: "Online"
        case .backoff(let attempt, _)?: "Reconnecting (\(attempt))"
        case .loginFailed?: "Login failed"
        case .closed?: "Disconnected"
        case .other(let s)?: s.prefix(1).uppercased() + s.dropFirst()
        }
    }

    static func stateColor(_ s: DeviceState?) -> Color {
        switch s {
        case .connected?: .green
        case .connecting?, .backoff?: .orange
        case .loginFailed?, .other?: .red
        default: .secondary
        }
    }
}
