#![deny(unsafe_code)]

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use std::io;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, WM_ERASEBKGND, WM_NCDESTROY};

    const SUBCLASS_ID: usize = 0x544f_4247;

    struct BackgroundState {
        color: Arc<AtomicU32>,
    }

    /// Keeps the native resize background color synchronized with the GPU clear color.
    pub struct WindowBackground {
        color: Arc<AtomicU32>,
    }

    impl WindowBackground {
        /// Installs a Win32 subclass that erases only the invalidated client region.
        pub fn install(hwnd: isize, rgb: [u8; 3]) -> io::Result<Self> {
            if hwnd == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "window handle must not be null",
                ));
            }
            let color = Arc::new(AtomicU32::new(colorref(rgb)));
            let state = Box::new(BackgroundState {
                color: Arc::clone(&color),
            });
            let state_ptr = Box::into_raw(state);
            let installed = unsafe {
                SetWindowSubclass(
                    hwnd as HWND,
                    Some(background_subclass_proc),
                    SUBCLASS_ID,
                    state_ptr as usize,
                )
            };
            if installed == 0 {
                unsafe {
                    drop(Box::from_raw(state_ptr));
                }
                return Err(io::Error::last_os_error());
            }
            Ok(Self { color })
        }

        pub fn set_color(&self, rgb: [u8; 3]) {
            self.color.store(colorref(rgb), Ordering::Relaxed);
        }
    }

    const fn colorref([red, green, blue]: [u8; 3]) -> u32 {
        (red as u32) | ((green as u32) << 8) | ((blue as u32) << 16)
    }

    unsafe extern "system" fn background_subclass_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _subclass_id: usize,
        state_ptr: usize,
    ) -> LRESULT {
        if message == WM_NCDESTROY {
            unsafe {
                RemoveWindowSubclass(hwnd, Some(background_subclass_proc), SUBCLASS_ID);
                drop(Box::from_raw(state_ptr as *mut BackgroundState));
                return DefSubclassProc(hwnd, message, wparam, lparam);
            }
        }
        if message == WM_ERASEBKGND {
            let state = unsafe { &*(state_ptr as *const BackgroundState) };
            let mut client_rect = RECT::default();
            let brush = unsafe { CreateSolidBrush(state.color.load(Ordering::Relaxed)) };
            if !brush.is_null() {
                let client_rect_available = unsafe { GetClientRect(hwnd, &mut client_rect) } != 0;
                let filled = client_rect_available
                    && unsafe { FillRect(wparam as _, &client_rect, brush) } != 0;
                unsafe {
                    DeleteObject(brush);
                }
                if filled {
                    // FillRect observes the update-region clip on the WM_ERASEBKGND
                    // HDC, so existing GPU content is retained while only newly
                    // exposed resize pixels receive the themed fallback color.
                    return 1;
                }
            }
        }
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }

    #[cfg(test)]
    mod tests {
        use super::colorref;

        #[test]
        fn converts_rgb_to_win32_colorref() {
            assert_eq!(colorref([0x12, 0x34, 0x56]), 0x0056_3412);
        }
    }
}

#[cfg(target_os = "windows")]
pub use windows::WindowBackground;
