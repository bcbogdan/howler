import Foundation

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

private func check(_ code: Int32) throws {
    guard code == 0 else {
        let message = howler_application_error_message(code).map(String.init(cString:)) ?? "Unknown Rust error"
        throw RustError.code(code, message)
    }
}

@_silgen_name("howler_application_error_message") private func howler_application_error_message(_ code: Int32) -> UnsafePointer<CChar>?
@_silgen_name("howler_folder_open") private func howler_folder_open(_ path: UnsafePointer<CChar>, _ state: UnsafePointer<CChar>, _ adopt: Int32, _ output: UnsafeMutablePointer<OpaquePointer?>) -> Int32
@_silgen_name("howler_folder_create") private func howler_folder_create(_ path: UnsafePointer<CChar>, _ state: UnsafePointer<CChar>, _ adopt: Int32, _ output: UnsafeMutablePointer<OpaquePointer?>) -> Int32
@_silgen_name("howler_folder_create_note") private func howler_folder_create_note(_ folder: OpaquePointer?, _ source: UnsafePointer<CChar>?, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_folder_open_editor") private func howler_folder_open_editor(_ folder: OpaquePointer?, _ id: UnsafePointer<CChar>, _ output: UnsafeMutablePointer<OpaquePointer?>) -> Int32
@_silgen_name("howler_folder_restore_recovery") private func howler_folder_restore_recovery(_ folder: OpaquePointer?, _ id: UnsafePointer<CChar>, _ output: UnsafeMutablePointer<OpaquePointer?>) -> Int32
@_silgen_name("howler_folder_rename_title") private func howler_folder_rename_title(_ folder: OpaquePointer?, _ id: UnsafePointer<CChar>, _ title: UnsafePointer<CChar>, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_folder_move_note") private func howler_folder_move_note(_ folder: OpaquePointer?, _ id: UnsafePointer<CChar>, _ destination: UnsafePointer<CChar>, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_folder_trash_note") private func howler_folder_trash_note(_ folder: OpaquePointer?, _ id: UnsafePointer<CChar>, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_folder_restore_note") private func howler_folder_restore_note(_ folder: OpaquePointer?, _ path: UnsafePointer<CChar>, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_folder_recoveries") private func howler_folder_recoveries(_ folder: OpaquePointer?, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_folder_discard_recovery") private func howler_folder_discard_recovery(_ folder: OpaquePointer?, _ id: UnsafePointer<CChar>) -> Int32
@_silgen_name("howler_folder_search") private func howler_folder_search(_ folder: OpaquePointer?, _ query: UnsafePointer<CChar>, _ limit: Int, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_folder_rescan") private func howler_folder_rescan(_ folder: OpaquePointer?, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_note_editor_snapshot") private func howler_note_editor_snapshot(_ editor: OpaquePointer?, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_note_editor_apply_json") private func howler_note_editor_apply_json(_ editor: OpaquePointer?, _ transaction: UnsafePointer<CChar>, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_note_editor_command_json") private func howler_note_editor_command_json(_ editor: OpaquePointer?, _ revision: UInt64, _ command: UnsafePointer<CChar>, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_note_editor_undo") private func howler_note_editor_undo(_ editor: OpaquePointer?, _ revision: UInt64, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_note_editor_redo") private func howler_note_editor_redo(_ editor: OpaquePointer?, _ revision: UInt64, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_note_editor_save") private func howler_note_editor_save(_ folder: OpaquePointer?, _ editor: OpaquePointer?, _ expectedRevision: UInt64, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_note_editor_reconcile") private func howler_note_editor_reconcile(_ editor: OpaquePointer?, _ output: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>) -> Int32
@_silgen_name("howler_folder_destroy") private func howler_folder_destroy(_ folder: OpaquePointer?)
@_silgen_name("howler_note_editor_destroy") private func howler_note_editor_destroy(_ editor: OpaquePointer?)
@_silgen_name("howler_application_string_free") private func howler_application_string_free(_ string: UnsafeMutablePointer<CChar>?)
