//! iOS retains raster storage for the lifetime of a Core Graphics data provider.
use std::ffi::c_void;
use std::sync::{Arc, Weak};

/// Returns an owning token for immutable RGBA bytes, or null for a stale ID.
/// The caller must supply writable output pointers and release the token once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn torto_ios_retain_pixels(
    id: u64,
    data: *mut *const u8,
    size: *mut usize,
) -> *const c_void {
    if data.is_null() || size.is_null() {
        return std::ptr::null();
    }
    let Some(page) = crate::pixels::registry()
        .lock()
        .ok()
        .and_then(|pages| pages.get(&id).and_then(Weak::upgrade))
    else {
        return std::ptr::null();
    };
    let expected = (page.width() as usize)
        .checked_mul(page.height() as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    if expected != Some(page.pixels.len()) || page.pixels.is_empty() {
        return std::ptr::null();
    }
    // SAFETY: The caller supplies writable storage for both outputs. The
    // retained Arc keeps the immutable buffer alive until the provider frees it.
    unsafe {
        data.write(page.pixels.as_ptr());
        size.write(page.pixels.len());
    }
    Arc::into_raw(Arc::clone(&page.pixels)).cast()
}

/// Releases one token returned by `torto_ios_retain_pixels`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn torto_ios_release_pixels(token: *const c_void) {
    if !token.is_null() {
        // SAFETY: The caller transfers back exactly one owning Arc token.
        drop(unsafe { Arc::from_raw(token.cast::<Vec<u8>>()) });
    }
}
