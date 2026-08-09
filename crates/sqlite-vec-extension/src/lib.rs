//! Safe registration boundary for the statically linked sqlite-vec extension.

use std::sync::Once;

type SqliteExtensionEntry = unsafe extern "C" fn(
    *mut rusqlite::ffi::sqlite3,
    *mut *mut std::os::raw::c_char,
    *const rusqlite::ffi::sqlite3_api_routines,
) -> std::os::raw::c_int;

static REGISTER: Once = Once::new();

/// Registers sqlite-vec for subsequently opened rusqlite connections.
pub fn register() {
    REGISTER.call_once(|| {
        // SAFETY: sqlite-vec exposes `sqlite3_vec_init` with SQLite's extension
        // entry-point ABI. `sqlite3_auto_extension` stores that process-lifetime
        // function pointer and invokes it for subsequently opened connections.
        unsafe {
            rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
                *const (),
                SqliteExtensionEntry,
            >(
                sqlite_vec::sqlite3_vec_init as *const (),
            )));
        }
    });
}
