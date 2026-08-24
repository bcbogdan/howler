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
                    Button("Check Active Note for External Changes") { delegate.model.checkExternalChanges() }
                }
            }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = AppModel()
    private var panel: HowlerPanel?
    private var shortcut: GlobalShortcut?

    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
    }

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
        self.panel = panel
        NSApp.activate(ignoringOtherApps: true)
        panel.makeKeyAndOrderFront(nil)
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

enum ImmediateSaveResult: Equatable {
    case safeToReplace
    case mustRetainEditor
}

@MainActor
protocol EffectScheduler: AnyObject {
    func schedule(id: String, delayMilliseconds: UInt64, operation: @escaping @MainActor () -> Void)
    func cancel(id: String)
}

@MainActor
protocol RetryScheduler: AnyObject {
    func schedule(operation: @escaping @MainActor () -> Void)
    func cancel()
}

@MainActor
final class TaskRetryScheduler: RetryScheduler {
    private var task: Task<Void, Never>?

    func schedule(operation: @escaping @MainActor () -> Void) {
        cancel()
        task = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 10_000_000)
            guard !Task.isCancelled else { return }
            self?.task = nil
            operation()
        }
    }

    func cancel() {
        task?.cancel()
        task = nil
    }
}

@MainActor
final class TaskEffectScheduler: EffectScheduler {
    private var tasks: [String: Task<Void, Never>] = [:]

    func schedule(id: String, delayMilliseconds: UInt64, operation: @escaping @MainActor () -> Void) {
        cancel(id: id)
        tasks[id] = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: delayMilliseconds * 1_000_000)
            guard !Task.isCancelled else { return }
            self?.tasks[id] = nil
            operation()
        }
    }

    func cancel(id: String) {
        tasks.removeValue(forKey: id)?.cancel()
    }
}

@MainActor
final class AppModel: ObservableObject {
    @Published private(set) var state = ApplicationState.empty
    @Published var palettePresented = false
    @Published var paletteQuery = ""
    @Published var searchResults: [SearchResult] = []
    @Published var folderPath = UserDefaults.standard.string(forKey: "noteFolder") ?? ""
    @Published var saveState = "Choose a note folder"
    @Published var errorMessage: String?
    @Published var shortcutRegistered = true
    @Published private(set) var isComposing = false

    var snapshot: EditorSnapshot { state.active?.editor.snapshot ?? .empty }
    var recoveries: [RecoveryDraft] { state.recoveries }
    var hasActiveNote: Bool { state.active != nil }
    var hasConnectedFolder: Bool { state.folder != nil }
    var editorSource: String {
        currentPendingDraft?.draft.source ?? state.active?.pending_native_draft?.source ?? pendingInput?.nativeSource ?? snapshot.source
    }
    var hasPendingNativeDraft: Bool { state.active?.pending_native_draft != nil || pendingDraft != nil }
    var hasDurablePendingNativeDraft: Bool { state.active?.pending_native_draft?.durable == true }
    var selectionPresentationContext: EditorSelectionPresentationContext {
        EditorSelectionPresentationContext(
            noteID: state.active?.note_id,
            generation: state.active?.generation,
            revision: snapshot.revision,
            presentsPendingDraft: hasPendingNativeDraft
        )
    }

    private let session: ApplicationSessionProtocol
    private let effectScheduler: EffectScheduler
    private let retryScheduler: RetryScheduler
    private var scheduledEffectIDs: Set<String> = []
    private var externalPoller: Task<Void, Never>?
    private var retryScheduled = false
    private var pendingInput: PendingInput?
    private var pendingDraft: LocalPendingDraft?
    private var pendingResolutionOperationID: String?

    private struct EditorOwner: Equatable {
        let noteID: RustIdentity
        let generation: UInt64
    }

    private struct PendingInput {
        let owner: EditorOwner
        let edit: NativeTextEdit
        let submittedSource: String
        var nativeSource: String
    }

    private struct LocalPendingDraft {
        let owner: EditorOwner
        let draft: PendingNativeDraft
    }

    private var currentOwner: EditorOwner? {
        state.active.map { EditorOwner(noteID: $0.note_id, generation: $0.generation) }
    }

    private var currentPendingDraft: LocalPendingDraft? {
        guard pendingDraft?.owner == currentOwner else { return nil }
        return pendingDraft
    }

    convenience init() {
        self.init(
            session: try! RustApplicationSession(),
            effectScheduler: TaskEffectScheduler(),
            retryScheduler: TaskRetryScheduler()
        )
    }

    convenience init(session: ApplicationSessionProtocol, effectScheduler: EffectScheduler) {
        self.init(session: session, effectScheduler: effectScheduler, retryScheduler: TaskRetryScheduler())
    }

    init(
        session: ApplicationSessionProtocol,
        effectScheduler: EffectScheduler,
        retryScheduler: RetryScheduler
    ) {
        self.session = session
        self.effectScheduler = effectScheduler
        self.retryScheduler = retryScheduler
    }

    func connectSavedFolder() {
        guard !folderPath.isEmpty else { return }
        connect(path: folderPath)
    }

    func chooseFolder() {
        guard canReplaceEditor() else { return }
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
        guard canReplaceEditor() else { return }
        do {
            let stateURL = try FileManager.default.url(
                for: .applicationSupportDirectory,
                in: .userDomainMask,
                appropriateFor: nil,
                create: true
            ).appendingPathComponent("Howler", isDirectory: true)
            try FileManager.default.createDirectory(at: stateURL, withIntermediateDirectories: true)
            let response = try session.connect(path: path, statePath: stateURL.path, create: false)
            _ = consume(response)
            startExternalPolling()
        } catch {
            saveState = "Folder error"
            errorMessage = error.localizedDescription
        }
    }

    func createNote() {
        guard canReplaceEditor() else { return }
        do {
            switch consume(try session.createNote(source: nil)) {
            case .applied: palettePresented = false
            case .rejected: break
            }
        } catch { report(error) }
    }

    func openPalette() {
        paletteQuery = ""
        palettePresented = true
        search("")
    }

    func search(_ query: String) {
        guard state.folder != nil else { searchResults = []; return }
        do {
            switch consume(try session.search(query, limit: 40)) {
            case let .applied(results): searchResults = results
            case .rejected: searchResults = []
            }
        } catch { report(error) }
    }

    func open(_ note: NoteSummary) throws {
        guard canReplaceEditor() else { throw RustError.code(0, "Pending native input must be resolved first") }
        switch consume(try session.openNote(id: note.id.value)) {
        case .applied: palettePresented = false
        case let .rejected(problem): throw RustError.code(0, problem.diagnostic)
        }
    }

    func apply(
        range: Range<Int>,
        replacement: String,
        selectionAnchor: Int,
        selectionHead: Int,
        affinity: SelectionAffinity,
        nativeSource: String
    ) {
        guard state.active != nil else { return }
        if let pendingDraft, pendingDraft.owner == currentOwner {
            self.pendingDraft = LocalPendingDraft(
                owner: pendingDraft.owner,
                draft: PendingNativeDraft(base_revision: pendingDraft.draft.base_revision, source: nativeSource)
            )
            return
        }
        guard state.active?.pending_native_draft == nil else { return }
        if pendingInput != nil {
            pendingInput?.nativeSource = nativeSource
            return
        }
        let revision = snapshot.revision
        let edit = NativeTextEdit(
            expected_revision: revision,
            replacements: [.init(range: TextRange(range), text: replacement)],
            selections: [.init(anchor: selectionAnchor, head: selectionHead, affinity: affinity, revision: revision + 1)],
            history: .typing,
            composition: nil
        )
        guard let owner = currentOwner else { return }
        pendingInput = PendingInput(owner: owner, edit: edit, submittedSource: nativeSource, nativeSource: nativeSource)
        submitPendingInput()
    }

    func undo() { mutateHistory { try session.undo(revision: snapshot.revision) } }
    func redo() { mutateHistory { try session.redo(revision: snapshot.revision) } }

    @discardableResult
    func saveImmediately() -> ImmediateSaveResult {
        guard !isComposing else {
            errorMessage = "Finish text composition before saving or hiding the editor."
            return .mustRetainEditor
        }
        cancelScheduledEffects()
        if let pendingDraft = currentPendingDraft {
            preserveNativeSource(pendingDraft.draft.source, baseRevision: pendingDraft.draft.base_revision, owner: pendingDraft.owner)
            if self.pendingDraft != nil { return .mustRetainEditor }
        }
        if pendingDraft != nil { return .mustRetainEditor }
        if pendingInput != nil {
            submitPendingInput(scheduleBusyRetry: false)
            if pendingInput != nil { return .mustRetainEditor }
        }
        if state.active?.pending_native_draft != nil { return .mustRetainEditor }
        guard let active = state.active else { return .safeToReplace }
        do {
            let target = SaveTarget(note_id: active.note_id, revision: active.editor.snapshot.revision, generation: active.generation)
            _ = consume(try session.save(target: target))
        } catch {
            report(error)
        }
        return state.active?.persistence.replacement_safety == .mustRetainEditor ? .mustRetainEditor : .safeToReplace
    }

    func restoreRecovery(_ recovery: RecoveryDraft) {
        guard canReplaceEditor() else { return }
        do {
            _ = consume(try session.restoreRecovery(id: recovery.note_id))
        } catch { report(error) }
    }

    func discardRecovery(_ recovery: RecoveryDraft) {
        guard canReplaceEditor() else { return }
        do {
            _ = consume(try session.discardRecovery(id: recovery.note_id))
        } catch { report(error) }
    }

    func checkExternalChanges() {
        guard canReplaceEditor() else { return }
        guard state.active != nil else { search(paletteQuery); return }
        do {
            _ = consume(try session.reconcileActive())
            search(paletteQuery)
        } catch { report(error) }
    }

    func compositionChanged(active: Bool) {
        isComposing = active
    }

    func retryPendingNativeDraftPreservation() {
        guard !isComposing, let pendingDraft = currentPendingDraft else { return }
        preserveNativeSource(pendingDraft.draft.source, baseRevision: pendingDraft.draft.base_revision, owner: pendingDraft.owner)
    }

    func savePendingNativeDraftAsNew() {
        guard canResolvePendingDraft() else { return }
        let operationID = pendingResolutionOperationID ?? UUID().uuidString
        pendingResolutionOperationID = operationID
        resolvePendingDraft(.saveAsNew(operationID: operationID, title: nil))
    }

    func discardPendingNativeDraft() {
        guard canResolvePendingDraft() else { return }
        resolvePendingDraft(.discard)
    }

    private func mutateHistory(_ operation: () throws -> ApplicationResponse<EditResult?>) {
        guard state.active != nil, !hasNativeInputBlocker else { return }
        do {
            _ = consume(try operation())
        } catch { report(error) }
    }

    private func submitPendingInput(scheduleBusyRetry: Bool = true) {
        guard let pendingInput else { return }
        guard pendingInput.owner == currentOwner else {
            cancelInputRetry()
            self.pendingInput = nil
            pendingDraft = LocalPendingDraft(
                owner: pendingInput.owner,
                draft: PendingNativeDraft(base_revision: pendingInput.edit.expected_revision, source: pendingInput.nativeSource)
            )
            errorMessage = "Native input was retained for its original note and must not be applied to the current editor."
            return
        }
        do {
            let response = try session.apply(pendingInput.edit)
            let outcome = consume(response)
            let latestSource = self.pendingInput?.nativeSource ?? pendingInput.nativeSource
            self.pendingInput = nil
            cancelInputRetry()
            switch outcome {
            case .applied where latestSource == pendingInput.submittedSource:
                return
            case .applied, .rejected:
                preserveNativeSource(latestSource, baseRevision: pendingInput.edit.expected_revision, owner: pendingInput.owner)
            }
        } catch let error as RustError where error.isBusy {
            guard scheduleBusyRetry, !retryScheduled else { return }
            retryScheduled = true
            retryScheduler.schedule { [weak self] in
                self?.retryScheduled = false
                self?.submitPendingInput()
            }
        } catch {
            let source = pendingInput.nativeSource
            self.pendingInput = nil
            cancelInputRetry()
            preserveNativeSource(source, baseRevision: pendingInput.edit.expected_revision, owner: pendingInput.owner)
            report(error)
        }
    }

    private func preserveNativeSource(_ source: String, baseRevision: UInt64, owner: EditorOwner) {
        let draft = PendingNativeDraft(base_revision: baseRevision, source: source)
        pendingDraft = LocalPendingDraft(owner: owner, draft: draft)
        guard owner == currentOwner else {
            errorMessage = "Native input belongs to a different editor and remains retained locally."
            return
        }
        do {
            let response = try session.preservePendingNativeDraft(draft)
            let outcome = consume(response)
            if case .applied = outcome,
               response.state.active?.pending_native_draft?.durable == true,
               response.state.active?.pending_native_draft?.base_revision == baseRevision,
               response.state.active?.pending_native_draft?.source == source,
               response.state.active.map({ EditorOwner(noteID: $0.note_id, generation: $0.generation) }) == owner {
                pendingDraft = nil
            }
        } catch {
            report(error)
        }
    }

    private func resolvePendingDraft(_ resolution: PendingDraftResolution) {
        do {
            let outcome = consume(try session.resolvePendingNativeDraft(resolution))
            if case .applied = outcome, state.active?.pending_native_draft == nil {
                pendingResolutionOperationID = nil
            }
        } catch { report(error) }
    }

    private var hasNativeInputBlocker: Bool {
        isComposing || pendingInput != nil || pendingDraft != nil || state.active?.pending_native_draft != nil
    }

    private func canReplaceEditor() -> Bool {
        guard !hasNativeInputBlocker else {
            errorMessage = isComposing
                ? "Finish text composition before replacing the editor."
                : "Resolve or preserve pending native input before replacing the editor."
            return false
        }
        return true
    }

    private func canResolvePendingDraft() -> Bool {
        guard !isComposing, pendingInput == nil, pendingDraft == nil,
              state.active?.pending_native_draft?.durable == true else {
            errorMessage = "The native draft must be durably preserved before it can be resolved."
            return false
        }
        return true
    }

    private func cancelInputRetry() {
        retryScheduled = false
        retryScheduler.cancel()
    }

    @discardableResult
    private func consume<Value>(_ response: ApplicationResponse<Value>) -> OperationOutcome<Value> {
        let priorOwner = currentOwner
        state = response.state
        if let pendingInput, pendingInput.owner != currentOwner {
            cancelInputRetry()
            self.pendingInput = nil
            pendingDraft = LocalPendingDraft(
                owner: pendingInput.owner,
                draft: PendingNativeDraft(base_revision: pendingInput.edit.expected_revision, source: pendingInput.nativeSource)
            )
        } else if priorOwner != currentOwner, pendingInput == nil {
            cancelInputRetry()
        }
        if let active = state.active, let authoritative = active.pending_native_draft,
           !authoritative.durable, pendingDraft == nil {
            pendingDraft = LocalPendingDraft(
                owner: EditorOwner(noteID: active.note_id, generation: active.generation),
                draft: PendingNativeDraft(base_revision: authoritative.base_revision, source: authoritative.source)
            )
        }
        perform(response.effects)
        renderState()
        if case let .rejected(problem) = response.outcome { present(problem) }
        return response.outcome
    }

    private func perform(_ effects: [HostEffect]) {
        for effect in effects {
            switch effect {
            case let .scheduleAutosave(id, delay, target):
                scheduledEffectIDs.insert(id)
                effectScheduler.schedule(id: id, delayMilliseconds: delay) { [weak self] in
                    self?.executeAutosave(id: id, target: target)
                }
            case let .cancel(id):
                scheduledEffectIDs.remove(id)
                effectScheduler.cancel(id: id)
            }
        }
    }

    private func executeAutosave(id: String, target: SaveTarget) {
        guard scheduledEffectIDs.contains(id) else { return }
        do {
            _ = consume(try session.save(target: target))
            scheduledEffectIDs.remove(id)
        } catch let error as RustError where error.isBusy {
            guard scheduledEffectIDs.contains(id) else { return }
            effectScheduler.schedule(id: id, delayMilliseconds: 10) { [weak self] in
                self?.executeAutosave(id: id, target: target)
            }
        } catch {
            scheduledEffectIDs.remove(id)
            report(error)
        }
    }

    private func cancelScheduledEffects() {
        for id in scheduledEffectIDs { effectScheduler.cancel(id: id) }
        scheduledEffectIDs.removeAll()
    }

    private func renderState() {
        guard let active = state.active else {
            saveState = state.recoveries.isEmpty ? "No note open" : "Recovery decision required"
            return
        }
        if active.pending_native_draft != nil {
            saveState = "Native input preserved; resolution required"
        } else if active.conflict != nil {
            saveState = "External conflict"
        } else if let issue = active.persistence.issues.first {
            saveState = issueLabel(issue.kind)
            errorMessage = issue.diagnostic
        } else {
            switch active.persistence.durability {
            case .accepted: saveState = "Accepted; recovery pending"
            case .recoveryDurable: saveState = "Recovery durable"
            case .fileSaved: saveState = "File saved"
            }
            errorMessage = nil
        }
    }

    private func issueLabel(_ issue: PersistenceIssueKind) -> String {
        switch issue {
        case .recoveryWrite: "Recovery write failed"
        case .canonicalWrite: "Canonical save failed"
        case .canonicalDurabilityUncertain: "Canonical durability uncertain"
        case .recoveryCleanup: "Recovery cleanup failed"
        case .indexStale: "File saved; index pending"
        case .recencyUpdate: "File saved; recency update failed"
        }
    }

    private func present(_ problem: ApplicationProblem) {
        switch problem.code {
        case .staleRevision: errorMessage = "The note changed before this operation completed."
        case .externalConflict: errorMessage = "The disk and in-memory versions were both preserved."
        case .recoveryPending: errorMessage = "Resolve the pending recovery before opening this note."
        case .persistenceFailure: errorMessage = "The note does not yet have a safe durable copy."
        default: errorMessage = problem.diagnostic
        }
    }

    private func startExternalPolling() {
        externalPoller?.cancel()
        externalPoller = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                guard !Task.isCancelled else { return }
                self?.checkExternalChanges()
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
            if !model.hasActiveNote && model.recoveries.isEmpty {
                EmptyEditorView(model: model)
            }
            if model.palettePresented { PaletteView(model: model).padding(30) }
            if !model.recoveries.isEmpty { RecoveryView(model: model).padding(30) }
            if model.hasPendingNativeDraft { PendingNativeDraftView(model: model).padding(30) }
        }
        .background(.ultraThinMaterial)
        .overlay(alignment: .bottomTrailing) {
            VStack(alignment: .trailing) {
                if let error = model.errorMessage { Text(error).foregroundStyle(.red) }
                Text(model.saveState).foregroundStyle(.secondary)
            }.font(.caption).padding(10).allowsHitTesting(false)
        }
    }
}

struct EmptyEditorView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(spacing: 12) {
            if model.hasConnectedFolder {
                Text("No note open").font(.headline)
                Button("New Note") { model.createNote() }
            } else {
                Text("Choose a folder for your notes").font(.headline)
                Button("Choose Folder…") { model.chooseFolder() }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct PendingNativeDraftView: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Native input needs attention").font(.headline)
            Text(model.hasDurablePendingNativeDraft
                 ? "The native text is durable. Save it as a new note or explicitly discard it."
                 : "The native text is still retained locally. Retry durable preservation before continuing.")
                .foregroundStyle(.secondary)
            HStack {
                if model.hasDurablePendingNativeDraft {
                    Button("Discard Native Draft", role: .destructive) { model.discardPendingNativeDraft() }
                    Button("Save as New Note") { model.savePendingNativeDraftAsNew() }
                } else {
                    Button("Retry Preservation") { model.retryPendingNativeDraftPreservation() }
                }
            }
        }
        .padding(18)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
        .shadow(radius: 24)
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
            Button("Check active note for external changes") { model.checkExternalChanges() }
        }.padding().frame(width: 520)
    }
}
