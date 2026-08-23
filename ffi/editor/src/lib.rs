//! C ABI implementation. Callers must pass live handles and valid pointers as declared in the
//! public header, mutate each handle serially, and free values with this ABI's matching function.
//! Unsafe code is confined to pointer validation, conversion, and allocator ownership transfer.
#![allow(unsafe_code, clippy::missing_safety_doc)]

use howler_editor::{EditorCommand, EditorError, EditorSession, TextRange, Transaction};
use libc::c_char;
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::{Mutex, TryLockError};

pub const ABI_VERSION: u32 = 1;
pub const OK: i32 = 0;
pub const INVALID_ARGUMENT: i32 = 1;
pub const STALE_REVISION: i32 = 2;
pub const INVALID_RANGE: i32 = 3;
pub const BUSY: i32 = 4;
pub const INTERNAL: i32 = 255;

pub struct HowlerEditor {
    session: Mutex<EditorSession>,
}

#[no_mangle]
pub extern "C" fn howler_editor_abi_version() -> u32 {
    ABI_VERSION
}

#[no_mangle]
pub extern "C" fn howler_editor_error_message(code: i32) -> *const c_char {
    match code {
        OK => c"ok".as_ptr(),
        INVALID_ARGUMENT => c"invalid argument".as_ptr(),
        STALE_REVISION => c"stale revision".as_ptr(),
        INVALID_RANGE => c"invalid range or selection".as_ptr(),
        BUSY => c"editor is busy".as_ptr(),
        _ => c"internal error".as_ptr(),
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_editor_create(
    source_utf8: *const c_char,
    out_editor: *mut *mut HowlerEditor,
) -> i32 {
    if out_editor.is_null() {
        return INVALID_ARGUMENT;
    }
    unsafe { *out_editor = ptr::null_mut() };
    if source_utf8.is_null() {
        return INVALID_ARGUMENT;
    }
    let Ok(source) = (unsafe { CStr::from_ptr(source_utf8) }).to_str() else {
        return INVALID_ARGUMENT;
    };
    unsafe {
        *out_editor = Box::into_raw(Box::new(HowlerEditor {
            session: Mutex::new(EditorSession::new(source)),
        }));
    }
    OK
}

#[no_mangle]
pub unsafe extern "C" fn howler_editor_snapshot(
    editor: *mut HowlerEditor,
    out_json: *mut *mut c_char,
) -> i32 {
    with_editor(editor, out_json, |session| {
        serde_json::to_string(&session.snapshot()).map_err(|_| INTERNAL)
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_editor_apply_json(
    editor: *mut HowlerEditor,
    transaction_json: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    if transaction_json.is_null() {
        return INVALID_ARGUMENT;
    }
    let Ok(json) = (unsafe { CStr::from_ptr(transaction_json) }).to_str() else {
        return INVALID_ARGUMENT;
    };
    let Ok(transaction) = serde_json::from_str::<Transaction>(json) else {
        return INVALID_ARGUMENT;
    };
    with_editor(editor, out_json, |session| {
        session
            .apply(transaction)
            .map_err(error_code)
            .and_then(|result| serde_json::to_string(&result).map_err(|_| INTERNAL))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_editor_command_json(
    editor: *mut HowlerEditor,
    expected_revision: u64,
    command_json: *const c_char,
    out_json: *mut *mut c_char,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    if command_json.is_null() {
        return INVALID_ARGUMENT;
    }
    let Ok(json) = (unsafe { CStr::from_ptr(command_json) }).to_str() else {
        return INVALID_ARGUMENT;
    };
    let Ok(command) = serde_json::from_str::<EditorCommand>(json) else {
        return INVALID_ARGUMENT;
    };
    with_editor(editor, out_json, |session| {
        session
            .execute_command(expected_revision, command)
            .map_err(error_code)
            .and_then(|result| serde_json::to_string(&result).map_err(|_| INTERNAL))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_editor_undo(
    editor: *mut HowlerEditor,
    expected_revision: u64,
    out_json: *mut *mut c_char,
) -> i32 {
    with_editor(editor, out_json, |session| {
        session
            .undo(expected_revision)
            .map_err(error_code)
            .and_then(|result| serde_json::to_string(&result).map_err(|_| INTERNAL))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_editor_redo(
    editor: *mut HowlerEditor,
    expected_revision: u64,
    out_json: *mut *mut c_char,
) -> i32 {
    with_editor(editor, out_json, |session| {
        session
            .redo(expected_revision)
            .map_err(error_code)
            .and_then(|result| serde_json::to_string(&result).map_err(|_| INTERNAL))
    })
}

#[no_mangle]
pub unsafe extern "C" fn howler_editor_decorations(
    editor: *mut HowlerEditor,
    start: usize,
    end: usize,
    expected_revision: u64,
    out_json: *mut *mut c_char,
) -> i32 {
    with_editor(editor, out_json, |session| {
        session
            .decorations(TextRange::new(start, end), expected_revision)
            .map_err(error_code)
            .and_then(|result| serde_json::to_string(&result).map_err(|_| INTERNAL))
    })
}

unsafe fn with_editor(
    editor: *mut HowlerEditor,
    out_json: *mut *mut c_char,
    operation: impl FnOnce(&mut EditorSession) -> Result<String, i32>,
) -> i32 {
    if !init_string_out(out_json) {
        return INVALID_ARGUMENT;
    }
    if editor.is_null() {
        return INVALID_ARGUMENT;
    }
    let editor = unsafe { &*editor };
    let mut session = match editor.session.try_lock() {
        Ok(session) => session,
        Err(TryLockError::WouldBlock) => return BUSY,
        Err(TryLockError::Poisoned(_)) => return INTERNAL,
    };
    match operation(&mut session) {
        Ok(json) => match CString::new(json) {
            Ok(json) => {
                unsafe {
                    *out_json = json.into_raw();
                }
                OK
            }
            Err(_) => INTERNAL,
        },
        Err(code) => code,
    }
}

fn init_string_out(out_json: *mut *mut c_char) -> bool {
    if out_json.is_null() {
        return false;
    }
    unsafe { *out_json = ptr::null_mut() };
    true
}

fn error_code(error: EditorError) -> i32 {
    match error {
        EditorError::StaleRevision { .. } => STALE_REVISION,
        EditorError::InvalidRange { .. }
        | EditorError::OverlappingEdits
        | EditorError::InvalidSelection
        | EditorError::InvalidCommand => INVALID_RANGE,
    }
}

#[no_mangle]
pub unsafe extern "C" fn howler_editor_destroy(editor: *mut HowlerEditor) {
    if !editor.is_null() {
        drop(unsafe { Box::from_raw(editor) });
    }
}
#[no_mangle]
pub unsafe extern "C" fn howler_editor_string_free(string: *mut c_char) {
    if !string.is_null() {
        drop(unsafe { CString::from_raw(string) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ffi_owns_and_frees_handles_and_strings() {
        unsafe {
            let source = CString::new("hello").unwrap();
            let mut editor = ptr::null_mut();
            assert_eq!(howler_editor_create(source.as_ptr(), &mut editor), OK);
            let mut snapshot = ptr::null_mut();
            assert_eq!(howler_editor_snapshot(editor, &mut snapshot), OK);
            assert!(CStr::from_ptr(snapshot).to_str().unwrap().contains("hello"));
            howler_editor_string_free(snapshot);
            howler_editor_destroy(editor);
        }
    }

    #[test]
    fn outputs_history_and_decorations_are_revision_checked() {
        unsafe {
            let mut editor = 1usize as *mut HowlerEditor;
            assert_eq!(
                howler_editor_create(ptr::null(), &mut editor),
                INVALID_ARGUMENT
            );
            assert!(editor.is_null());

            let source = CString::new("# title").unwrap();
            assert_eq!(howler_editor_create(source.as_ptr(), &mut editor), OK);
            let mut json = 1usize as *mut c_char;
            assert_eq!(howler_editor_undo(editor, 1, &mut json), STALE_REVISION);
            assert!(json.is_null());
            assert_eq!(howler_editor_decorations(editor, 0, 7, 0, &mut json), OK);
            assert!(CStr::from_ptr(json).to_str().unwrap().contains("Heading"));
            howler_editor_string_free(json);
            howler_editor_destroy(editor);
        }
    }
}
