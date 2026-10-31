//! Thin JNI hooks for retained pixels and Android system TLS initialization.
//! IDs are checked registry keys, never pointers supplied by the caller.
use jni::{
    JNIEnv,
    objects::{JClass, JObject},
    sys::{jboolean, jlong},
};
use ndk_sys::{
    AndroidBitmap_getInfo, AndroidBitmap_lockPixels, AndroidBitmap_unlockPixels, AndroidBitmapInfo,
};

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_tortotech_torto_NativePixels_copyPage(
    env: JNIEnv,
    _class: JClass,
    id: jlong,
    bitmap: JObject,
) -> jboolean {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let page = crate::pixels::registry()
            .lock()
            .ok()
            .and_then(|map| map.get(&(id as u64)).and_then(std::sync::Weak::upgrade));
        let Some(page) = page else { return false };
        let raw_env = env.get_native_interface().cast();
        let raw_bitmap = bitmap.as_raw().cast();
        let mut info: AndroidBitmapInfo = unsafe { std::mem::zeroed() };
        // SAFETY: JNI guarantees that env and bitmap references remain valid for
        // this call. All dimensions and format are checked before locking pixels.
        if unsafe { AndroidBitmap_getInfo(raw_env, raw_bitmap, &mut info) } != 0
            || info.format != 1
            || info.width != page.width()
            || info.height != page.height()
            || info.stride < info.width * 4
        {
            return false;
        }
        let mut data = std::ptr::null_mut();
        if unsafe { AndroidBitmap_lockPixels(raw_env, raw_bitmap, &mut data) } != 0 {
            return false;
        }
        if data.is_null() {
            unsafe {
                AndroidBitmap_unlockPixels(raw_env, raw_bitmap);
            }
            return false;
        }
        // Lock/unlock are paired on every path; copy cannot panic after dimensions
        // have been validated. Kotlin publishes the bitmap only after completion.
        for row in 0..info.height as usize {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    page.pixels.as_ptr().add(row * info.width as usize * 4),
                    data.cast::<u8>().add(row * info.stride as usize),
                    info.width as usize * 4,
                );
            }
        }
        unsafe {
            AndroidBitmap_unlockPixels(raw_env, raw_bitmap);
        }
        true
    }))
    .unwrap_or(false) as jboolean
}

/// Bind Android's system certificate verifier before any worker can use HTTPS.
/// The Application context and class loader are retained as JVM global references.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_tortotech_torto_NativePlatform_initializeTls<'caller>(
    mut env: jni_platform::EnvUnowned<'caller>,
    _class: jni_platform::objects::JClass<'caller>,
    context: jni_platform::objects::JObject<'caller>,
) {
    env.with_env(|env| rustls_platform_verifier::android::init_with_env(env, context))
        .resolve::<jni_platform::errors::ThrowRuntimeExAndDefault>();
}
