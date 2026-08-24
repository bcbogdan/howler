# Application Session ABI v2 JSON

Status: partial implementation contract. The normative structural contract is
`docs/schema/application-session-v2.schema.json`; C transport and ownership rules are normative in
`ffi/application/include/howler_application.h`.

The schema root, `$defs/applicationResponse`, is the normative response fragment for every
successful `*_json` transport call. Each operation's input fragment is the named `$defs` entry in
the table below; functions documented with no input or a borrowed string have no JSON request
fragment. `$defs/appliedValue` lists the concrete response values used by the operation matrix.

Wire objects are extensible: unknown optional object fields are allowed and may be ignored by
tolerant decoders. Required fields remain required, and unknown required enum values must be
rejected. Rust currently serializes all fields shown by the schema. Paths are UTF-8 JSON strings.
Text offsets are UTF-8 byte offsets. Editor enum values inherited from `howler-editor` are
PascalCase; application enum tags are snake_case.

## Operations

| C function | Input contract | Applied `value` |
| --- | --- | --- |
| `howler_session_state_json` | none | `null` |
| `howler_session_connect_json` | `connectRequest` | `{ "opened_note": NoteSummary|null }` |
| `howler_session_adopt_folder_json` | none | connect result |
| `howler_session_create_note_json` | `createNoteRequest` | `{ "note": NoteSummary }` |
| `howler_session_open_note_json` | borrowed note-ID string | note result |
| `howler_session_close_note_json` | none | `null` |
| `howler_session_apply_text_edit_json` | `hostTextEdit` | editor `EditResult` |
| `howler_session_preserve_pending_native_draft_json` | `pendingNativeDraft` | `null` |
| `howler_session_resolve_pending_native_draft_json` | `pendingDraftResolution` | note result |
| `howler_session_execute_command_json` | expected revision argument plus editor command JSON | editor `EditResult` |
| `howler_session_undo_json` / `howler_session_redo_json` | expected revision argument | `EditResult|null` |
| `howler_session_save_json` | `saveTarget` | `{ "save": SaveOutcome }` |
| `howler_session_resolve_conflict_json` | `conflictResolution` | note result |
| `howler_session_restore_recovery_json` | borrowed note-ID string | note result |
| `howler_session_discard_recovery_json` | borrowed note-ID string | `null` |
| `howler_session_reconcile_active_json` | none | `ReconcileResult` |
| `howler_session_search_json` | `searchQuery` | array of `SearchResult` |
| `howler_session_rename_note_json` | `renameNoteRequest` | note result |
| `howler_session_move_note_json` | `moveNoteRequest` | note result |
| `howler_session_trash_note_json` | borrowed note-ID string | `{ "trash_path": string }` |
| `howler_session_restore_note_json` | `restoreNoteRequest` | note result |
| `howler_session_diagnostics_json` | none | array of `Diagnostic` |
| `howler_session_diagnostic_bundle_json` | none | `DiagnosticBundle` |

`operation_id` is mandatory for both save-as-new variants. It is 1-128 ASCII letters, digits,
periods, hyphens, or underscores. `title` is optional and may be omitted or `null`. A retry must use
the same source-affecting request fields. The same ID and source returns the original committed
note, including after the pending/conflict state has been cleared; the same ID with a different
request or source is rejected.

Editor command JSON follows Serde's externally tagged representation, for example
`{"Bold":{"range":{"start":0,"end":4}}}`. The implemented command variants are `Bold`,
`Emphasis`, `Link`, `UnorderedList`, and `Checkbox` as declared by `howler-editor`.
