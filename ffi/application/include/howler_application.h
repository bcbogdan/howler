#ifndef HOWLER_APPLICATION_H
#define HOWLER_APPLICATION_H
#include <stdint.h>
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif

#define HOWLER_APPLICATION_ABI_VERSION 1
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
