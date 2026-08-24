import Foundation
import CHowlerApplication

enum RustError: LocalizedError, Equatable {
    case code(Int32, String)
    case invalidResponse

    var isBusy: Bool {
        if case let .code(code, _) = self { return code == SessionTransportStatus.busy }
        return false
    }

    var errorDescription: String? {
        switch self {
        case let .code(_, message): message
        case .invalidResponse: "Rust returned an invalid response"
        }
    }
}

enum SessionTransportStatus {
    static let busy = Int32(HOWLER_APPLICATION_BUSY)
}

enum IdentityKind: String, Codable { case adopted, provisional }

struct RustIdentity: Codable, Hashable {
    let kind: IdentityKind
    let value: String
}

struct NoteSummary: Codable, Identifiable, Hashable {
    let id: RustIdentity
    let relative_path: String
    let title: String
    let content_hash: String
}

enum SearchReason: String, Codable {
    case exactTitle = "exact_title"
    case prefixTitle = "prefix_title"
    case fuzzyTitle = "fuzzy_title"
    case body
    case recent
}

struct SearchResult: Codable, Identifiable {
    let note: NoteSummary
    let snippet: String
    let reason: SearchReason
    var id: RustIdentity { note.id }
}

enum SelectionAffinity: String, Codable { case upstream = "Upstream", downstream = "Downstream" }

struct RustSelection: Codable, Equatable {
    let anchor: Int
    let head: Int
    let affinity: SelectionAffinity
    let revision: UInt64
}

struct EditorSnapshot: Codable, Equatable {
    let revision: UInt64
    let source: String
    let selections: [RustSelection]
    let can_undo: Bool
    let can_redo: Bool

    static let empty = EditorSnapshot(revision: 0, source: "", selections: [], can_undo: false, can_redo: false)
}

struct TextRange: Codable, Equatable {
    let start: Int
    let end: Int

    init(_ range: Range<Int>) {
        start = range.lowerBound
        end = range.upperBound
    }
}

enum DecorationKind: Decodable, Equatable {
    case emphasis, strong, link, code, listItem
    case heading(Int)
    case checkbox(Bool)

    private enum ObjectKeys: String, CodingKey { case heading = "Heading", checkbox = "Checkbox" }

    init(from decoder: Decoder) throws {
        let single = try decoder.singleValueContainer()
        if let value = try? single.decode(String.self) {
            switch value {
            case "Emphasis": self = .emphasis
            case "Strong": self = .strong
            case "Link": self = .link
            case "Code": self = .code
            case "ListItem": self = .listItem
            default: throw DecodingError.dataCorruptedError(in: single, debugDescription: "Unknown decoration kind")
            }
            return
        }
        let object = try decoder.container(keyedBy: ObjectKeys.self)
        if let level = try object.decodeIfPresent(Int.self, forKey: .heading) {
            self = .heading(level)
        } else if let checked = try object.decodeIfPresent(Bool.self, forKey: .checkbox) {
            self = .checkbox(checked)
        } else {
            throw DecodingError.dataCorrupted(.init(codingPath: decoder.codingPath, debugDescription: "Unknown decoration kind"))
        }
    }
}

struct Decoration: Decodable, Equatable {
    let range: TextRange
    let kind: DecorationKind
}

struct DecorationSet: Decodable, Equatable {
    let revision: UInt64
    let items: [Decoration]
}

struct EditorPresentationState: Decodable, Equatable {
    let snapshot: EditorSnapshot
    let decorations: DecorationSet
}

enum DurabilityState: String, Codable { case accepted, recoveryDurable = "recovery_durable", fileSaved = "file_saved" }
enum ReplacementSafety: String, Codable { case safe, mustRetainEditor = "must_retain_editor" }
enum RecoveryCleanup: String, Codable { case removed, alreadyAbsent = "already_absent", retained }
enum IndexState: String, Codable { case current, stale }

enum PersistenceIssueKind: String, Codable {
    case recoveryWrite = "recovery_write"
    case canonicalWrite = "canonical_write"
    case canonicalDurabilityUncertain = "canonical_durability_uncertain"
    case recoveryCleanup = "recovery_cleanup"
    case indexStale = "index_stale"
    case recencyUpdate = "recency_update"
}

struct PersistenceIssue: Codable, Equatable {
    let kind: PersistenceIssueKind
    let diagnostic: String
}

struct PersistenceState: Codable, Equatable {
    let durability: DurabilityState
    let replacement_safety: ReplacementSafety
    let issues: [PersistenceIssue]
}

struct ConflictState: Codable, Equatable {
    let external_source: String
    let external_hash: String
}

struct PendingNativeDraftState: Codable, Equatable {
    let base_revision: UInt64
    let source: String
    let durable: Bool
}

struct ActiveEditorState: Decodable, Equatable {
    let note_id: RustIdentity
    let editor: EditorPresentationState
    let persistence: PersistenceState
    let conflict: ConflictState?
    let pending_native_draft: PendingNativeDraftState?
    let generation: UInt64
}

struct FolderState: Codable, Equatable {
    let path: String
    let adopted: Bool
    let generation: UInt64
}

struct RecoveryDraft: Codable, Equatable {
    let note_id: String
    let relative_path: String
    let revision: UInt64
    let base_hash: String
    let source: String
}

struct BackgroundTaskState: Codable, Equatable {
    let id: String
    let operation: String
}

struct ApplicationState: Decodable, Equatable {
    let folder: FolderState?
    let active: ActiveEditorState?
    let recoveries: [RecoveryDraft]
    let background_tasks: [BackgroundTaskState]

    static let empty = ApplicationState(folder: nil, active: nil, recoveries: [], background_tasks: [])
}

enum ProblemCode: String, Codable {
    case notConnected = "not_connected"
    case noteNotFound = "note_not_found"
    case recoveryNotFound = "recovery_not_found"
    case recoveryPending = "recovery_pending"
    case staleRevision = "stale_revision"
    case externalConflict = "external_conflict"
    case identityChanged = "identity_changed"
    case staleEditor = "stale_editor"
    case wrongOwner = "wrong_owner"
    case destinationExists = "destination_exists"
    case duplicateIdentity = "duplicate_identity"
    case invalidOperation = "invalid_operation"
    case persistenceFailure = "persistence_failure"
    case taskNotFound = "task_not_found"
    case contentHashMismatch = "content_hash_mismatch"
    case adoptionRequired = "adoption_required"
    case databaseFailure = "database_failure"
}

enum ProblemDetails: Decodable, Equatable {
    case staleRevision(expected: UInt64, current: UInt64)
    case externalConflict(source: String, hash: String)
    case recoveryPending(noteID: RustIdentity)
    case persistence(issues: [PersistenceIssue])
    case contentHashMismatch(expected: String, current: String)
    case adoptionRequired(folderPath: String)

    private enum CodingKeys: String, CodingKey {
        case kind, expected_revision, current_revision, external_source, external_hash, note_id, issues
        case expected_hash, current_hash, folder_path
    }
    private enum Kind: String, Decodable {
        case staleRevision = "stale_revision", externalConflict = "external_conflict"
        case recoveryPending = "recovery_pending", persistence
        case contentHashMismatch = "content_hash_mismatch", adoptionRequired = "adoption_required"
    }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(Kind.self, forKey: .kind) {
        case .staleRevision:
            self = .staleRevision(expected: try values.decode(UInt64.self, forKey: .expected_revision), current: try values.decode(UInt64.self, forKey: .current_revision))
        case .externalConflict:
            self = .externalConflict(source: try values.decode(String.self, forKey: .external_source), hash: try values.decode(String.self, forKey: .external_hash))
        case .recoveryPending: self = .recoveryPending(noteID: try values.decode(RustIdentity.self, forKey: .note_id))
        case .persistence: self = .persistence(issues: try values.decode([PersistenceIssue].self, forKey: .issues))
        case .contentHashMismatch:
            self = .contentHashMismatch(expected: try values.decode(String.self, forKey: .expected_hash), current: try values.decode(String.self, forKey: .current_hash))
        case .adoptionRequired: self = .adoptionRequired(folderPath: try values.decode(String.self, forKey: .folder_path))
        }
    }
}

struct ApplicationProblem: Decodable, Equatable {
    let code: ProblemCode
    let diagnostic: String
    let details: ProblemDetails?
}

enum OperationOutcome<Value: Decodable>: Decodable {
    case applied(Value)
    case rejected(ApplicationProblem)

    private enum CodingKeys: String, CodingKey { case status, value }
    private enum Status: String, Decodable { case applied, rejected }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Status.self, forKey: .status) {
        case .applied: self = .applied(try container.decode(Value.self, forKey: .value))
        case .rejected: self = .rejected(try container.decode(ApplicationProblem.self, forKey: .value))
        }
    }
}

struct SaveTarget: Codable, Equatable {
    let note_id: RustIdentity
    let revision: UInt64
    let generation: UInt64
}

enum HostEffect: Decodable, Equatable {
    case scheduleAutosave(id: String, delayMilliseconds: UInt64, target: SaveTarget)
    case cancel(id: String)

    private enum CodingKeys: String, CodingKey { case kind, effect_id, delay_ms, target }
    private enum Kind: String, Decodable { case scheduleAutosave = "schedule_autosave", cancelEffect = "cancel_effect" }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .kind) {
        case .scheduleAutosave:
            self = .scheduleAutosave(id: try container.decode(String.self, forKey: .effect_id), delayMilliseconds: try container.decode(UInt64.self, forKey: .delay_ms), target: try container.decode(SaveTarget.self, forKey: .target))
        case .cancelEffect: self = .cancel(id: try container.decode(String.self, forKey: .effect_id))
        }
    }
}

struct ApplicationResponse<Value: Decodable>: Decodable {
    let state: ApplicationState
    let effects: [HostEffect]
    let outcome: OperationOutcome<Value>
}

struct EmptyResult: Decodable, Equatable {
    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        guard container.decodeNil() else {
            throw DecodingError.typeMismatch(EmptyResult.self, .init(codingPath: decoder.codingPath, debugDescription: "Expected null"))
        }
    }
}

struct ConnectResult: Decodable { let opened_note: NoteSummary? }
struct NoteResult: Decodable { let note: NoteSummary }
struct TrashResult: Decodable { let trash_path: String }

struct EditResult: Decodable, Equatable {
    let revision: UInt64
    let changed_ranges: [TextRange]
    let selections: [RustSelection]
    let decorations: [Decoration]
}

struct SaveOutcome: Decodable, Equatable {
    let revision: UInt64
    let durability: DurabilityState
    let recovery_cleanup: RecoveryCleanup
    let recovery_cleanup_error: String?
    let index_state: IndexState
    let index_error: String?
    let recency_error: String?
    let canonical_error: String?
}

struct SessionSaveResult: Decodable { let save: SaveOutcome }

enum ReconcileOutcome: Decodable, Equatable {
    case unchanged
    case refreshed(revision: UInt64)
    case conflict(externalSource: String)

    private enum CodingKeys: String, CodingKey { case status, revision, external_source }
    private enum Status: String, Decodable { case unchanged, refreshed, conflict }

    init(from decoder: Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(Status.self, forKey: .status) {
        case .unchanged: self = .unchanged
        case .refreshed: self = .refreshed(revision: try values.decode(UInt64.self, forKey: .revision))
        case .conflict: self = .conflict(externalSource: try values.decode(String.self, forKey: .external_source))
        }
    }
}

struct Diagnostic: Decodable, Equatable {
    let severity: String
    let code: String
    let relative_path: String?
    let message: String
}

struct DiagnosticBundle: Decodable {
    let application_version: String
    let editor_version: String
    let index_schema: UInt32
    let state_schema: UInt32
    let adopted: Bool
    let note_count: UInt64
    let recovery_count: UInt64
    let diagnostics: [Diagnostic]
}

struct NativeTextEdit: Encodable, Equatable {
    struct Replacement: Encodable, Equatable { let range: TextRange; let text: String }
    enum History: String, Encodable { case typing = "Typing", paste = "Paste", formatting = "Formatting", isolated = "Isolated" }
    struct Composition: Encodable, Equatable { let original_range: TextRange; let original_text: String }

    let expected_revision: UInt64
    let replacements: [Replacement]
    let selections: [RustSelection]
    let history: History
    let composition: Composition?
}

struct PendingNativeDraft: Encodable, Equatable {
    let base_revision: UInt64
    let source: String
}

enum PendingDraftResolution: Encodable, Equatable {
    case saveAsNew(operationID: String, title: String?)
    case discard

    private enum CodingKeys: String, CodingKey { case resolution, operation_id, title }

    func encode(to encoder: Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .saveAsNew(operationID, title):
            try values.encode("save_as_new", forKey: .resolution)
            try values.encode(operationID, forKey: .operation_id)
            try values.encodeIfPresent(title, forKey: .title)
        case .discard:
            try values.encode("discard", forKey: .resolution)
        }
    }
}

private struct ConnectRequest: Encodable {
    let folder_path: String
    let application_state_path: String
    let adopt: Bool
    let create_missing: Bool
}

private struct CreateNoteRequest: Encodable { let source: String? }
private struct SearchQuery: Encodable { let query: String; let limit: Int }

@MainActor
protocol ApplicationSessionProtocol: AnyObject {
    func state() throws -> ApplicationResponse<EmptyResult>
    func connect(path: String, statePath: String, create: Bool) throws -> ApplicationResponse<ConnectResult>
    func createNote(source: String?) throws -> ApplicationResponse<NoteResult>
    func openNote(id: String) throws -> ApplicationResponse<NoteResult>
    func closeNote() throws -> ApplicationResponse<EmptyResult>
    func apply(_ edit: NativeTextEdit) throws -> ApplicationResponse<EditResult>
    func preservePendingNativeDraft(_ draft: PendingNativeDraft) throws -> ApplicationResponse<EmptyResult>
    func resolvePendingNativeDraft(_ resolution: PendingDraftResolution) throws -> ApplicationResponse<NoteResult>
    func undo(revision: UInt64) throws -> ApplicationResponse<EditResult?>
    func redo(revision: UInt64) throws -> ApplicationResponse<EditResult?>
    func save(target: SaveTarget) throws -> ApplicationResponse<SessionSaveResult>
    func restoreRecovery(id: String) throws -> ApplicationResponse<NoteResult>
    func discardRecovery(id: String) throws -> ApplicationResponse<EmptyResult>
    func reconcileActive() throws -> ApplicationResponse<ReconcileOutcome>
    func search(_ query: String, limit: Int) throws -> ApplicationResponse<[SearchResult]>
}

@MainActor
final class RustApplicationSession: ApplicationSessionProtocol {
    private var handle: OpaquePointer?

    init() throws {
        guard howler_session_abi_version() == 2 else { throw RustError.invalidResponse }
        try check(howler_session_create(&handle))
        guard handle != nil else { throw RustError.invalidResponse }
    }

    deinit { howler_session_destroy(handle) }

    func state() throws -> ApplicationResponse<EmptyResult> { try output(howler_session_state_json) }

    func connect(path: String, statePath: String, create: Bool) throws -> ApplicationResponse<ConnectResult> {
        try input(ConnectRequest(folder_path: path, application_state_path: statePath, adopt: false, create_missing: create), howler_session_connect_json)
    }

    func createNote(source: String?) throws -> ApplicationResponse<NoteResult> {
        try input(CreateNoteRequest(source: source), howler_session_create_note_json)
    }

    func openNote(id: String) throws -> ApplicationResponse<NoteResult> { try stringInput(id, howler_session_open_note_json) }
    func closeNote() throws -> ApplicationResponse<EmptyResult> { try output(howler_session_close_note_json) }
    func apply(_ edit: NativeTextEdit) throws -> ApplicationResponse<EditResult> { try input(edit, howler_session_apply_text_edit_json) }

    func preservePendingNativeDraft(_ draft: PendingNativeDraft) throws -> ApplicationResponse<EmptyResult> {
        try input(draft, howler_session_preserve_pending_native_draft_json)
    }

    func resolvePendingNativeDraft(_ resolution: PendingDraftResolution) throws -> ApplicationResponse<NoteResult> {
        try input(resolution, howler_session_resolve_pending_native_draft_json)
    }

    func undo(revision: UInt64) throws -> ApplicationResponse<EditResult?> {
        try revisionInput(revision, howler_session_undo_json)
    }

    func redo(revision: UInt64) throws -> ApplicationResponse<EditResult?> {
        try revisionInput(revision, howler_session_redo_json)
    }

    func save(target: SaveTarget) throws -> ApplicationResponse<SessionSaveResult> { try input(target, howler_session_save_json) }
    func restoreRecovery(id: String) throws -> ApplicationResponse<NoteResult> { try stringInput(id, howler_session_restore_recovery_json) }
    func discardRecovery(id: String) throws -> ApplicationResponse<EmptyResult> { try stringInput(id, howler_session_discard_recovery_json) }
    func reconcileActive() throws -> ApplicationResponse<ReconcileOutcome> { try output(howler_session_reconcile_active_json) }

    func search(_ query: String, limit: Int = 40) throws -> ApplicationResponse<[SearchResult]> {
        try input(SearchQuery(query: query, limit: limit), howler_session_search_json)
    }

    private func input<Input: Encodable, Output: Decodable>(
        _ input: Input,
        _ operation: (OpaquePointer?, UnsafePointer<CChar>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?) -> Int32
    ) throws -> Output {
        let data = try JSONEncoder().encode(input)
        guard let json = String(data: data, encoding: .utf8) else { throw RustError.invalidResponse }
        return try json.withCString { pointer in try callSessionJSON { operation(handle, pointer, $0, $1) } }
    }

    private func stringInput<Output: Decodable>(
        _ input: String,
        _ operation: (OpaquePointer?, UnsafePointer<CChar>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?) -> Int32
    ) throws -> Output {
        try input.withCString { pointer in try callSessionJSON { operation(handle, pointer, $0, $1) } }
    }

    private func output<Output: Decodable>(
        _ operation: (OpaquePointer?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?) -> Int32
    ) throws -> Output {
        try callSessionJSON { operation(handle, $0, $1) }
    }

    private func revisionInput<Output: Decodable>(
        _ revision: UInt64,
        _ operation: (OpaquePointer?, UInt64, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?) -> Int32
    ) throws -> Output {
        try callSessionJSON { operation(handle, revision, $0, $1) }
    }
}

private struct BoundaryProblem: Decodable { let diagnostic: String }

private func callSessionJSON<T: Decodable>(
    _ operation: (UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
) throws -> T {
    var response: UnsafeMutablePointer<CChar>?
    var boundary: UnsafeMutablePointer<CChar>?
    let code = operation(&response, &boundary)
    defer {
        howler_session_string_free(response)
        howler_session_string_free(boundary)
    }
    guard code == HOWLER_APPLICATION_OK else {
        if let boundary,
           let problem = try? JSONDecoder().decode(BoundaryProblem.self, from: Data(bytes: boundary, count: strlen(boundary))) {
            throw RustError.code(code, problem.diagnostic)
        }
        throw RustError.code(code, errorMessage(code))
    }
    guard let response else { throw RustError.invalidResponse }
    return try JSONDecoder().decode(T.self, from: Data(bytes: response, count: strlen(response)))
}

private func check(_ code: Int32) throws {
    guard code == HOWLER_APPLICATION_OK else { throw RustError.code(code, errorMessage(code)) }
}

private func errorMessage(_ code: Int32) -> String {
    howler_application_error_message(code).map(String.init(cString:)) ?? "Unknown Rust error"
}
