//! C ABI implementation. Callers must pass live, correctly typed handles and valid pointers as
//! declared in the public header, serialize destruction against use, and use this ABI's matching
//! free calls. Unsafe code is confined to pointer conversion and allocator ownership transfer.
#![allow(unsafe_code, clippy::missing_safety_doc)]

use howler_app::{AppError, ApplicationSession, NoteEditor, NoteFolder};
use howler_editor::{EditorCommand, EditorError, Transaction};
use libc::c_char;
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::{Mutex, TryLockError};

pub const ABI_VERSION: u32 = 1;
pub const SESSION_ABI_VERSION: u32 = 2;
pub const OK: i32 = 0;
pub const INVALID_ARGUMENT: i32 = 1;
pub const STALE_REVISION: i32 = 2;
pub const BUSY: i32 = 4;
pub const NOT_FOUND: i32 = 5;
pub const CONFLICT: i32 = 6;
pub const IO: i32 = 7;
pub const WRONG_OWNER: i32 = 8;
pub const DESTINATION_EXISTS: i32 = 9;
pub const DUPLICATE_ID: i32 = 10;
pub const INTERNAL: i32 = 255;

pub struct HowlerNoteFolder {
    context_id: String,
    inner: Mutex<NoteFolder>,
}

pub struct HowlerNoteEditor {
    owner_context_id: String,
    inner: Mutex<NoteEditor>,
}

pub struct HowlerApplicationSession {
    inner: Mutex<ApplicationSession>,
}

#[derive(Serialize)]
struct BoundaryProblem<'a> {
    code: &'a str,
    diagnostic: &'a str,
}

#[no_mangle]
pub extern "C" fn howler_application_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
pub extern "C" fn howler_session_abi_version() -> u32 {
    SESSION_ABI_VERSION
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_create(
    out_session: *mut *mut HowlerApplicationSession,
) -> i32 {
    if !init_handle_out(out_session) {
        return INVALID_ARGUMENT;
    }
    unsafe {
        *out_session = Box::into_raw(Box::new(HowlerApplicationSession {
            inner: Mutex::new(ApplicationSession::default()),
        }));
    }
    OK
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_destroy(session: *mut HowlerApplicationSession) {
    if !session.is_null() {
        drop(unsafe { Box::from_raw(session) });
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_state_json(
    session: *mut HowlerApplicationSession,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        |session| session.inspect(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_connect_json(
    session: *mut HowlerApplicationSession,
    request_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        request_json,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::connect,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_adopt_folder_json(
    session: *mut HowlerApplicationSession,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        |session| session.adopt_folder(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_create_note_json(
    session: *mut HowlerApplicationSession,
    request_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        request_json,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::create_note,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_open_note_json(
    session: *mut HowlerApplicationSession,
    note_id: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_string_input(
        session,
        note_id,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::open_note,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_close_note_json(
    session: *mut HowlerApplicationSession,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        |session| session.close_note(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_apply_text_edit_json(
    session: *mut HowlerApplicationSession,
    edit_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        edit_json,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::apply_text_edit,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_preserve_pending_native_draft_json(
    session: *mut HowlerApplicationSession,
    draft_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        draft_json,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::preserve_pending_native_draft,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_resolve_pending_native_draft_json(
    session: *mut HowlerApplicationSession,
    resolution_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        resolution_json,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::resolve_pending_native_draft,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_execute_command_json(
    session: *mut HowlerApplicationSession,
    expected_revision: u64,
    command_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        command_json,
        out_response_json,
        out_boundary_problem_json,
        |session, command| session.execute_command(expected_revision, command),
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_undo_json(
    session: *mut HowlerApplicationSession,
    expected_revision: u64,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        |session| session.undo(expected_revision),
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_redo_json(
    session: *mut HowlerApplicationSession,
    expected_revision: u64,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        |session| session.redo(expected_revision),
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_save_json(
    session: *mut HowlerApplicationSession,
    target_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        target_json,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::save,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_resolve_conflict_json(
    session: *mut HowlerApplicationSession,
    resolution_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        resolution_json,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::resolve_conflict,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_restore_recovery_json(
    session: *mut HowlerApplicationSession,
    note_id: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_string_input(
        session,
        note_id,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::restore_recovery,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_discard_recovery_json(
    session: *mut HowlerApplicationSession,
    note_id: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_string_input(
        session,
        note_id,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::discard_recovery,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_reconcile_active_json(
    session: *mut HowlerApplicationSession,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        |session| session.reconcile_active(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_search_json(
    session: *mut HowlerApplicationSession,
    query_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        query_json,
        out_response_json,
        out_boundary_problem_json,
        |session, query| session.search(query),
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_rename_note_json(
    session: *mut HowlerApplicationSession,
    request_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        request_json,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::rename_note,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_move_note_json(
    session: *mut HowlerApplicationSession,
    request_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        request_json,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::move_note,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_trash_note_json(
    session: *mut HowlerApplicationSession,
    note_id: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_string_input(
        session,
        note_id,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::trash_note,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_restore_note_json(
    session: *mut HowlerApplicationSession,
    request_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_input(
        session,
        request_json,
        out_response_json,
        out_boundary_problem_json,
        ApplicationSession::restore_note,
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_diagnostics_json(
    session: *mut HowlerApplicationSession,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        |session| session.diagnostics(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_diagnostic_bundle_json(
    session: *mut HowlerApplicationSession,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
) -> i32 {
    session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        |session| session.diagnostic_bundle(),
    )
}

#[no_mangle]
pub unsafe extern "C" fn howler_session_string_free(string: *mut c_char) {
    unsafe { howler_application_string_free(string) }
}

#[no_mangle]
pub extern "C" fn howler_application_error_message(code: i32) -> *const c_char {
    match code {
        OK => c"ok".as_ptr(),
        INVALID_ARGUMENT => c"invalid argument".as_ptr(),
        STALE_REVISION => c"stale revision".as_ptr(),
        BUSY => c"handle is busy".as_ptr(),
        NOT_FOUND => c"note or recovery not found".as_ptr(),
        CONFLICT => c"external change conflict".as_ptr(),
        IO => c"I/O error".as_ptr(),
        WRONG_OWNER => c"note editor belongs to another folder".as_ptr(),
        DESTINATION_EXISTS => c"destination exists".as_ptr(),
        DUPLICATE_ID => c"duplicate note ID requires repair".as_ptr(),
        _ => c"internal error".as_ptr(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_open(
    path: *const c_char,
    state_path: *const c_char,
    adopt: i32,
    out_folder: *mut *mut HowlerNoteFolder,
) -> i32 {
    if !init_handle_out(out_folder) {
        return INVALID_ARGUMENT;
    }
    let (Ok(path), Ok(state)) = (cstr(path), cstr(state_path)) else {
        return INVALID_ARGUMENT;
    };
    match NoteFolder::open(path, state, adopt != 0) {
        Ok(folder) => {
            let context_id = folder.context_id().to_owned();
            unsafe {
                *out_folder = Box::into_raw(Box::new(HowlerNoteFolder {
                    context_id,
                    inner: Mutex::new(folder),
                }));
            }
            OK
        }
        Err(error) => app_error(&error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_create(
    path: *const c_char,
    state_path: *const c_char,
    adopt: i32,
    out_folder: *mut *mut HowlerNoteFolder,
) -> i32 {
    if !init_handle_out(out_folder) {
        return INVALID_ARGUMENT;
    }
    let (Ok(path), Ok(state)) = (cstr(path), cstr(state_path)) else {
        return INVALID_ARGUMENT;
    };
    match NoteFolder::create(path, state, adopt != 0) {
        Ok(folder) => {
            let context_id = folder.context_id().to_owned();
            unsafe {
                *out_folder = Box::into_raw(Box::new(HowlerNoteFolder {
                    context_id,
                    inner: Mutex::new(folder),
                }));
            }
            OK
        }
        Err(error) => app_error(&error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_create_note(
    folder: *mut HowlerNoteFolder,
    source: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    let source = if source.is_null() {
        None
    } else {
        match cstr(source) {
            Ok(source) => Some(source),
            Err(code) => return code,
        }
    };
    with_folder(folder, |folder| {
        output_initialized(out_json, folder.create_note(source))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_open_editor(
    folder: *mut HowlerNoteFolder,
    note_id: *const c_char,
    out_editor: *mut *mut HowlerNoteEditor,
) -> i32 {
    if !init_handle_out(out_editor) {
        return INVALID_ARGUMENT;
    }
    let Ok(note_id) = cstr(note_id) else {
        return INVALID_ARGUMENT;
    };
    let Some(folder_handle) = (unsafe { folder.as_ref() }) else {
        return INVALID_ARGUMENT;
    };
    let folder = match try_lock(&folder_handle.inner) {
        Ok(folder) => folder,
        Err(code) => return code,
    };
    match folder.open_editor(note_id) {
        Ok(editor) => {
            unsafe {
                *out_editor = Box::into_raw(Box::new(HowlerNoteEditor {
                    owner_context_id: folder_handle.context_id.clone(),
                    inner: Mutex::new(editor),
                }));
            }
            OK
        }
        Err(error) => app_error(&error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_restore_recovery(
    folder: *mut HowlerNoteFolder,
    note_id: *const c_char,
    out_editor: *mut *mut HowlerNoteEditor,
) -> i32 {
    if !init_handle_out(out_editor) {
        return INVALID_ARGUMENT;
    }
    let Ok(note_id) = cstr(note_id) else {
        return INVALID_ARGUMENT;
    };
    let Some(folder_handle) = (unsafe { folder.as_ref() }) else {
        return INVALID_ARGUMENT;
    };
    let folder = match try_lock(&folder_handle.inner) {
        Ok(folder) => folder,
        Err(code) => return code,
    };
    match folder.restore_recovery(note_id) {
        Ok(editor) => {
            unsafe {
                *out_editor = Box::into_raw(Box::new(HowlerNoteEditor {
                    owner_context_id: folder_handle.context_id.clone(),
                    inner: Mutex::new(editor),
                }));
            }
            OK
        }
        Err(error) => app_error(&error),
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_note_editor_snapshot(
    editor: *mut HowlerNoteEditor,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    with_editor(editor, |editor| {
        output_value_initialized(out_json, &editor.snapshot())
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_note_editor_apply_json(
    editor: *mut HowlerNoteEditor,
    transaction_json: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    let Ok(json) = cstr(transaction_json) else {
        return INVALID_ARGUMENT;
    };
    let Ok(transaction) = serde_json::from_str::<Transaction>(json) else {
        return INVALID_ARGUMENT;
    };
    with_editor(editor, |editor| {
        output_initialized(out_json, editor.apply(transaction))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_note_editor_command_json(
    editor: *mut HowlerNoteEditor,
    expected_revision: u64,
    command_json: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    let Ok(json) = cstr(command_json) else {
        return INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_str::<EditorCommand>(json) else {
        return INVALID_ARGUMENT;
    };
    with_editor(editor, |editor| {
        output_initialized(out_json, editor.execute_command(expected_revision, command))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_note_editor_undo(
    editor: *mut HowlerNoteEditor,
    expected_revision: u64,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    with_editor(editor, |editor| {
        output_initialized(out_json, editor.undo(expected_revision))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_note_editor_redo(
    editor: *mut HowlerNoteEditor,
    expected_revision: u64,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    with_editor(editor, |editor| {
        output_initialized(out_json, editor.redo(expected_revision))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_note_editor_save(
    folder: *mut HowlerNoteFolder,
    editor: *mut HowlerNoteEditor,
    expected_revision: u64,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    let (Some(folder_handle), Some(editor_handle)) =
        (unsafe { folder.as_ref() }, unsafe { editor.as_ref() })
    else {
        return INVALID_ARGUMENT;
    };
    if folder_handle.context_id != editor_handle.owner_context_id {
        return WRONG_OWNER;
    }
    let folder = match try_lock(&folder_handle.inner) {
        Ok(folder) => folder,
        Err(code) => return code,
    };
    let mut editor = match try_lock(&editor_handle.inner) {
        Ok(editor) => editor,
        Err(code) => return code,
    };
    output_initialized(out_json, folder.save_editor(&mut editor, expected_revision))
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_rename_title(
    folder: *mut HowlerNoteFolder,
    note_id: *const c_char,
    title: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    let (Ok(note_id), Ok(title)) = (cstr(note_id), cstr(title)) else {
        return INVALID_ARGUMENT;
    };
    with_folder(folder, |folder| {
        output_initialized(out_json, folder.rename_title(note_id, title))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_move_note(
    folder: *mut HowlerNoteFolder,
    note_id: *const c_char,
    destination: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    let (Ok(note_id), Ok(destination)) = (cstr(note_id), cstr(destination)) else {
        return INVALID_ARGUMENT;
    };
    with_folder(folder, |folder| {
        output_initialized(out_json, folder.move_note(note_id, destination))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_trash_note(
    folder: *mut HowlerNoteFolder,
    note_id: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    let Ok(note_id) = cstr(note_id) else {
        return INVALID_ARGUMENT;
    };
    with_folder(folder, |folder| {
        output_initialized(out_json, folder.trash(note_id))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_restore_note(
    folder: *mut HowlerNoteFolder,
    trash_path: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    let Ok(trash_path) = cstr(trash_path) else {
        return INVALID_ARGUMENT;
    };
    with_folder(folder, |folder| {
        output_initialized(out_json, folder.restore(trash_path))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_recoveries(
    folder: *mut HowlerNoteFolder,
    out_json: *mut *mut c_char,
) -> i32 {
    folder_json(folder, out_json, NoteFolder::recoveries)
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_discard_recovery(
    folder: *mut HowlerNoteFolder,
    note_id: *const c_char,
) -> i32 {
    let Ok(note_id) = cstr(note_id) else {
        return INVALID_ARGUMENT;
    };
    with_folder(folder, |folder| {
        folder
            .discard_recovery(note_id)
            .map(|()| OK)
            .unwrap_or_else(|error| app_error(&error))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_note_editor_reconcile(
    editor: *mut HowlerNoteEditor,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    with_editor(editor, |editor| {
        output_initialized(out_json, editor.reconcile_external())
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_search(
    folder: *mut HowlerNoteFolder,
    query: *const c_char,
    limit: usize,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    let Ok(query) = cstr(query) else {
        return INVALID_ARGUMENT;
    };
    with_folder(folder, |folder| {
        output_initialized(out_json, folder.search(query, limit))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_rebuild(
    folder: *mut HowlerNoteFolder,
    out_json: *mut *mut c_char,
) -> i32 {
    folder_json(folder, out_json, NoteFolder::rebuild_index)
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_rescan(
    folder: *mut HowlerNoteFolder,
    out_json: *mut *mut c_char,
) -> i32 {
    folder_json(folder, out_json, NoteFolder::rescan)
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_diagnostics(
    folder: *mut HowlerNoteFolder,
    out_json: *mut *mut c_char,
) -> i32 {
    folder_json(folder, out_json, NoteFolder::diagnostics)
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_diagnostic_bundle(
    folder: *mut HowlerNoteFolder,
    out_json: *mut *mut c_char,
) -> i32 {
    folder_json(folder, out_json, NoteFolder::diagnostic_bundle)
}

unsafe fn session_input<I, O>(
    session: *mut HowlerApplicationSession,
    input_json: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
    operation: impl FnOnce(&mut ApplicationSession, I) -> O,
) -> i32
where
    I: for<'de> Deserialize<'de>,
    O: Serialize,
{
    if !init_session_outputs(out_response_json, out_boundary_problem_json) {
        return INVALID_ARGUMENT;
    }
    let input = match cstr(input_json) {
        Ok(json) => match serde_json::from_str(json) {
            Ok(input) => input,
            Err(error) => {
                return boundary_failure(
                    out_boundary_problem_json,
                    INVALID_ARGUMENT,
                    "malformed_json",
                    &error.to_string(),
                )
            }
        },
        Err(_) => {
            return boundary_failure(
                out_boundary_problem_json,
                INVALID_ARGUMENT,
                "invalid_utf8",
                "input must be non-null NUL-terminated UTF-8",
            )
        }
    };
    with_session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        |session| operation(session, input),
    )
}

unsafe fn session_string_input<O>(
    session: *mut HowlerApplicationSession,
    input: *const c_char,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
    operation: impl FnOnce(&mut ApplicationSession, &str) -> O,
) -> i32
where
    O: Serialize,
{
    if !init_session_outputs(out_response_json, out_boundary_problem_json) {
        return INVALID_ARGUMENT;
    }
    let input = match cstr(input) {
        Ok(input) => input,
        Err(_) => {
            return boundary_failure(
                out_boundary_problem_json,
                INVALID_ARGUMENT,
                "invalid_utf8",
                "input must be non-null NUL-terminated UTF-8",
            )
        }
    };
    with_session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        |session| operation(session, input),
    )
}

unsafe fn session_output<O>(
    session: *mut HowlerApplicationSession,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
    operation: impl FnOnce(&mut ApplicationSession) -> O,
) -> i32
where
    O: Serialize,
{
    if !init_session_outputs(out_response_json, out_boundary_problem_json) {
        return INVALID_ARGUMENT;
    }
    with_session_output(
        session,
        out_response_json,
        out_boundary_problem_json,
        operation,
    )
}

fn with_session_output<O: Serialize>(
    session: *mut HowlerApplicationSession,
    out_response_json: *mut *mut c_char,
    out_boundary_problem_json: *mut *mut c_char,
    operation: impl FnOnce(&mut ApplicationSession) -> O,
) -> i32 {
    let Some(session) = (unsafe { session.as_ref() }) else {
        return boundary_failure(
            out_boundary_problem_json,
            INVALID_ARGUMENT,
            "invalid_handle",
            "session handle is null",
        );
    };
    let mut session = match try_lock(&session.inner) {
        Ok(session) => session,
        Err(BUSY) => {
            return boundary_failure(
                out_boundary_problem_json,
                BUSY,
                "busy",
                "session is in use by another call",
            )
        }
        Err(_) => {
            return boundary_failure(
                out_boundary_problem_json,
                INTERNAL,
                "poisoned_handle",
                "session lock is unavailable",
            )
        }
    };
    output_value_initialized(out_response_json, &operation(&mut session))
}

fn init_session_outputs(response: *mut *mut c_char, boundary: *mut *mut c_char) -> bool {
    let response_valid = init_string_out(response);
    let boundary_valid = init_string_out(boundary);
    response_valid && boundary_valid
}

fn boundary_failure(out: *mut *mut c_char, status: i32, code: &str, diagnostic: &str) -> i32 {
    if !out.is_null() {
        let _ = output_value_initialized(out, &BoundaryProblem { code, diagnostic });
    }
    status
}

unsafe fn folder_json<T: Serialize>(
    folder: *mut HowlerNoteFolder,
    out_json: *mut *mut c_char,
    operation: impl FnOnce(&NoteFolder) -> Result<T, AppError>,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    with_folder(folder, |folder| {
        output_initialized(out_json, operation(folder))
    })
}

fn with_folder(folder: *mut HowlerNoteFolder, operation: impl FnOnce(&NoteFolder) -> i32) -> i32 {
    let Some(folder) = (unsafe { folder.as_ref() }) else {
        return INVALID_ARGUMENT;
    };
    match try_lock(&folder.inner) {
        Ok(folder) => operation(&folder),
        Err(code) => code,
    }
}

fn with_editor(
    editor: *mut HowlerNoteEditor,
    operation: impl FnOnce(&mut NoteEditor) -> i32,
) -> i32 {
    let Some(editor) = (unsafe { editor.as_ref() }) else {
        return INVALID_ARGUMENT;
    };
    match try_lock(&editor.inner) {
        Ok(mut editor) => operation(&mut editor),
        Err(code) => code,
    }
}

fn try_lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, i32> {
    match mutex.try_lock() {
        Ok(value) => Ok(value),
        Err(TryLockError::WouldBlock) => Err(BUSY),
        Err(TryLockError::Poisoned(_)) => Err(INTERNAL),
    }
}

fn output_initialized<T: Serialize>(out: *mut *mut c_char, result: Result<T, AppError>) -> i32 {
    match result {
        Ok(value) => output_value_initialized(out, &value),
        Err(error) => app_error(&error),
    }
}

fn output_value_initialized<T: Serialize>(out: *mut *mut c_char, value: &T) -> i32 {
    match serde_json::to_string(value)
        .ok()
        .and_then(|json| CString::new(json).ok())
    {
        Some(json) => {
            unsafe {
                *out = json.into_raw();
            }
            OK
        }
        None => INTERNAL,
    }
}

fn init_string_out(out: *mut *mut c_char) -> bool {
    if out.is_null() {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
    }
    true
}

fn init_handle_out<T>(out: *mut *mut T) -> bool {
    if out.is_null() {
        return false;
    }
    unsafe {
        *out = ptr::null_mut();
    }
    true
}

fn cstr<'a>(value: *const c_char) -> Result<&'a str, i32> {
    if value.is_null() {
        return Err(INVALID_ARGUMENT);
    }
    unsafe { CStr::from_ptr(value) }
        .to_str()
        .map_err(|_| INVALID_ARGUMENT)
}

fn app_error(error: &AppError) -> i32 {
    match error {
        AppError::NoteNotFound(_) | AppError::RecoveryNotFound(_) => NOT_FOUND,
        AppError::RecoveryPending(_) | AppError::IdentityChanged | AppError::StaleHandle => {
            CONFLICT
        }
        AppError::NoteAlreadyOpen(_) | AppError::PendingNativeDraft(_) => CONFLICT,
        AppError::ExternalConflict { .. } => CONFLICT,
        AppError::Io(_) | AppError::InvalidUtf8(_) => IO,
        AppError::WrongOwner => WRONG_OWNER,
        AppError::DestinationExists(_) => DESTINATION_EXISTS,
        AppError::DuplicateIdentity(_) => DUPLICATE_ID,
        AppError::Editor(EditorError::StaleRevision { .. }) => STALE_REVISION,
        AppError::Editor(_)
        | AppError::PathEscape
        | AppError::MalformedMetadata(_)
        | AppError::InvalidTitle => INVALID_ARGUMENT,
        AppError::Database(_) => INTERNAL,
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_folder_destroy(folder: *mut HowlerNoteFolder) {
    if !folder.is_null() {
        drop(unsafe { Box::from_raw(folder) });
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_note_editor_destroy(editor: *mut HowlerNoteEditor) {
    if !editor.is_null() {
        drop(unsafe { Box::from_raw(editor) });
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_application_string_free(string: *mut c_char) {
    if !string.is_null() {
        drop(unsafe { CString::from_raw(string) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    unsafe fn open_folder(notes: &Path, state: &Path) -> *mut HowlerNoteFolder {
        let path = CString::new(notes.to_str().unwrap()).unwrap();
        let state_path = CString::new(state.to_str().unwrap()).unwrap();
        let mut folder = ptr::null_mut();
        assert_eq!(
            unsafe { howler_folder_open(path.as_ptr(), state_path.as_ptr(), 0, &mut folder) },
            OK
        );
        folder
    }

    unsafe fn create_and_open(folder: *mut HowlerNoteFolder) -> *mut HowlerNoteEditor {
        let source = CString::new("# FFI").unwrap();
        let mut json = ptr::null_mut();
        assert_eq!(
            unsafe { howler_folder_create_note(folder, source.as_ptr(), &mut json) },
            OK
        );
        let value: serde_json::Value =
            serde_json::from_str(unsafe { CStr::from_ptr(json) }.to_str().unwrap()).unwrap();
        let id = value["id"]["value"].as_str().unwrap();
        let id = CString::new(id).unwrap();
        unsafe { howler_application_string_free(json) };
        let mut editor = ptr::null_mut();
        assert_eq!(
            unsafe { howler_folder_open_editor(folder, id.as_ptr(), &mut editor) },
            OK
        );
        editor
    }

    #[test]
    fn ownership_undo_redo_and_durability_results() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        unsafe {
            let folder = open_folder(notes.path(), state.path());
            let editor = create_and_open(folder);
            let transaction = CString::new(r#"{"expected_revision":0,"replacements":[{"range":{"start":5,"end":5},"text":"!"}],"selections":[{"anchor":6,"head":6,"affinity":"Downstream","revision":1}],"history":"Typing"}"#).unwrap();
            let mut json = ptr::null_mut();
            assert_eq!(
                howler_note_editor_apply_json(editor, transaction.as_ptr(), &mut json),
                OK
            );
            assert!(CStr::from_ptr(json)
                .to_str()
                .unwrap()
                .contains("recovery_durable"));
            howler_application_string_free(json);
            assert_eq!(howler_note_editor_undo(editor, 1, &mut json), OK);
            assert!(CStr::from_ptr(json)
                .to_str()
                .unwrap()
                .contains("\"revision\":2"));
            howler_application_string_free(json);
            assert_eq!(howler_note_editor_redo(editor, 2, &mut json), OK);
            howler_application_string_free(json);
            json = 1usize as *mut c_char;
            assert_eq!(
                howler_note_editor_save(folder, editor, 2, &mut json),
                STALE_REVISION
            );
            assert!(json.is_null());
            assert_eq!(howler_note_editor_save(folder, editor, 3, &mut json), OK);
            assert!(CStr::from_ptr(json)
                .to_str()
                .unwrap()
                .contains("file_saved"));
            howler_application_string_free(json);
            howler_note_editor_destroy(editor);
            howler_folder_destroy(folder);
        }
    }

    #[test]
    fn wrong_owner_busy_and_outputs_are_initialized() {
        let notes_a = tempfile::tempdir().unwrap();
        let notes_b = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        unsafe {
            let folder_a = open_folder(notes_a.path(), state.path());
            let folder_b = open_folder(notes_b.path(), state.path());
            let editor = create_and_open(folder_a);
            let mut json = 1usize as *mut c_char;
            assert_eq!(
                howler_note_editor_save(folder_b, editor, 0, &mut json),
                WRONG_OWNER
            );
            assert!(json.is_null());

            let editor_ref = &*editor;
            let _guard = editor_ref.inner.lock().unwrap();
            json = 1usize as *mut c_char;
            assert_eq!(howler_note_editor_snapshot(editor, &mut json), BUSY);
            assert!(json.is_null());
            drop(_guard);

            let invalid = CString::new("not json").unwrap();
            json = 1usize as *mut c_char;
            assert_eq!(
                howler_note_editor_apply_json(editor, invalid.as_ptr(), &mut json),
                INVALID_ARGUMENT
            );
            assert!(json.is_null());
            howler_note_editor_destroy(editor);
            howler_folder_destroy(folder_a);
            howler_folder_destroy(folder_b);
        }
    }

    #[test]
    fn lifecycle_and_recovery_endpoints_are_callable() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        fs::write(notes.path().join("note.md"), "body").unwrap();
        unsafe {
            let folder = open_folder(notes.path(), state.path());
            let mut json = ptr::null_mut();
            assert_eq!(howler_folder_rescan(folder, &mut json), OK);
            howler_application_string_free(json);
            assert_eq!(howler_folder_recoveries(folder, &mut json), OK);
            assert_eq!(CStr::from_ptr(json).to_str().unwrap(), "[]");
            howler_application_string_free(json);
            assert_eq!(howler_folder_diagnostic_bundle(folder, &mut json), OK);
            assert!(!CStr::from_ptr(json)
                .to_str()
                .unwrap()
                .contains(notes.path().to_str().unwrap()));
            howler_application_string_free(json);
            howler_folder_destroy(folder);
        }
    }

    unsafe fn create_session() -> *mut HowlerApplicationSession {
        let mut session = ptr::null_mut();
        assert_eq!(unsafe { howler_session_create(&mut session) }, OK);
        session
    }

    unsafe fn connect_session(session: *mut HowlerApplicationSession, notes: &Path, state: &Path) {
        let request = CString::new(
            serde_json::json!({
                "folder_path": notes,
                "application_state_path": state,
                "adopt": false,
                "create_missing": false
            })
            .to_string(),
        )
        .unwrap();
        let mut response = ptr::null_mut();
        let mut boundary = ptr::null_mut();
        assert_eq!(
            unsafe {
                howler_session_connect_json(session, request.as_ptr(), &mut response, &mut boundary)
            },
            OK
        );
        assert!(boundary.is_null());
        assert!(unsafe { CStr::from_ptr(response) }
            .to_str()
            .unwrap()
            .contains("\"status\":\"applied\""));
        unsafe { howler_session_string_free(response) };
    }

    #[test]
    fn session_abi_returns_applied_and_structured_rejected_responses() {
        let notes = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        unsafe {
            let session = create_session();
            connect_session(session, notes.path(), state.path());
            let mut response = ptr::null_mut();
            let mut boundary = ptr::null_mut();
            assert_eq!(
                howler_session_state_json(session, &mut response, &mut boundary),
                OK
            );
            let state_json = CStr::from_ptr(response).to_str().unwrap();
            assert!(state_json.contains("\"status\":\"applied\""));
            assert!(state_json.contains("\"state\":{"));
            howler_session_string_free(response);
            let edit = CString::new(
                r#"{"expected_revision":8,"replacements":[],"selections":[],"history":"Isolated","composition":null}"#,
            )
            .unwrap();
            response = ptr::null_mut();
            boundary = ptr::null_mut();
            assert_eq!(
                howler_session_apply_text_edit_json(
                    session,
                    edit.as_ptr(),
                    &mut response,
                    &mut boundary,
                ),
                OK
            );
            assert!(boundary.is_null());
            let json = CStr::from_ptr(response).to_str().unwrap();
            assert!(json.contains("\"status\":\"rejected\""));
            assert!(json.contains("\"code\":\"stale_revision\""));
            assert!(json.contains("\"current_revision\":0"));
            assert!(json.contains("\"state\":{"));
            howler_session_string_free(response);
            howler_session_destroy(session);
        }
    }

    #[test]
    fn session_abi_boundary_failures_initialize_outputs_and_report_busy() {
        unsafe {
            let session = create_session();
            let invalid = CString::new("not json").unwrap();
            let mut response = 1usize as *mut c_char;
            let mut boundary = 1usize as *mut c_char;
            assert_eq!(
                howler_session_connect_json(
                    session,
                    invalid.as_ptr(),
                    &mut response,
                    &mut boundary,
                ),
                INVALID_ARGUMENT
            );
            assert!(response.is_null());
            assert!(CStr::from_ptr(boundary)
                .to_str()
                .unwrap()
                .contains("malformed_json"));
            howler_session_string_free(boundary);

            let _guard = (*session).inner.lock().unwrap();
            boundary = ptr::null_mut();
            assert_eq!(
                howler_session_state_json(session, &mut response, &mut boundary),
                BUSY
            );
            assert!(response.is_null());
            assert!(CStr::from_ptr(boundary).to_str().unwrap().contains("busy"));
            howler_session_string_free(boundary);
            drop(_guard);
            howler_session_destroy(session);
        }
    }

    #[test]
    fn v1_v2_versions_are_stable() {
        assert_eq!(howler_application_abi_version(), 1);
        assert_eq!(howler_session_abi_version(), 2);
    }

    #[test]
    fn checked_in_v2_json_schema_is_valid_and_covers_exported_requests() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../../docs/schema/application-session-v2.schema.json"
        ))
        .unwrap();
        assert_eq!(
            schema["$id"],
            "https://howler.local/schema/application-session-v2.schema.json"
        );
        assert_eq!(schema["$ref"], "#/$defs/applicationResponse");
        let definitions = schema["$defs"].as_object().unwrap();
        for required in [
            "applicationResponse",
            "appliedValue",
            "applicationState",
            "problem",
            "hostEffect",
            "connectRequest",
            "createNoteRequest",
            "hostTextEdit",
            "editorCommand",
            "pendingNativeDraft",
            "pendingDraftResolution",
            "saveTarget",
            "conflictResolution",
            "searchQuery",
            "renameNoteRequest",
            "moveNoteRequest",
            "restoreNoteRequest",
            "noteSummary",
            "noteResult",
            "editResult",
            "saveResult",
            "reconcileResult",
            "searchResult",
            "diagnostic",
            "diagnosticBundle",
        ] {
            assert!(
                definitions.contains_key(required),
                "missing schema {required}"
            );
        }
        assert_eq!(
            definitions["applicationResponse"]["properties"]["outcome"]["oneOf"][0]["properties"]
                ["value"]["$ref"],
            "#/$defs/appliedValue"
        );
        for resolution in ["pendingDraftResolution", "conflictResolution"] {
            let required = definitions[resolution]["oneOf"][0]["required"]
                .as_array()
                .unwrap();
            assert!(!required.iter().any(|field| field == "title"));
        }
        fn assert_extensible(value: &serde_json::Value) {
            if let Some(object) = value.as_object() {
                assert_ne!(
                    object.get("additionalProperties"),
                    Some(&serde_json::Value::Bool(false))
                );
                for child in object.values() {
                    assert_extensible(child);
                }
            } else if let Some(array) = value.as_array() {
                for child in array {
                    assert_extensible(child);
                }
            }
        }
        assert_extensible(&schema);
    }
}
