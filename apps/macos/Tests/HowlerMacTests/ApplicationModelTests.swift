import XCTest
@testable import HowlerMac

@MainActor
final class ApplicationModelTests: XCTestCase {
    func testApplicationJSONContractsDecodeTypedEnumsAndIntegerRevisions() throws {
        let snapshot = try JSONDecoder().decode(EditorSnapshot.self, from: Data(#"{"revision":2,"source":"a😀","selections":[{"anchor":5,"head":5,"affinity":"Downstream","revision":2}],"can_undo":true,"can_redo":false}"#.utf8))
        XCTAssertEqual(snapshot.source, "a😀")
        XCTAssertEqual(snapshot.selections.first?.affinity, .downstream)

        let results = try JSONDecoder().decode([SearchResult].self, from: Data(#"[{"note":{"id":{"kind":"provisional","value":"id"},"relative_path":"note.md","title":"Note","content_hash":"hash"},"snippet":"body","reason":"fuzzy_title"}]"#.utf8))
        XCTAssertEqual(results.first?.note.id.kind, .provisional)
        XCTAssertEqual(results.first?.reason, .fuzzyTitle)
    }

    func testSessionResponseDecodesAuthoritativeStateAndIdentifiedEffect() throws {
        let data = Data(#"{"state":{"folder":{"path":"/notes","adopted":false,"generation":3},"active":null,"recoveries":[],"background_tasks":[]},"effects":[{"kind":"schedule_autosave","effect_id":"autosave-1","delay_ms":750,"target":{"note_id":{"kind":"provisional","value":"id"},"revision":2,"generation":3}}],"outcome":{"status":"applied","value":{"opened_note":null}}}"#.utf8)
        let response = try JSONDecoder().decode(ApplicationResponse<ConnectResult>.self, from: data)
        XCTAssertEqual(response.state.folder?.generation, 3)
        XCTAssertEqual(response.effects, [.scheduleAutosave(id: "autosave-1", delayMilliseconds: 750, target: SaveTarget(note_id: identity, revision: 2, generation: 3))])
    }

    func testUnknownRequiredEnumIsRejected() {
        let data = Data(#"{"state":{"folder":null,"active":null,"recoveries":[],"background_tasks":[]},"effects":[],"outcome":{"status":"rejected","value":{"code":"future_code","diagnostic":"future","details":null}}}"#.utf8)
        XCTAssertThrowsError(try JSONDecoder().decode(ApplicationResponse<ConnectResult>.self, from: data))
    }

    func testRejectedResponseStillInstallsAuthoritativeState() {
        let session = FakeSession()
        session.connectResponse = ApplicationResponse(
            state: makeState(source: "authoritative", revision: 4),
            effects: [],
            outcome: .rejected(ApplicationProblem(code: .recoveryPending, diagnostic: "pending", details: nil))
        )
        let model = AppModel(session: session, effectScheduler: FakeEffectScheduler())

        model.connect(path: "/tmp/notes")

        XCTAssertEqual(model.snapshot.source, "authoritative")
        XCTAssertEqual(model.snapshot.revision, 4)
        XCTAssertEqual(model.errorMessage, "Resolve the pending recovery before opening this note.")
    }

    func testBusyRetriesExactNativeEditDeterministically() {
        let session = FakeSession()
        session.connectResponse = connectedResponse(state: makeState(source: "a", revision: 1))
        session.applyResults = [
            .failure(.code(SessionTransportStatus.busy, "busy")),
            .success(ApplicationResponse(state: makeState(source: "ab", revision: 2), effects: [], outcome: .applied(editResult(revision: 2))))
        ]
        let retries = FakeRetryScheduler()
        let model = AppModel(session: session, effectScheduler: FakeEffectScheduler(), retryScheduler: retries)
        model.connect(path: "/tmp/notes")

        model.apply(range: 1..<1, replacement: "b", selectionAnchor: 2, selectionHead: 2, affinity: .downstream, nativeSource: "ab")
        XCTAssertEqual(session.appliedEdits.count, 1)
        retries.fire()

        XCTAssertEqual(session.appliedEdits.count, 2)
        XCTAssertEqual(session.appliedEdits[0], session.appliedEdits[1])
        XCTAssertEqual(model.snapshot.source, "ab")
    }

    func testBusyInputBlocksReplacementTransition() {
        let session = FakeSession()
        session.connectResponse = connectedResponse(state: makeState(source: "a", revision: 1))
        session.applyResults = [.failure(.code(SessionTransportStatus.busy, "busy"))]
        let retries = FakeRetryScheduler()
        let model = AppModel(session: session, effectScheduler: FakeEffectScheduler(), retryScheduler: retries)
        model.connect(path: "/tmp/notes")

        model.apply(range: 1..<1, replacement: "b", selectionAnchor: 2, selectionHead: 2, affinity: .downstream, nativeSource: "ab")
        model.createNote()

        XCTAssertEqual(session.createNoteCalls, 0)
        XCTAssertNotNil(retries.operation)
    }

    func testStaleNativeEditPreservesUnreplayableSource() {
        let session = FakeSession()
        session.connectResponse = connectedResponse(state: makeState(source: "a", revision: 1))
        let stale = ApplicationProblem(code: .staleRevision, diagnostic: "stale", details: .staleRevision(expected: 1, current: 2))
        session.applyResults = [.success(ApplicationResponse(state: makeState(source: "external", revision: 2), effects: [], outcome: .rejected(stale)))]
        session.preserveResults = [.success(ApplicationResponse(state: makeState(source: "external", revision: 2, pendingSource: "ab"), effects: [], outcome: .applied(emptyResult)))]
        let model = AppModel(session: session, effectScheduler: FakeEffectScheduler())
        model.connect(path: "/tmp/notes")

        model.apply(range: 1..<1, replacement: "b", selectionAnchor: 2, selectionHead: 2, affinity: .downstream, nativeSource: "ab")

        XCTAssertEqual(session.preservedDrafts, [PendingNativeDraft(base_revision: 1, source: "ab")])
        XCTAssertEqual(model.editorSource, "ab")
        XCTAssertEqual(model.state.active?.persistence.replacement_safety, .mustRetainEditor)
    }

    func testRejectedAndNonDurablePreservationRetainExactLocalDraftForSaveRetry() {
        let session = FakeSession()
        session.connectResponse = connectedResponse(state: makeState(source: "a", revision: 1))
        let stale = ApplicationProblem(code: .staleRevision, diagnostic: "stale", details: .staleRevision(expected: 1, current: 2))
        let persistence = ApplicationProblem(code: .persistenceFailure, diagnostic: "disk full", details: .persistence(issues: []))
        session.applyResults = [.success(ApplicationResponse(state: makeState(source: "external", revision: 2), effects: [], outcome: .rejected(stale)))]
        session.preserveResults = [
            .success(ApplicationResponse(state: makeState(source: "external", revision: 2), effects: [], outcome: .rejected(persistence))),
            .success(ApplicationResponse(state: makeState(source: "external", revision: 2, pendingSource: "ab", pendingDurable: false), effects: [], outcome: .applied(emptyResult)))
        ]
        let model = AppModel(session: session, effectScheduler: FakeEffectScheduler())
        model.connect(path: "/tmp/notes")

        model.apply(range: 1..<1, replacement: "b", selectionAnchor: 2, selectionHead: 2, affinity: .downstream, nativeSource: "ab")
        XCTAssertEqual(model.editorSource, "ab")
        model.createNote()
        XCTAssertEqual(session.createNoteCalls, 0)
        XCTAssertEqual(model.saveImmediately(), .mustRetainEditor)

        XCTAssertEqual(session.preservedDrafts, [
            PendingNativeDraft(base_revision: 1, source: "ab"),
            PendingNativeDraft(base_revision: 1, source: "ab")
        ])
        XCTAssertEqual(model.editorSource, "ab")
        XCTAssertEqual(session.saveTargets.count, 0)
    }

    func testIMEBlocksSaveAndReplacement() {
        let session = FakeSession()
        session.connectResponse = connectedResponse(state: makeState(source: "a", revision: 1))
        let model = AppModel(session: session, effectScheduler: FakeEffectScheduler())
        model.connect(path: "/tmp/notes")
        model.compositionChanged(active: true)

        model.createNote()

        XCTAssertEqual(session.createNoteCalls, 0)
        XCTAssertEqual(model.saveImmediately(), .mustRetainEditor)
        XCTAssertEqual(session.saveTargets.count, 0)
    }

    func testAutosaveBusyRetriesExactTargetAndCancellationStopsRetry() {
        let session = FakeSession()
        let target = SaveTarget(note_id: identity, revision: 1, generation: 1)
        session.connectResponse = ApplicationResponse(
            state: makeState(source: "a", revision: 1),
            effects: [.scheduleAutosave(id: "save-1", delayMilliseconds: 750, target: target)],
            outcome: .applied(ConnectResult(opened_note: nil))
        )
        session.saveResults = [.failure(.code(SessionTransportStatus.busy, "busy"))]
        let scheduler = FakeEffectScheduler()
        let model = AppModel(session: session, effectScheduler: scheduler)
        model.connect(path: "/tmp/notes")

        scheduler.fire("save-1")
        XCTAssertEqual(session.saveTargets, [target])
        XCTAssertEqual(scheduler.scheduled, ["save-1", "save-1"])

        session.applyResults = [.success(ApplicationResponse(
            state: makeState(source: "ab", revision: 2),
            effects: [.cancel(id: "save-1")],
            outcome: .applied(editResult(revision: 2))
        ))]
        model.apply(range: 1..<1, replacement: "b", selectionAnchor: 2, selectionHead: 2, affinity: .downstream, nativeSource: "ab")
        scheduler.fire("save-1")

        XCTAssertEqual(session.saveTargets, [target])
        XCTAssertEqual(scheduler.cancelled, ["save-1"])
    }

    func testAutosaveBusyRetriesExactTargetUntilApplied() {
        let session = FakeSession()
        let target = SaveTarget(note_id: identity, revision: 1, generation: 1)
        session.connectResponse = ApplicationResponse(
            state: makeState(source: "a", revision: 1),
            effects: [.scheduleAutosave(id: "save-1", delayMilliseconds: 750, target: target)],
            outcome: .applied(ConnectResult(opened_note: nil))
        )
        session.saveResults = [
            .failure(.code(SessionTransportStatus.busy, "busy")),
            .success(saveResponse(state: makeState(source: "a", revision: 1)))
        ]
        let scheduler = FakeEffectScheduler()
        let model = AppModel(session: session, effectScheduler: scheduler)
        model.connect(path: "/tmp/notes")

        scheduler.fire("save-1")
        scheduler.fire("save-1")

        XCTAssertEqual(session.saveTargets, [target, target])
    }

    func testDurablePendingDraftCanSaveAsNewAndDiscardContractsEncode() throws {
        let saveAsNew = try JSONSerialization.jsonObject(with: JSONEncoder().encode(PendingDraftResolution.saveAsNew(operationID: "operation-1", title: nil))) as! [String: Any]
        let discard = try JSONSerialization.jsonObject(with: JSONEncoder().encode(PendingDraftResolution.discard)) as! [String: Any]
        XCTAssertEqual(saveAsNew["resolution"] as? String, "save_as_new")
        XCTAssertEqual(saveAsNew["operation_id"] as? String, "operation-1")
        XCTAssertEqual(discard["resolution"] as? String, "discard")

        let session = FakeSession()
        session.connectResponse = connectedResponse(state: makeState(source: "a", revision: 1, pendingSource: "native", pendingDurable: true))
        session.resolveResponse = ApplicationResponse(
            state: makeState(source: "a", revision: 1),
            effects: [],
            outcome: .applied(NoteResult(note: noteSummary))
        )
        let model = AppModel(session: session, effectScheduler: FakeEffectScheduler())
        model.connect(path: "/tmp/notes")

        model.savePendingNativeDraftAsNew()

        guard case let .saveAsNew(operationID, title) = session.resolutions.first else {
            return XCTFail("Expected save-as-new resolution")
        }
        XCTAssertFalse(operationID.isEmpty)
        XCTAssertNil(title)
        XCTAssertFalse(model.hasPendingNativeDraft)
    }

    func testCancelEffectCancelsScheduledIdentifier() {
        let session = FakeSession()
        let target = SaveTarget(note_id: identity, revision: 1, generation: 1)
        session.connectResponse = ApplicationResponse(
            state: makeState(source: "a", revision: 1),
            effects: [.scheduleAutosave(id: "save-1", delayMilliseconds: 750, target: target), .cancel(id: "save-1")],
            outcome: .applied(ConnectResult(opened_note: nil))
        )
        let scheduler = FakeEffectScheduler()
        let model = AppModel(session: session, effectScheduler: scheduler)

        model.connect(path: "/tmp/notes")

        XCTAssertEqual(scheduler.scheduled, ["save-1"])
        XCTAssertEqual(scheduler.cancelled, ["save-1"])
    }
}

private let identity = RustIdentity(kind: .provisional, value: "id")
private let noteSummary = NoteSummary(id: identity, relative_path: "note.md", title: "Note", content_hash: "hash")
private let emptyResult = try! JSONDecoder().decode(EmptyResult.self, from: Data("null".utf8))

private func makeState(
    source: String,
    revision: UInt64,
    pendingSource: String? = nil,
    pendingDurable: Bool = true
) -> ApplicationState {
    let snapshot = EditorSnapshot(revision: revision, source: source, selections: [], can_undo: revision > 0, can_redo: false)
    let pending = pendingSource.map { PendingNativeDraftState(base_revision: revision, source: $0, durable: pendingDurable) }
    return ApplicationState(
        folder: FolderState(path: "/notes", adopted: false, generation: 1),
        active: ActiveEditorState(
            note_id: identity,
            editor: EditorPresentationState(snapshot: snapshot, decorations: DecorationSet(revision: revision, items: [])),
            persistence: PersistenceState(durability: .recoveryDurable, replacement_safety: pending == nil ? .safe : .mustRetainEditor, issues: []),
            conflict: nil,
            pending_native_draft: pending,
            generation: 1
        ),
        recoveries: [],
        background_tasks: []
    )
}

private func connectedResponse(state: ApplicationState) -> ApplicationResponse<ConnectResult> {
    ApplicationResponse(state: state, effects: [], outcome: .applied(ConnectResult(opened_note: nil)))
}

private func editResult(revision: UInt64) -> EditResult {
    EditResult(revision: revision, changed_ranges: [], selections: [], decorations: [])
}

private func saveResponse(state: ApplicationState) -> ApplicationResponse<SessionSaveResult> {
    let save = SaveOutcome(
        revision: state.active?.editor.snapshot.revision ?? 0,
        durability: .fileSaved,
        recovery_cleanup: .removed,
        recovery_cleanup_error: nil,
        index_state: .current,
        index_error: nil,
        recency_error: nil,
        canonical_error: nil
    )
    return ApplicationResponse(state: state, effects: [], outcome: .applied(SessionSaveResult(save: save)))
}

@MainActor
private final class FakeEffectScheduler: EffectScheduler {
    var scheduled: [String] = []
    var cancelled: [String] = []
    var operations: [String: @MainActor () -> Void] = [:]

    func schedule(id: String, delayMilliseconds: UInt64, operation: @escaping @MainActor () -> Void) {
        scheduled.append(id)
        operations[id] = operation
    }

    func cancel(id: String) {
        cancelled.append(id)
        operations[id] = nil
    }

    func fire(_ id: String) {
        let operation = operations.removeValue(forKey: id)
        operation?()
    }
}

@MainActor
private final class FakeRetryScheduler: RetryScheduler {
    var operation: (@MainActor () -> Void)?

    func schedule(operation: @escaping @MainActor () -> Void) { self.operation = operation }
    func cancel() { operation = nil }

    func fire() {
        let operation = operation
        self.operation = nil
        operation?()
    }
}

@MainActor
private final class FakeSession: ApplicationSessionProtocol {
    var connectResponse = connectedResponse(state: .empty)
    var applyResults: [Result<ApplicationResponse<EditResult>, RustError>] = []
    var preserveResults: [Result<ApplicationResponse<EmptyResult>, RustError>] = []
    var saveResults: [Result<ApplicationResponse<SessionSaveResult>, RustError>] = []
    var resolveResponse = ApplicationResponse(state: ApplicationState.empty, effects: [], outcome: .applied(NoteResult(note: noteSummary)))
    var appliedEdits: [NativeTextEdit] = []
    var preservedDrafts: [PendingNativeDraft] = []
    var saveTargets: [SaveTarget] = []
    var resolutions: [PendingDraftResolution] = []
    var createNoteCalls = 0

    func state() throws -> ApplicationResponse<EmptyResult> { fatalError() }
    func connect(path: String, statePath: String, create: Bool) throws -> ApplicationResponse<ConnectResult> { connectResponse }
    func createNote(source: String?) throws -> ApplicationResponse<NoteResult> {
        createNoteCalls += 1
        return ApplicationResponse(state: ApplicationState.empty, effects: [], outcome: .applied(NoteResult(note: noteSummary)))
    }
    func openNote(id: String) throws -> ApplicationResponse<NoteResult> { fatalError() }
    func closeNote() throws -> ApplicationResponse<EmptyResult> { fatalError() }
    func apply(_ edit: NativeTextEdit) throws -> ApplicationResponse<EditResult> {
        appliedEdits.append(edit)
        return try applyResults.removeFirst().get()
    }
    func preservePendingNativeDraft(_ draft: PendingNativeDraft) throws -> ApplicationResponse<EmptyResult> {
        preservedDrafts.append(draft)
        return try preserveResults.removeFirst().get()
    }
    func resolvePendingNativeDraft(_ resolution: PendingDraftResolution) throws -> ApplicationResponse<NoteResult> {
        resolutions.append(resolution)
        return resolveResponse
    }
    func undo(revision: UInt64) throws -> ApplicationResponse<EditResult?> { fatalError() }
    func redo(revision: UInt64) throws -> ApplicationResponse<EditResult?> { fatalError() }
    func save(target: SaveTarget) throws -> ApplicationResponse<SessionSaveResult> {
        saveTargets.append(target)
        return try saveResults.removeFirst().get()
    }
    func restoreRecovery(id: String) throws -> ApplicationResponse<NoteResult> { fatalError() }
    func discardRecovery(id: String) throws -> ApplicationResponse<EmptyResult> { fatalError() }
    func reconcileActive() throws -> ApplicationResponse<ReconcileOutcome> { fatalError() }
    func search(_ query: String, limit: Int) throws -> ApplicationResponse<[SearchResult]> { fatalError() }
}
