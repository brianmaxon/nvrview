import AppKit
import NVRCore
import SwiftUI
import UniformTypeIdentifiers

/// Settings, Updates (docs/UPDATES.md, "Settings and checks in the apps"):
/// the source, the trusted keys, automatic checks, Check Now, and one line
/// of status.
struct UpdateSettingsView: View {
    let updater: Updater
    @State private var sourceText = ""
    @State private var sourceProblem: String?
    @State private var keyText = ""
    @State private var keyProblem: String?

    var body: some View {
        Form {
            Section {
                HStack {
                    TextField("Folder or URL", text: $sourceText, prompt: Text("https://… or a folder"))
                        .onSubmit(applySource)
                    Button("Choose Folder…", action: chooseFolder)
                }
                if let sourceProblem {
                    Text(sourceProblem).font(.callout).foregroundStyle(.red)
                } else {
                    Text("A folder (a network share works), an https:// address, or http:// on your local network.")
                        .font(.callout).foregroundStyle(.secondary)
                }
            } header: {
                Text("Update source")
            }
            Section {
                ForEach(updater.settings.keys, id: \.self) { key in
                    HStack {
                        Text("Key \(updater.keyID(key))").font(.body.monospaced())
                        Spacer()
                        Button("Remove") { updater.removeKey(key) }
                    }
                }
                HStack {
                    TextField("Public key", text: $keyText, prompt: Text("nvrview-ed25519-pub …"))
                        .onSubmit(addKey)
                    Button("Add Key", action: addKey).disabled(keyText.isEmpty)
                    Button("Choose File…", action: chooseKeyFile)
                }
                if let keyProblem {
                    Text(keyProblem).font(.callout).foregroundStyle(.red)
                } else {
                    Text("Updates are installed only when signed by one of these keys.")
                        .font(.callout).foregroundStyle(.secondary)
                }
            } header: {
                Text("Trusted keys")
            }
            Section {
                Toggle(
                    "Check automatically",
                    isOn: Binding(get: { updater.settings.automatic }, set: { updater.setAutomatic($0) }))
                HStack {
                    Text(updater.status.line(current: updater.current, formatter: Self.time))
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button("Check Now") {
                        applySource()
                        updater.check(userAsked: true)
                    }
                    .disabled(!updater.settings.canCheck)
                }
            }
        }
        .formStyle(.grouped)
        .onAppear { sourceText = updater.settings.source }
    }

    private func applySource() {
        sourceProblem = updater.setSource(sourceText)
    }

    private func addKey() {
        keyProblem = updater.addKey(keyText)
        if keyProblem == nil { keyText = "" }
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.prompt = "Use Folder"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        sourceText = url.path
        applySource()
    }

    private func chooseKeyFile() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        guard panel.runModal() == .OK, let url = panel.url,
            let text = try? String(contentsOf: url, encoding: .utf8), text.utf8.count < 4096
        else { return }
        keyProblem = updater.addKey(text)
    }

    private static func time(_ date: Date) -> String {
        date.formatted(date: Calendar.current.isDateInToday(date) ? .omitted : .abbreviated, time: .shortened)
    }
}
