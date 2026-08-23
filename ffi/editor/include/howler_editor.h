#ifndef HOWLER_EDITOR_H
#define HOWLER_EDITOR_H
#include <stdint.h>
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
#define HOWLER_EDITOR_ABI_VERSION 1
#define HOWLER_EDITOR_OK 0
#define HOWLER_EDITOR_INVALID_ARGUMENT 1
#define HOWLER_EDITOR_STALE_REVISION 2
#define HOWLER_EDITOR_INVALID_RANGE 3
#define HOWLER_EDITOR_BUSY 4
#define HOWLER_EDITOR_INTERNAL 255
typedef struct HowlerEditor HowlerEditor;

/* All calls are synchronous. A handle may be used by one thread at a time; concurrent use returns
 * BUSY. The caller must synchronize destruction with every use. error_message returns a borrowed
 * static string that must not be freed. Every non-null output pointer is initialized to NULL before
 * input validation. On success, JSON outputs are owned by the caller and must be released with
 * howler_editor_string_free. Input strings must be non-null, NUL-terminated UTF-8. */
uint32_t howler_editor_abi_version(void);
const char *howler_editor_error_message(int32_t code);
int32_t howler_editor_create(const char *source_utf8, HowlerEditor **out_editor);
/* Snapshot JSON: {"revision":0,"source":"text","selections":[{"anchor":0,"head":0,
 * "affinity":"Downstream","revision":0}],"can_undo":false,"can_redo":false}. */
int32_t howler_editor_snapshot(HowlerEditor *editor, char **out_json);
/* Transaction JSON: {"expected_revision":0,"replacements":[{"range":{"start":0,"end":0},
 * "text":"x"}],"selections":[],"history":"Typing"}. Edit-result JSON contains revision,
 * changed_ranges, selections, and decorations. Byte offsets are UTF-8 boundaries. */
int32_t howler_editor_apply_json(HowlerEditor *editor, const char *transaction_json, char **out_json);
int32_t howler_editor_command_json(HowlerEditor *editor, uint64_t expected_revision, const char *command_json, char **out_json);
/* Undo/redo JSON is an edit result or null when no history exists. */
int32_t howler_editor_undo(HowlerEditor *editor, uint64_t expected_revision, char **out_json);
int32_t howler_editor_redo(HowlerEditor *editor, uint64_t expected_revision, char **out_json);
/* Decoration JSON is an array such as [{"range":{"start":0,"end":3},"kind":{"Heading":1}}]. */
int32_t howler_editor_decorations(HowlerEditor *editor, size_t start, size_t end, uint64_t expected_revision, char **out_json);
/* NULL is accepted by destroy/free. No other pointer value may be freed more than once. */
void howler_editor_destroy(HowlerEditor *editor);
void howler_editor_string_free(char *string);
#ifdef __cplusplus
}
#endif
#endif
