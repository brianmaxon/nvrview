import AppKit
import NVRCore
import SwiftUI

/// Settings > Capture: where saved pictures go, their format, and the
/// share-safe names (docs/CAPTURE.md "Save picture").
struct CaptureSettingsView: View {
    let model: AppModel

    var body: some View {
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
                    Button("Reset") { model.picturesFolder = AppModel.defaultPicturesFolder }
                        .disabled(model.picturesFolder == AppModel.defaultPicturesFolder)
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
                    "Pictures are named with the time and the camera's name. With share-safe names the camera is called Camera A, so a file can be shared without saying where it came from. JPEG is saved at quality 0.9."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.prompt = "Choose"
        panel.directoryURL = model.picturesFolder.deletingLastPathComponent()
        if panel.runModal() == .OK, let url = panel.url { model.picturesFolder = url }
    }
}
