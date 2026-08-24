import Foundation
import CHowlerApplication

enum RustError: LocalizedError {
    case code(Int32, String)
    case invalidResponse

    var errorDescription: String? {
        switch self {
        case let .code(_, message): message
        case .invalidResponse: "Rust returned an invalid response"
        }
    }
}

struct RustIdentity: Codable, Hashable {
    let kind: String
    let value: String
}

struct NoteSummary: Codable, Identifiable, Hashable {
    let id: RustIdentity
    let relative_path: String
    let title: String
    let content_hash: String
}

struct SearchResult: Codable, Identifiable {
    let note: NoteSummary
    let snippet: String
    let reason: String
    var id: RustIdentity { note.id }
}

struct EditorSnapshot: Codable, Equatable {
    let revision: UInt64
    let source: String
    let selections: [RustSelection]
    let can_undo: Bool
    let can_redo: Bool

    static let empty = EditorSnapshot(revision: 0, source: "", selections: [], can_undo: false, can_redo: false)
}

struct RustSelection: Codable, Equatable {
    let anchor: Int
    let head: Int
    let affinity: String
    let revision: UInt64
}

struct MutationOutcome: Codable {
    let durability: String
    let recovery_error: String?
}

struct SaveOutcome: Codable {
    let revision: UInt64
    let durability: String
    let recovery_cleanup: String
    let recovery_cleanup_error: String?
    let index_state: String
    let index_error: String?
    let recency_error: String?
    let canonical_error: String?
}

struct RecoveryDraft: Codable {
    let note_id: String
    let relative_path: String
    let revision: UInt64
    let base_hash: String
    let source: String
}

struct ReconcileOutcome: Codable {
    let status: String
    let revision: UInt64?
    let external_source: String?
}

private struct Replacement: Encodable {
    let range: ByteRange
    let text: String
}

private struct ByteRange: Encodable {
    let start: Int
    let end: Int
}

private struct EditTransaction: Encodable {
    let expected_revision: UInt64
    let replacements: [Replacement]
    let selections: [RustSelection]
    let history: String
}

final class RustFolder {
    private(set) var handle: OpaquePointer?

    init(path: String, statePath: String, adopt: Bool = false, create: Bool = false) throws {
        let code = path.withCString { pathPointer in
            statePath.withCString { statePointer in
                if create {
                    howler_folder_create(pathPointer, statePointer, adopt ? 1 : 0, &handle)
                } else {
                    howler_folder_open(pathPointer, statePointer, adopt ? 1 : 0, &handle)
                }
            }
        }
        try check(code)
    }

    deinit { howler_folder_destroy(handle) }

    func createNote(source: String = "") throws -> NoteSummary {
        try source.withCString { pointer in
            try callJSON { howler_folder_create_note(handle, pointer, $0) }
        }
    }

    func openEditor(id: String) throws -> RustNoteEditor {
        var editor: OpaquePointer?
        let code = id.withCString { howler_folder_open_editor(handle, $0, &editor) }
        try check(code)
        guard let editor else { throw RustError.invalidResponse }
        return RustNoteEditor(handle: editor, folder: self)
    }

    func search(_ query: String, limit: Int = 40) throws -> [SearchResult] {
        try query.withCString { pointer in
            try callJSON { howler_folder_search(handle, pointer, limit, $0) }
        }
    }

    func renameTitle(id: String, title: String) throws -> NoteSummary {
        try id.withCString { idPointer in
            try title.withCString { titlePointer in
                try callJSON { howler_folder_rename_title(handle, idPointer, titlePointer, $0) }
            }
        }
    }

    func moveNote(id: String, destination: String) throws -> NoteSummary {
        try id.withCString { idPointer in
            try destination.withCString { destinationPointer in
                try callJSON { howler_folder_move_note(handle, idPointer, destinationPointer, $0) }
            }
        }
    }

    func trashNote(id: String) throws -> String {
        try id.withCString { pointer in
            try callJSON { howler_folder_trash_note(handle, pointer, $0) }
        }
    }

    func restoreNote(path: String) throws -> NoteSummary {
        try path.withCString { pointer in
            try callJSON { howler_folder_restore_note(handle, pointer, $0) }
        }
    }

    func recoveries() throws -> [RecoveryDraft] {
        try callJSON { howler_folder_recoveries(handle, $0) }
    }

    func restoreRecovery(id: String) throws -> RustNoteEditor {
        var editor: OpaquePointer?
        let code = id.withCString { howler_folder_restore_recovery(handle, $0, &editor) }
        try check(code)
        guard let editor else { throw RustError.invalidResponse }
        return RustNoteEditor(handle: editor, folder: self)
    }

    func discardRecovery(id: String) throws {
        try id.withCString { try check(howler_folder_discard_recovery(handle, $0)) }
    }

    func rescan() throws {
        let _: [String: JSONValue] = try callJSON { howler_folder_rescan(handle, $0) }
    }
}

final class RustNoteEditor {
    private(set) var handle: OpaquePointer?
    private let folder: RustFolder

    fileprivate init(handle: OpaquePointer, folder: RustFolder) {
        self.handle = handle
        self.folder = folder
    }

    deinit { howler_note_editor_destroy(handle) }

    func snapshot() throws -> EditorSnapshot {
        try callJSON { howler_note_editor_snapshot(handle, $0) }
    }

    func apply(range: Range<Int>, replacement: String, selection: Int, revision: UInt64) throws -> MutationOutcome {
        let transaction = EditTransaction(
            expected_revision: revision,
            replacements: [Replacement(range: ByteRange(start: range.lowerBound, end: range.upperBound), text: replacement)],
            selections: [RustSelection(anchor: selection, head: selection, affinity: "Downstream", revision: revision + 1)],
            history: "Typing"
        )
        let data = try JSONEncoder().encode(transaction)
        guard let json = String(data: data, encoding: .utf8) else { throw RustError.invalidResponse }
        return try json.withCString { pointer in
            try callJSON { howler_note_editor_apply_json(handle, pointer, $0) }
        }
    }

    func undo(expectedRevision: UInt64) throws -> MutationOutcome {
        try callJSON { howler_note_editor_undo(handle, expectedRevision, $0) }
    }

    func redo(expectedRevision: UInt64) throws -> MutationOutcome {
        try callJSON { howler_note_editor_redo(handle, expectedRevision, $0) }
    }

    func save(expectedRevision: UInt64) throws -> SaveOutcome {
        try callJSON { howler_note_editor_save(folder.handle, handle, expectedRevision, $0) }
    }

    func reconcile() throws -> ReconcileOutcome {
        try callJSON { howler_note_editor_reconcile(handle, $0) }
    }

    func command<T: Encodable>(_ command: T, revision: UInt64) throws -> MutationOutcome {
        let data = try JSONEncoder().encode(command)
        guard let json = String(data: data, encoding: .utf8) else { throw RustError.invalidResponse }
        return try json.withCString { pointer in
            try callJSON { howler_note_editor_command_json(handle, revision, pointer, $0) }
        }
    }
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

struct ApplicationProblem: Codable {
    let code: ProblemCode
    let diagnostic: String
    let details: JSONValue?
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

struct FolderState: Codable {
    let path: String
    let adopted: Bool
    let generation: UInt64
}

struct DecorationSet: Codable {
    let revision: UInt64
    let items: [JSONValue]
}

struct EditorPresentationState: Codable {
    let snapshot: EditorSnapshot
    let decorations: DecorationSet
}

struct PersistenceState: Codable {
    let durability: String
    let replacement_safety: String
    let issues: [JSONValue]
}

struct ConflictState: Codable {
    let external_source: String
    let external_hash: String
}

struct PendingNativeDraftState: Codable {
    let base_revision: UInt64
    let source: String
    let durable: Bool
}

struct ActiveEditorState: Codable {
    let note_id: RustIdentity
    let editor: EditorPresentationState
    let persistence: PersistenceState
    let conflict: ConflictState?
    let pending_native_draft: PendingNativeDraftState?
    let generation: UInt64
}

struct ApplicationState: Codable {
    let folder: FolderState?
    let active: ActiveEditorState?
    let recoveries: [RecoveryDraft]
    let background_tasks: [JSONValue]

    static let empty = ApplicationState(folder: nil, active: nil, recoveries: [], background_tasks: [])
}

struct ApplicationResponse<Value: Decodable>: Decodable {
    let state: ApplicationState
    let effects: [HostEffect]
    let outcome: OperationOutcome<Value>
}

struct SaveTarget: Codable {
    let note_id: RustIdentity
    let revision: UInt64
    let generation: UInt64
}

enum HostEffect: Decodable {
    case scheduleAutosave(id: String, delayMilliseconds: UInt64, target: SaveTarget)
    case cancel(id: String)

    private enum CodingKeys: String, CodingKey { case kind, effect_id, delay_ms, target }
    private enum Kind: String, Decodable { case scheduleAutosave = "schedule_autosave", cancelEffect = "cancel_effect" }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .kind) {
        case .scheduleAutosave:
            self = .scheduleAutosave(
                id: try container.decode(String.self, forKey: .effect_id),
                delayMilliseconds: try container.decode(UInt64.self, forKey: .delay_ms),
                target: try container.decode(SaveTarget.self, forKey: .target)
            )
        case .cancelEffect:
            self = .cancel(id: try container.decode(String.self, forKey: .effect_id))
        }
    }
}

struct ConnectResult: Codable { let opened_note: NoteSummary? }
struct NoteResult: Codable { let note: NoteSummary }
struct SessionSaveResult: Codable { let save: SaveOutcome }

private struct ConnectRequest: Encodable {
    let folder_path: String
    let application_state_path: String
    let adopt: Bool
    let create_missing: Bool
}

private struct SessionTextEdit: Encodable {
    let expected_revision: UInt64
    let replacements: [Replacement]
    let selections: [RustSelection]
    let history: String
    let composition: JSONValue? = nil
}

protocol ApplicationSessionProtocol: AnyObject {
    func connect(path: String, statePath: String, create: Bool) throws -> ApplicationResponse<ConnectResult>
    func apply(range: Range<Int>, replacement: String, selection: Int, revision: UInt64) throws -> ApplicationResponse<JSONValue>
    func save(target: SaveTarget) throws -> ApplicationResponse<SessionSaveResult>
}

final class RustApplicationSession: ApplicationSessionProtocol {
    private var handle: OpaquePointer?

    init() throws {
        guard howler_session_abi_version() == 2 else { throw RustError.invalidResponse }
        try check(howler_session_create(&handle))
        guard handle != nil else { throw RustError.invalidResponse }
    }

    deinit { howler_session_destroy(handle) }

    func connect(path: String, statePath: String, create: Bool) throws -> ApplicationResponse<ConnectResult> {
        try call(
            ConnectRequest(
                folder_path: path,
                application_state_path: statePath,
                adopt: false,
                create_missing: create
            ),
            howler_session_connect_json
        )
    }

    func apply(range: Range<Int>, replacement: String, selection: Int, revision: UInt64) throws -> ApplicationResponse<JSONValue> {
        try call(
            SessionTextEdit(
                expected_revision: revision,
                replacements: [Replacement(range: ByteRange(start: range.lowerBound, end: range.upperBound), text: replacement)],
                selections: [RustSelection(anchor: selection, head: selection, affinity: "Downstream", revision: revision + 1)],
                history: "Typing"
            ),
            howler_session_apply_text_edit_json
        )
    }

    func save(target: SaveTarget) throws -> ApplicationResponse<SessionSaveResult> {
        try call(target, howler_session_save_json)
    }

    private func call<Input: Encodable, Output: Decodable>(
        _ input: Input,
        _ operation: (OpaquePointer?, UnsafePointer<CChar>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?, UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?) -> Int32
    ) throws -> Output {
        let data = try JSONEncoder().encode(input)
        guard let json = String(data: data, encoding: .utf8) else { throw RustError.invalidResponse }
        return try json.withCString { pointer in
            try callSessionJSON { operation(handle, pointer, $0, $1) }
        }
    }
}

enum JSONValue: Codable {
    case string(String), number(Double), bool(Bool), object([String: JSONValue]), array([JSONValue]), null

    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if value.decodeNil() { self = .null }
        else if let decoded = try? value.decode(Bool.self) { self = .bool(decoded) }
        else if let decoded = try? value.decode(Double.self) { self = .number(decoded) }
        else if let decoded = try? value.decode(String.self) { self = .string(decoded) }
        else if let decoded = try? value.decode([String: JSONValue].self) { self = .object(decoded) }
        else { self = .array(try value.decode([JSONValue].self)) }
    }

    func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self {
        case let .string(decoded): try value.encode(decoded)
        case let .number(decoded): try value.encode(decoded)
        case let .bool(decoded): try value.encode(decoded)
        case let .object(decoded): try value.encode(decoded)
        case let .array(decoded): try value.encode(decoded)
        case .null: try value.encodeNil()
        }
    }
}

private func callJSON<T: Decodable>(_ operation: (UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32) throws -> T {
    var output: UnsafeMutablePointer<CChar>?
    let code = operation(&output)
    try check(code)
    guard let output else { throw RustError.invalidResponse }
    defer { howler_application_string_free(output) }
    return try JSONDecoder().decode(T.self, from: Data(bytes: output, count: strlen(output)))
}

private struct BoundaryProblem: Decodable {
    let code: String
    let diagnostic: String
}

private func callSessionJSON<T: Decodable>(
    _ operation: (
        UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>,
        UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>
    ) -> Int32
) throws -> T {
    var response: UnsafeMutablePointer<CChar>?
    var boundary: UnsafeMutablePointer<CChar>?
    let code = operation(&response, &boundary)
    defer {
        howler_session_string_free(response)
        howler_session_string_free(boundary)
    }
    guard code == 0 else {
        if let boundary,
           let problem = try? JSONDecoder().decode(
               BoundaryProblem.self,
               from: Data(bytes: boundary, count: strlen(boundary))
           ) {
            throw RustError.code(code, problem.diagnostic)
        }
        try check(code)
        throw RustError.invalidResponse
    }
    guard let response else { throw RustError.invalidResponse }
    return try JSONDecoder().decode(T.self, from: Data(bytes: response, count: strlen(response)))
}

private func check(_ code: Int32) throws {
    guard code == 0 else {
        let message = howler_application_error_message(code).map(String.init(cString:)) ?? "Unknown Rust error"
        throw RustError.code(code, message)
    }
}
