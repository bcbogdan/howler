import AppKit
import Carbon
import SwiftUI

@main
struct HowlerApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate

    var body: some Scene {
        Settings { SettingsView(model: delegate.model) }
            .commands {
                CommandGroup(replacing: .newItem) {
                    Button("New Note") { delegate.model.createNote() }.keyboardShortcut("n")
                }
                CommandGroup(replacing: .undoRedo) {
                    Button("Undo") { delegate.model.undo() }
                        .keyboardShortcut("z")
                        .disabled(!delegate.model.snapshot.can_undo)
                    Button("Redo") { delegate.model.redo() }
                        .keyboardShortcut("z", modifiers: [.command, .shift])
                        .disabled(!delegate.model.snapshot.can_redo)
                }
                CommandMenu("Navigate") {
                    Button("Open Palette") { delegate.model.openPalette() }.keyboardShortcut("p")
                    Button("Check for External Changes") { delegate.model.rescan() }
                }
            }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = AppModel()
    private var panel: HowlerPanel?
    private var shortcut: GlobalShortcut?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let panel = HowlerPanel(
            contentRect: NSRect(x: 0, y: 0, width: 680, height: 520),
            styleMask: [.borderless, .resizable],
            backing: .buffered,
            defer: false
        )
        panel.contentView = NSHostingView(rootView: ContentView(model: model))
        panel.center()
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.saveGate = { [weak self] in self?.model.saveImmediately() == .safeToReplace }
        panel.makeKeyAndOrderFront(nil)
        self.panel = panel
        shortcut = GlobalShortcut(
            keyCode: UInt32(kVK_ANSI_H),
            modifiers: UInt32(cmdKey | optionKey)
        ) { [weak self] in Task { @MainActor in self?.togglePanel() } }
        model.shortcutRegistered = shortcut != nil
        model.connectSavedFolder()
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        model.saveImmediately() == .safeToReplace ? .terminateNow : .terminateCancel
    }

    private func togglePanel() {
        guard let panel else { return }
        if panel.isVisible {
            if model.saveImmediately() == .safeToReplace { panel.orderOut(nil) }
        } else {
            NSApp.activate(ignoringOtherApps: true)
            panel.makeKeyAndOrderFront(nil)
        }
    }
}

final class HowlerPanel: NSPanel {
    var saveGate: (() -> Bool)?
    override var canBecomeKey: Bool { true }
    override func cancelOperation(_ sender: Any?) {
        if saveGate?() != false { orderOut(sender) }
    }
}

enum ImmediateSaveResult {
    case safeToReplace
    case mustRetainEditor
}

@MainActor
final class AppModel: ObservableObject {
    @Published var snapshot = EditorSnapshot.empty
    @Published var palettePresented = false
    @Published var paletteQuery = ""
    @Published var searchResults: [SearchResult] = []
    @Published var folderPath = UserDefaults.standard.string(forKey: "noteFolder") ?? ""
    @Published var saveState = "Choose a note folder"
    @Published var errorMessage: String?
    @Published var recoveries: [RecoveryDraft] = []
    @Published var shortcutRegistered = true
    @Published private(set) var isComposing = false

    private var folder: RustFolder?
    private var editor: RustNoteEditor?
    private var autosave: Task<Void, Never>?
    private var externalPoller: Task<Void, Never>?
    private var durability = "file_saved"
    private var currentNoteID: String?

    func connectSavedFolder() {
        guard !folderPath.isEmpty else { return }
        connect(path: folderPath)
    }

    func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let path = panel.url?.path else { return }
        folderPath = path
        UserDefaults.standard.set(path, forKey: "noteFolder")
        connect(path: path)
    }

    func connect(path: String) {
        guard saveImmediately() == .safeToReplace else { return }
        do {
            let stateURL = try FileManager.default.url(
                for: .applicationSupportDirectory,
                in: .userDomainMask,
                appropriateFor: nil,
                create: true
            ).appendingPathComponent("Howler", isDirectory: true)
            try FileManager.default.createDirectory(at: stateURL, withIntermediateDirectories: true)
            let folder = try RustFolder(path: path, statePath: stateURL.path)
            self.folder = folder
            recoveries = try folder.recoveries()
            if !recoveries.isEmpty {
                editor = nil
                currentNoteID = nil
                snapshot = .empty
                saveState = "Recovery decision required"
                errorMessage = nil
                startExternalPolling()
                return
            }
            let results = try folder.search("")
            if let first = results.first {
                try open(first.note)
            } else {
                createNote()
            }
            startExternalPolling()
            saveState = "File saved"
            errorMessage = nil
        } catch {
            saveState = "Folder error"
            errorMessage = error.localizedDescription
        }
    }

    func createNote() {
        guard let folder else { errorMessage = "Choose a note folder first"; return }
        guard saveImmediately() == .safeToReplace else { return }
        do {
            try open(folder.createNote())
            palettePresented = false
        } catch { report(error) }
    }

    func openPalette() {
        paletteQuery = ""
        palettePresented = true
        search("")
    }

    func search(_ query: String) {
        guard let folder else { searchResults = []; return }
        do { searchResults = try folder.search(query) } catch { report(error) }
    }

    func open(_ note: NoteSummary) throws {
        guard saveImmediately() == .safeToReplace else { throw RustError.code(6, "Current draft is not durable") }
        if recoveries.contains(where: { $0.note_id == note.id.value }) {
            throw RustError.code(6, "Restore or discard this note's recovery first")
        }
        guard let folder else { return }
        let editor = try folder.openEditor(id: note.id.value)
        self.editor = editor
        currentNoteID = note.id.value
        snapshot = try editor.snapshot()
        saveState = "File saved"
        durability = "file_saved"
        palettePresented = false
    }

    func apply(range: Range<Int>, replacement: String, selection: Int) {
        guard let editor else { return }
        do {
            let outcome = try editor.apply(
                range: range,
                replacement: replacement,
                selection: selection,
                revision: snapshot.revision
            )
            snapshot = try editor.snapshot()
            saveState = outcome.durability == "recovery_durable" ? "Recovery durable" : "Accepted, recovery failed"
            durability = outcome.durability
            errorMessage = outcome.recovery_error
            scheduleSave()
        } catch { report(error) }
    }

    func undo() { mutateHistory { try $0.undo(expectedRevision: snapshot.revision) } }
    func redo() { mutateHistory { try $0.redo(expectedRevision: snapshot.revision) } }

    @discardableResult
    func saveImmediately() -> ImmediateSaveResult {
        autosave?.cancel()
        autosave = nil
        guard let editor else { return .safeToReplace }
        do {
            let outcome = try editor.save(expectedRevision: snapshot.revision)
            snapshot = try editor.snapshot()
            durability = outcome.durability
            if let canonicalError = outcome.canonical_error {
                saveState = "Recovery durable, canonical durability uncertain"
                errorMessage = canonicalError
                if let folder { recoveries = (try? folder.recoveries()) ?? recoveries }
            } else if outcome.index_state == "stale" {
                saveState = "File saved, index pending"
                errorMessage = outcome.index_error
            } else if outcome.recovery_cleanup == "retained" {
                saveState = "File saved, recovery cleanup failed"
                errorMessage = outcome.recovery_cleanup_error
            } else if let recencyError = outcome.recency_error {
                saveState = "File saved, recency update failed"
                errorMessage = recencyError
            } else {
                saveState = "File saved"
                errorMessage = nil
            }
            return outcome.durability == "accepted" ? .mustRetainEditor : .safeToReplace
        } catch {
            report(error)
            if let folder, let refreshed = try? folder.recoveries() {
                recoveries = refreshed
            }
            if durability == "accepted", let currentNoteID,
               recoveries.contains(where: { $0.note_id == currentNoteID }) {
                durability = "recovery_durable"
                saveState = "Recovery durable, file save failed"
            }
            return durability == "recovery_durable" || durability == "file_saved"
                ? .safeToReplace
                : .mustRetainEditor
        }
    }

    func restoreRecovery(_ recovery: RecoveryDraft) {
        guard let folder else { return }
        guard saveImmediately() == .safeToReplace else { return }
        do {
            editor = try folder.restoreRecovery(id: recovery.note_id)
            currentNoteID = recovery.note_id
            snapshot = try editor?.snapshot() ?? .empty
            durability = "recovery_durable"
            saveState = "Recovery restored"
            recoveries.removeAll { $0.note_id == recovery.note_id }
            errorMessage = nil
        } catch { report(error) }
    }

    func discardRecovery(_ recovery: RecoveryDraft) {
        guard let folder else { return }
        do {
            try folder.discardRecovery(id: recovery.note_id)
            recoveries.removeAll { $0.note_id == recovery.note_id }
            if editor == nil, let note = try folder.search("").first?.note { try open(note) }
        } catch { report(error) }
    }

    func rescan() {
        guard !isComposing else { return }
        do {
            try folder?.rescan()
            if let editor {
                let outcome = try editor.reconcile()
                if outcome.status == "conflict" {
                    saveState = "External conflict"
                    errorMessage = "The disk and in-memory versions were both preserved."
                    return
                }
                if outcome.status == "refreshed" { snapshot = try editor.snapshot() }
            }
            search(paletteQuery)
        } catch { report(error) }
    }

    func compositionChanged(active: Bool) {
        isComposing = active
    }

    private func mutateHistory(_ operation: (RustNoteEditor) throws -> MutationOutcome) {
        guard let editor else { return }
        do {
            let outcome = try operation(editor)
            snapshot = try editor.snapshot()
            saveState = outcome.durability == "recovery_durable" ? "Recovery durable" : "Accepted"
            durability = outcome.durability
            errorMessage = outcome.recovery_error
            scheduleSave()
        } catch { report(error) }
    }

    private func scheduleSave() {
        autosave?.cancel()
        autosave = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 750_000_000)
            guard !Task.isCancelled else { return }
            self?.saveImmediately()
        }
    }

    private func startExternalPolling() {
        externalPoller?.cancel()
        externalPoller = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                guard !Task.isCancelled else { return }
                self?.rescan()
            }
        }
    }

    private func report(_ error: Error) {
        saveState = "Operation failed"
        errorMessage = error.localizedDescription
    }
}

struct ContentView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        ZStack(alignment: .top) {
            EditorView(model: model).padding(22)
            if model.palettePresented { PaletteView(model: model).padding(30) }
            if !model.recoveries.isEmpty { RecoveryView(model: model).padding(30) }
        }
        .background(.ultraThinMaterial)
        .overlay(alignment: .bottomTrailing) {
            VStack(alignment: .trailing) {
                if let error = model.errorMessage { Text(error).foregroundStyle(.red) }
                Text(model.saveState).foregroundStyle(.secondary)
            }.font(.caption).padding(10)
        }
    }
}

struct RecoveryView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Unsaved drafts found").font(.headline)
            ForEach(model.recoveries, id: \.note_id) { recovery in
                HStack {
                    Text(recovery.relative_path).lineLimit(1)
                    Spacer()
                    Button("Discard") { model.discardRecovery(recovery) }
                    Button("Restore") { model.restoreRecovery(recovery) }
                }
            }
        }
        .padding(18)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .shadow(radius: 24)
    }
}

struct PaletteView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(spacing: 0) {
            TextField("Search notes", text: $model.paletteQuery)
                .textFieldStyle(.plain)
                .font(.title3)
                .padding(14)
                .onChange(of: model.paletteQuery) { model.search($0) }
            Divider()
            List(model.searchResults) { result in
                Button {
                    do { try model.open(result.note) } catch { model.errorMessage = error.localizedDescription }
                } label: {
                    VStack(alignment: .leading) {
                        Text(result.note.title)
                        Text(result.snippet).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                    }
                }.buttonStyle(.plain)
            }.frame(maxHeight: 320)
        }
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .shadow(radius: 24)
        .onExitCommand { model.palettePresented = false }
    }
}

struct SettingsView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        Form {
            LabeledContent("Note folder") {
                HStack {
                    Text(model.folderPath.isEmpty ? "Not selected" : model.folderPath).lineLimit(1)
                    Button("Choose…") { model.chooseFolder() }
                }
            }
            Text(model.shortcutRegistered
                 ? "Global shortcut: Command-Option-H"
                 : "Global shortcut registration failed")
                .foregroundStyle(model.shortcutRegistered ? Color.secondary : Color.red)
            Button("Rebuild and check external changes") { model.rescan() }
        }.padding().frame(width: 520)
    }
}
