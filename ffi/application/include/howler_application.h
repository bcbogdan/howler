#ifndef HOWLER_APPLICATION_H
#define HOWLER_APPLICATION_H
#include <stdint.h>
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif

#define HOWLER_APPLICATION_ABI_VERSION 1
#define HOWLER_SESSION_ABI_VERSION 2
#define HOWLER_APPLICATION_OK 0
#define HOWLER_APPLICATION_INVALID_ARGUMENT 1
#define HOWLER_APPLICATION_STALE_REVISION 2
#define HOWLER_APPLICATION_BUSY 4
#define HOWLER_APPLICATION_NOT_FOUND 5
#define HOWLER_APPLICATION_CONFLICT 6
#define HOWLER_APPLICATION_IO 7
#define HOWLER_APPLICATION_WRONG_OWNER 8
#define HOWLER_APPLICATION_DESTINATION_EXISTS 9
#define HOWLER_APPLICATION_DUPLICATE_ID 10
#define HOWLER_APPLICATION_INTERNAL 255

typedef struct HowlerNoteFolder HowlerNoteFolder;
typedef struct HowlerNoteEditor HowlerNoteEditor;
typedef struct HowlerApplicationSession HowlerApplicationSession;

/* Application-session ABI v2 is partially implemented as documented by ADR-0007.
 *
 * Functions whose names end in _json are synchronous and hold the session lock for their full
 * execution. Lock acquisition does not wait. Only these transport statuses are returned by v2:
 * HOWLER_APPLICATION_OK, HOWLER_APPLICATION_INVALID_ARGUMENT, HOWLER_APPLICATION_BUSY, and
 * HOWLER_APPLICATION_INTERNAL. Other HOWLER_APPLICATION_* statuses are v1-only.
 *
 * On a valid _json call, HOWLER_APPLICATION_OK means out_response_json contains one UTF-8 JSON
 * ApplicationResponse: {"state":...,"effects":[],"outcome":{"status":"applied|rejected",
 * "value":...}}. Domain rejection is transport success and includes authoritative state.
 * howler_session_state_json uses this envelope with null applied data. Version queries, create,
 * destroy, and free do not return ApplicationResponse.
 *
 * A NULL session returns INVALID_ARGUMENT. Passing any non-NULL pointer that is not a live session
 * returned by howler_session_create, including a destroyed handle, violates the caller contract and
 * has undefined behavior. A NULL required input or output slot returns INVALID_ARGUMENT. Invalid
 * UTF-8 and malformed JSON also return INVALID_ARGUMENT. Contention returns BUSY. A poisoned lock
 * or response serialization failure returns INTERNAL.
 *
 * Both required output slots are initialized to NULL before other validation when the slots
 * themselves are non-NULL. Transport failure leaves out_response_json NULL and, when allocation is
 * possible, returns a boundary problem through out_boundary_problem_json. Every non-NULL returned
 * response or boundary string is a distinct Rust allocation owned exclusively by the caller until
 * freed exactly once with howler_session_string_free; it must not be modified, passed to another
 * allocator, or used after free. Input strings and JSON are borrowed only for the duration of the
 * call and must remain valid NUL-terminated UTF-8 until it returns. The session is caller-owned from
 * successful create until one synchronized destroy; destroy(NULL) is a no-op. The caller must ensure
 * no call can overlap destruction.
 *
 * Search and diagnostics currently hold the session lock. Cancellable rescan/rebuild and provider
 * coordinated writes are deferred and are not exposed by this v2 header. No filesystem
 * compare-and-swap or native coordination capability is claimed. */
uint32_t howler_session_abi_version(void);
int32_t howler_session_create(HowlerApplicationSession **out_session);
void howler_session_destroy(HowlerApplicationSession *session);
int32_t howler_session_state_json(HowlerApplicationSession *session, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_capabilities_json(HowlerApplicationSession *session, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_connect_json(HowlerApplicationSession *session, const char *request_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_adopt_folder_json(HowlerApplicationSession *session, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_create_note_json(HowlerApplicationSession *session, const char *request_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_open_note_json(HowlerApplicationSession *session, const char *note_id, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_close_note_json(HowlerApplicationSession *session, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_apply_text_edit_json(HowlerApplicationSession *session, const char *edit_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_apply_selection_json(HowlerApplicationSession *session, const char *selection_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_preserve_pending_native_draft_json(HowlerApplicationSession *session, const char *draft_json, char **out_response_json, char **out_boundary_problem_json);
/* Pending-native storage is independent from normal recovery/autosave. An unresolved pending draft
 * always reports must_retain_editor and must be explicitly resolved before editor replacement. */
int32_t howler_session_resolve_pending_native_draft_json(HowlerApplicationSession *session, const char *resolution_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_execute_command_json(HowlerApplicationSession *session, uint64_t expected_revision, const char *command_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_undo_json(HowlerApplicationSession *session, uint64_t expected_revision, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_redo_json(HowlerApplicationSession *session, uint64_t expected_revision, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_save_json(HowlerApplicationSession *session, const char *target_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_resolve_conflict_json(HowlerApplicationSession *session, const char *resolution_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_restore_recovery_json(HowlerApplicationSession *session, const char *note_id, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_discard_recovery_json(HowlerApplicationSession *session, const char *note_id, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_reconcile_active_json(HowlerApplicationSession *session, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_search_json(HowlerApplicationSession *session, const char *query_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_rename_note_json(HowlerApplicationSession *session, const char *request_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_move_note_json(HowlerApplicationSession *session, const char *request_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_trash_note_json(HowlerApplicationSession *session, const char *note_id, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_restore_note_json(HowlerApplicationSession *session, const char *request_json, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_diagnostics_json(HowlerApplicationSession *session, char **out_response_json, char **out_boundary_problem_json);
int32_t howler_session_diagnostic_bundle_json(HowlerApplicationSession *session, char **out_response_json, char **out_boundary_problem_json);
void howler_session_string_free(char *string);

/* Calls are synchronous and use non-blocking per-handle locks; concurrent use returns BUSY. The
 * caller must synchronize destruction with all use. error_message returns borrowed static storage.
 * Every non-null output pointer is initialized to NULL before other validation. Returned JSON is
 * caller-owned and must be freed with howler_application_string_free. Input strings are
 * NUL-terminated UTF-8; create_note source may be NULL to mean empty, all other inputs are required. */
uint32_t howler_application_abi_version(void);
const char *howler_application_error_message(int32_t code);
int32_t howler_folder_open(const char *path, const char *state_path, int32_t adopt, HowlerNoteFolder **out_folder);
int32_t howler_folder_create(const char *path, const char *state_path, int32_t adopt, HowlerNoteFolder **out_folder);
int32_t howler_folder_create_note(HowlerNoteFolder *folder, const char *source, char **out_json);
/* Note summary JSON: {"id":{"kind":"adopted","value":"01..."},"relative_path":"n.md",
 * "title":"N","content_hash":"..."}. IDs must be implementation-generated ULIDs/hashes. */
int32_t howler_folder_open_editor(HowlerNoteFolder *folder, const char *note_id, HowlerNoteEditor **out_editor);
/* A pending recovery makes normal open return CONFLICT. Restore explicitly returns an editor whose
 * saved base hash is retained, so a changed canonical file conflicts instead of being overwritten. */
int32_t howler_folder_restore_recovery(HowlerNoteFolder *folder, const char *note_id, HowlerNoteEditor **out_editor);
int32_t howler_note_editor_snapshot(HowlerNoteEditor *editor, char **out_json);
int32_t howler_note_editor_apply_json(HowlerNoteEditor *editor, const char *transaction_json, char **out_json);
int32_t howler_note_editor_command_json(HowlerNoteEditor *editor, uint64_t expected_revision, const char *command_json, char **out_json);
int32_t howler_note_editor_undo(HowlerNoteEditor *editor, uint64_t expected_revision, char **out_json);
int32_t howler_note_editor_redo(HowlerNoteEditor *editor, uint64_t expected_revision, char **out_json);
/* Save JSON: {"revision":1,"durability":"file_saved","recovery_cleanup":"removed",
 * "recovery_cleanup_error":null,"index_state":"current","index_error":null,
 * "recency_error":null,"canonical_error":null}. Auxiliary failures are reported in JSON after
 * the file is saved. A parent-sync failure retains recovery and reports recovery_durable or
 * accepted with canonical_error because canonical durability is uncertain. */
int32_t howler_note_editor_save(HowlerNoteFolder *folder, HowlerNoteEditor *editor, uint64_t expected_revision, char **out_json);
int32_t howler_note_editor_reconcile(HowlerNoteEditor *editor, char **out_json);
int32_t howler_folder_rename_title(HowlerNoteFolder *folder, const char *note_id, const char *title, char **out_json);
int32_t howler_folder_move_note(HowlerNoteFolder *folder, const char *note_id, const char *destination, char **out_json);
int32_t howler_folder_trash_note(HowlerNoteFolder *folder, const char *note_id, char **out_json);
int32_t howler_folder_restore_note(HowlerNoteFolder *folder, const char *trash_path, char **out_json);
int32_t howler_folder_recoveries(HowlerNoteFolder *folder, char **out_json);
/* Recovery JSON is an array of {"note_id":"...","relative_path":"n.md","revision":1,
 * "base_hash":"...","source":"draft"}. */
int32_t howler_folder_discard_recovery(HowlerNoteFolder *folder, const char *note_id);
int32_t howler_folder_search(HowlerNoteFolder *folder, const char *query, size_t limit, char **out_json);
int32_t howler_folder_rebuild(HowlerNoteFolder *folder, char **out_json);
int32_t howler_folder_rescan(HowlerNoteFolder *folder, char **out_json);
int32_t howler_folder_diagnostics(HowlerNoteFolder *folder, char **out_json);
int32_t howler_folder_diagnostic_bundle(HowlerNoteFolder *folder, char **out_json);
void howler_folder_destroy(HowlerNoteFolder *folder);
/* Editors are independently owned but bound to the exact folder context used to create them.
 * Adoption may rewrite notes and migrate device-local state before publishing library metadata. */
void howler_note_editor_destroy(HowlerNoteEditor *editor);
void howler_application_string_free(char *string);

#ifdef __cplusplus
}
#endif
#endif
