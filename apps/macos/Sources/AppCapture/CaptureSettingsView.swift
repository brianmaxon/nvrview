import AppKit
import NVRCore
import SwiftUI

/// Settings > Capture: where saved pictures go, their format, the
/// share-safe names (docs/CAPTURE.md "Save picture"), and the live
/// recordings: their folder, "Record from", and when one stops by itself
/// (docs/CAPTURE.md "Live recording").
package struct CaptureSettingsView: View {
    package let model: any CaptureHost

    package var body: some View {
        Form {
            Section("Pictures") {
                HStack {
                    Text("Save pictures to")
                    Spacer()
                    Text((model.picturesFolder.path as NSString).abbreviatingWithTildeInPath)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Button("Choose…") { chooseFolder() }
                    Button("Reset") { model.picturesFolder = CaptureDefaults.picturesFolder }
                        .disabled(model.picturesFolder == CaptureDefaults.picturesFolder)
                }
                Picker(
                    "Format",
                    selection: Binding(get: { model.pictureFormat }, set: { model.pictureFormat = $0 })
                ) {
                    ForEach(PictureFormat.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                Toggle(
                    "Share-safe names",
                    isOn: Binding(get: { model.shareSafeNames }, set: { model.shareSafeNames = $0 }))
                Text(
                    "Pictures and recordings are named with the time and the camera's name. With share-safe names the camera is called Camera A, so a file can be shared without saying where it came from. JPEG is saved at quality 0.9."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Section("Recordings Window") {
                Picker("Export from", selection: Binding(get: { model.exportQuality }, set: { model.exportQuality = $0 })) {
                    ForEach(ExportQuality.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                Text(
                    "The Recordings window's Export tab downloads the main stream (full quality, larger files) or the sub-stream (smaller files, lower quality) of each camera. The tab can pick the other one for a single export."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Section("Recordings") {
                HStack {
                    Text("Save recordings to")
                    Spacer()
                    Text((model.recordingsFolder.path as NSString).abbreviatingWithTildeInPath)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Button("Choose…") { chooseFolder(model.recordingsFolder) { model.recordingsFolder = $0 } }
                    Button("Reset") { model.recordingsFolder = CaptureDefaults.recordingsFolder }
                        .disabled(model.recordingsFolder == CaptureDefaults.recordingsFolder)
                }
                Picker("Record from", selection: Binding(get: { model.recordFrom }, set: { model.recordFrom = $0 })) {
                    ForEach(RecordQuality.allCases, id: \.self) { Text($0.title).tag($0) }
                }
                Stepper(
                    "Stop after \(RecordingText.minutes(model.maxRecordingMinutes))",
                    value: Binding(get: { model.maxRecordingMinutes }, set: { model.maxRecordingMinutes = $0 }),
                    in: CaptureSettings.minutesRange, step: model.maxRecordingMinutes < 60 ? 5 : 30)
                Stepper(
                    "or at \(RecordingText.size(UInt64(model.maxRecordingGB * 1e9)))",
                    value: Binding(get: { model.maxRecordingGB }, set: { model.maxRecordingGB = $0 }),
                    in: 0.1...64, step: model.maxRecordingGB < 1 ? 0.1 : 1)
                Text(
                    "R, the tile's Record button or the Camera menu records a camera's live stream on this Mac as an MP4, from the next keyframe. \"Record from\" picks the stream; a tile's menu can record the other one. A recording keeps going when its tile closes, and stops by itself at these limits or when the disk is almost full. These are this Mac's recordings, gaps included, not the recorder's own."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }

    private func chooseFolder() {
        chooseFolder(model.picturesFolder) { model.picturesFolder = $0 }
    }

    private func chooseFolder(_ current: URL, _ set: (URL) -> Void) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.prompt = "Choose"
        panel.directoryURL = current.deletingLastPathComponent()
        if panel.runModal() == .OK, let url = panel.url { set(url) }
    }

    package init(model: any CaptureHost) {
        self.model = model
    }
}
