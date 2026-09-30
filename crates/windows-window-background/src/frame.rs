#![allow(unsafe_code)]
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
// Win32 behavior for a client-drawn caption, retaining standard window styles.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::InvalidateRect;
use windows_sys::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow, ScreenToClient,
};
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const SUBCLASS_ID: usize = 0x544f_4652;

#[derive(Clone, Default)]
pub struct FrameLayout {
    pub fullscreen: bool,
    pub header: Option<[i32; 4]>,
    pub excluded: Vec<[i32; 4]>,
    pub buttons: [Option<[i32; 4]>; 3],
    pub drag_enabled: bool,
}

#[derive(Default)]
struct State {
    layout: Mutex<FrameLayout>,
    hovered: AtomicU32,
    header_hovered: AtomicBool,
    pressed: AtomicU32,
}

pub struct WindowPlacement(WINDOWPLACEMENT);

pub struct WindowFrame {
    hwnd: isize,
    state: Arc<State>,
}
impl WindowFrame {
    pub fn install(hwnd: isize) -> io::Result<Self> {
        if hwnd == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid HWND"));
        }
        let state = Arc::new(State::default());
        let pointer = Box::into_raw(Box::new(Arc::clone(&state)));
        if unsafe {
            SetWindowSubclass(hwnd as HWND, Some(procedure), SUBCLASS_ID, pointer as usize)
        } == 0
        {
            unsafe {
                drop(Box::from_raw(pointer));
            }
            return Err(io::Error::last_os_error());
        }
        // Keep WS_THICKFRAME/WS_CAPTION so snapping, system menus and dragging a
        // maximized window still use Windows' own implementation.
        unsafe {
            SetWindowPos(
                hwnd as HWND,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
        Ok(Self { hwnd, state })
    }
    pub fn save_placement(&self) -> io::Result<WindowPlacement> {
        let mut placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        if unsafe { GetWindowPlacement(self.hwnd as HWND, &mut placement) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(WindowPlacement(placement))
    }
    pub fn restore_placement(&self, placement: &WindowPlacement) {
        unsafe {
            SetWindowPlacement(self.hwnd as HWND, &placement.0);
        }
    }
    pub fn set_layout(&self, layout: FrameLayout) {
        // A resize/fullscreen transition can move the caption beneath a stationary
        // pointer without delivering another mouse move. Reconcile hover too.
        let hwnd = self.hwnd as HWND;
        let mut point = POINT::default();
        let over_window =
            unsafe { GetCursorPos(&mut point) } != 0 && unsafe { WindowFromPoint(point) } == hwnd;
        unsafe {
            ScreenToClient(hwnd, &mut point);
        }
        let header_hovered = over_window && layout.header.is_some_and(|rect| contains(rect, point));
        if self
            .state
            .header_hovered
            .swap(header_hovered, Ordering::Relaxed)
            != header_hovered
        {
            unsafe {
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
        }
        let hovered = if over_window && !layout.fullscreen {
            layout
                .buttons
                .iter()
                .position(|rect| rect.is_some_and(|rect| contains(rect, point)))
                .map_or(0, |index| [HTMINBUTTON, HTMAXBUTTON, HTCLOSE][index])
        } else {
            0
        };
        redraw_hover(hwnd, &self.state, hovered);
        if let Ok(mut current) = self.state.layout.lock() {
            *current = layout;
        }
    }
    pub fn set_fullscreen(&self, fullscreen: bool) {
        if let Ok(mut layout) = self.state.layout.lock() {
            layout.fullscreen = fullscreen;
        }
        self.state.pressed.store(0, Ordering::Relaxed);
        self.state.hovered.store(0, Ordering::Relaxed);
        unsafe {
            ReleaseCapture();
            SetWindowPos(
                self.hwnd as HWND,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }
    pub fn header_hovered(&self) -> bool {
        self.state.header_hovered.load(Ordering::Relaxed)
    }
    pub fn hovered_button(&self) -> Option<usize> {
        button_index(self.state.hovered.load(Ordering::Relaxed))
    }
    pub fn pressed_button(&self) -> Option<usize> {
        button_index(self.state.pressed.load(Ordering::Relaxed))
    }
}

fn button_index(hit: u32) -> Option<usize> {
    [HTMINBUTTON, HTMAXBUTTON, HTCLOSE]
        .iter()
        .position(|code| *code == hit)
}

fn contains(rect: [i32; 4], point: POINT) -> bool {
    point.x >= rect[0] && point.x < rect[2] && point.y >= rect[1] && point.y < rect[3]
}
fn hit(
    layout: &FrameLayout,
    point: POINT,
    width: i32,
    height: i32,
    border: i32,
    maximized: bool,
) -> u32 {
    if layout.fullscreen {
        return HTCLIENT;
    }
    if !maximized {
        let left = point.x < border;
        let right = point.x >= width - border;
        let top = point.y < border;
        let bottom = point.y >= height - border;
        match (left, right, top, bottom) {
            (true, _, true, _) => return HTTOPLEFT,
            (_, true, true, _) => return HTTOPRIGHT,
            (true, _, _, true) => return HTBOTTOMLEFT,
            (_, true, _, true) => return HTBOTTOMRIGHT,
            (true, _, _, _) => return HTLEFT,
            (_, true, _, _) => return HTRIGHT,
            (_, _, true, _) => return HTTOP,
            (_, _, _, true) => return HTBOTTOM,
            _ => {}
        }
    }
    if let Some(index) = layout
        .buttons
        .iter()
        .position(|rect| rect.is_some_and(|rect| contains(rect, point)))
    {
        return [HTMINBUTTON, HTMAXBUTTON, HTCLOSE][index];
    }
    if layout.drag_enabled
        && layout.header.is_some_and(|rect| contains(rect, point))
        && !layout.excluded.iter().any(|rect| contains(*rect, point))
    {
        return HTCAPTION;
    }
    HTCLIENT
}

fn redraw_hover(hwnd: HWND, state: &State, hovered: u32) {
    if state.hovered.swap(hovered, Ordering::Relaxed) != hovered {
        unsafe {
            InvalidateRect(hwnd, std::ptr::null(), 0);
        }
    }
}

unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    pointer: usize,
) -> LRESULT {
    if message == WM_NCDESTROY {
        unsafe {
            RemoveWindowSubclass(hwnd, Some(procedure), SUBCLASS_ID);
            drop(Box::from_raw(pointer as *mut Arc<State>));
            return DefSubclassProc(hwnd, message, wparam, lparam);
        }
    }
    let state = unsafe { &*(pointer as *const Arc<State>) };
    let layout = state
        .layout
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    if message == WM_NCCALCSIZE {
        if wparam != 0 {
            let params = unsafe { &mut *(lparam as *mut NCCALCSIZE_PARAMS) };
            if !layout.fullscreen && unsafe { IsZoomed(hwnd) } != 0 {
                let mut monitor = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if unsafe {
                    GetMonitorInfoW(
                        MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                        &mut monitor,
                    )
                } != 0
                {
                    params.rgrc[0] = monitor.rcWork;
                }
            }
        }
        return 0;
    }
    if message == WM_NCHITTEST {
        let mut point = POINT {
            x: (lparam as u32 & 0xffff) as i16 as i32,
            y: ((lparam as u32 >> 16) & 0xffff) as i16 as i32,
        };
        let mut rect = RECT::default();
        unsafe {
            ScreenToClient(hwnd, &mut point);
            GetClientRect(hwnd, &mut rect);
        }
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let border = unsafe {
            GetSystemMetricsForDpi(SM_CXSIZEFRAME, dpi)
                + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
        }
        .max(4);
        return hit(
            &layout,
            point,
            rect.right,
            rect.bottom,
            border,
            unsafe { IsZoomed(hwnd) } != 0,
        ) as LRESULT;
    }
    if matches!(message, WM_MOUSEMOVE | WM_NCMOUSEMOVE) {
        let mut point = POINT {
            x: (lparam as u32 & 0xffff) as i16 as i32,
            y: ((lparam as u32 >> 16) & 0xffff) as i16 as i32,
        };
        if message == WM_NCMOUSEMOVE {
            unsafe {
                ScreenToClient(hwnd, &mut point);
            }
        }
        let hovered = layout.header.is_some_and(|rect| contains(rect, point));
        if state.header_hovered.swap(hovered, Ordering::Relaxed) != hovered {
            unsafe {
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
        }
        if message == WM_MOUSEMOVE {
            redraw_hover(hwnd, state, 0);
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                TrackMouseEvent(&mut track);
            }
        }
    }
    if matches!(message, WM_MOUSELEAVE | WM_NCMOUSELEAVE)
        && state.header_hovered.swap(false, Ordering::Relaxed)
    {
        unsafe {
            InvalidateRect(hwnd, std::ptr::null(), 0);
        }
    }
    if message == WM_NCMOUSEMOVE {
        redraw_hover(
            hwnd,
            state,
            if button_index(wparam as u32).is_some() {
                wparam as u32
            } else {
                0
            },
        );
        let mut track = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE | TME_NONCLIENT,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        unsafe {
            TrackMouseEvent(&mut track);
        }
    }
    if message == WM_NCMOUSELEAVE {
        redraw_hover(hwnd, state, 0);
    }
    if message == WM_NCLBUTTONDOWN && button_index(wparam as u32).is_some() {
        state.pressed.store(wparam as u32, Ordering::Relaxed);
        unsafe {
            SetCapture(hwnd);
            InvalidateRect(hwnd, std::ptr::null(), 0);
        }
        return 0;
    }
    let pressed = state.pressed.load(Ordering::Relaxed);
    if pressed != 0 && matches!(message, WM_MOUSEMOVE | WM_LBUTTONUP) {
        let point = POINT {
            x: (lparam as u32 & 0xffff) as i16 as i32,
            y: ((lparam as u32 >> 16) & 0xffff) as i16 as i32,
        };
        let hovered = button_index(pressed)
            .is_some_and(|index| layout.buttons[index].is_some_and(|rect| contains(rect, point)));
        redraw_hover(hwnd, state, if hovered { pressed } else { 0 });
        if message == WM_LBUTTONUP {
            state.pressed.store(0, Ordering::Relaxed);
            unsafe {
                ReleaseCapture();
                InvalidateRect(hwnd, std::ptr::null(), 0);
            }
            if hovered && !layout.fullscreen {
                let command = match pressed {
                    HTMINBUTTON => SC_MINIMIZE,
                    HTCLOSE => SC_CLOSE,
                    _ if unsafe { IsZoomed(hwnd) } != 0 => SC_RESTORE,
                    _ => SC_MAXIMIZE,
                };
                unsafe {
                    PostMessageW(hwnd, WM_SYSCOMMAND, command as usize, 0);
                }
            }
        }
        return 0;
    }
    if message == WM_CAPTURECHANGED && state.pressed.swap(0, Ordering::Relaxed) != 0 {
        unsafe {
            InvalidateRect(hwnd, std::ptr::null(), 0);
        }
    }
    // Never let DefWindowProc draw the old system caption on focus changes.
    if message == WM_NCPAINT {
        return 0;
    }
    if message == WM_NCACTIVATE {
        return unsafe { DefSubclassProc(hwnd, message, wparam, -1) };
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct HiddenWindow(HWND);
    impl Drop for HiddenWindow {
        fn drop(&mut self) {
            unsafe {
                DestroyWindow(self.0);
            }
        }
    }
    #[test]
    fn custom_frame_removes_caption_and_restores_saved_bounds_on_a_real_hidden_window() {
        let class = [83u16, 84, 65, 84, 73, 67, 0]; // Built-in STATIC class.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                100,
                100,
                800,
                600,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null());
        let _window = HiddenWindow(hwnd);
        let frame = WindowFrame::install(hwnd as isize).unwrap();
        let mut client = RECT::default();
        let mut outer = RECT::default();
        unsafe {
            GetClientRect(hwnd, &mut client);
            GetWindowRect(hwnd, &mut outer);
        }
        assert_eq!(
            client.right,
            outer.right - outer.left,
            "no system title bar or border consumes the client area"
        );
        assert_eq!(client.bottom, outer.bottom - outer.top);
        assert_ne!(
            unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32 & WS_THICKFRAME,
            0
        );
        let mut placement = frame.save_placement().unwrap();
        placement.0.showCmd = SW_HIDE as u32; // Keep this integration test invisible.
        frame.set_fullscreen(true);
        unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                120,
                160,
                1100,
                750,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        frame.set_fullscreen(false);
        frame.restore_placement(&placement);
        let restored = frame.save_placement().unwrap();
        let expected = placement.0.rcNormalPosition;
        let actual = restored.0.rcNormalPosition;
        assert_eq!(
            [actual.left, actual.top, actual.right, actual.bottom],
            [expected.left, expected.top, expected.right, expected.bottom]
        );
    }

    #[test]
    fn native_caption_buttons_request_system_actions_and_cancel_outside_releases() {
        let class = [83u16, 84, 65, 84, 73, 67, 0];
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                100,
                100,
                800,
                600,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null());
        let _window = HiddenWindow(hwnd);
        let frame = WindowFrame::install(hwnd as isize).unwrap();
        frame.set_layout(FrameLayout {
            header: Some([0, 0, 800, 44]),
            buttons: [
                Some([662, 0, 708, 44]),
                Some([708, 0, 754, 44]),
                Some([754, 0, 800, 44]),
            ],
            drag_enabled: true,
            ..Default::default()
        });
        for (index, (hit, command)) in [
            (HTMINBUTTON, SC_MINIMIZE),
            (HTMAXBUTTON, SC_MAXIMIZE),
            (HTCLOSE, SC_CLOSE),
        ]
        .into_iter()
        .enumerate()
        {
            let x = 685 + index * 46;
            unsafe {
                SendMessageW(hwnd, WM_NCLBUTTONDOWN, hit as usize, 0);
            }
            assert_eq!(frame.pressed_button(), Some(index));
            unsafe {
                SendMessageW(hwnd, WM_LBUTTONUP, 0, ((22 << 16) | x) as isize);
            }
            assert_eq!(frame.pressed_button(), None);
            let mut message = MSG::default();
            assert_ne!(
                unsafe {
                    PeekMessageW(&mut message, hwnd, WM_SYSCOMMAND, WM_SYSCOMMAND, PM_REMOVE)
                },
                0
            );
            assert_eq!(message.wParam, command as usize);
            // Consume rather than dispatch: this test never shows/minimizes/closes a visible window.
        }
        unsafe {
            SendMessageW(hwnd, WM_NCLBUTTONDOWN, HTCLOSE as usize, 0);
            SendMessageW(hwnd, WM_LBUTTONUP, 0, (100 << 16) | 500);
        }
        assert_eq!(frame.pressed_button(), None);
        let mut message = MSG::default();
        assert_eq!(
            unsafe { PeekMessageW(&mut message, hwnd, WM_SYSCOMMAND, WM_SYSCOMMAND, PM_REMOVE) },
            0
        );
    }
    #[test]
    fn native_hit_testing_keeps_controls_out_of_drag_and_fullscreen_out_of_resize() {
        let mut layout = FrameLayout {
            header: Some([0, 0, 1000, 44]),
            excluded: vec![[0, 0, 110, 44], [862, 0, 1000, 44]],
            buttons: [
                Some([862, 0, 908, 44]),
                Some([908, 0, 954, 44]),
                Some([954, 0, 1000, 44]),
            ],
            drag_enabled: true,
            ..Default::default()
        };
        let test = |layout: &FrameLayout, x, y, maximized| {
            hit(layout, POINT { x, y }, 1000, 700, 8, maximized)
        };
        assert_eq!(test(&layout, 500, 22, false), HTCAPTION);
        assert_eq!(test(&layout, 50, 22, false), HTCLIENT);
        assert_eq!(test(&layout, 930, 22, false), HTMAXBUTTON);
        assert_eq!(test(&layout, 977, 22, false), HTCLOSE);
        assert_eq!(test(&layout, 877, 22, false), HTMINBUTTON);
        assert_eq!(test(&layout, 1, 1, false), HTTOPLEFT);
        assert_eq!(test(&layout, 999, 699, false), HTBOTTOMRIGHT);
        assert_eq!(test(&layout, 500, 1, true), HTCAPTION);
        layout.fullscreen = true;
        assert_eq!(test(&layout, 930, 22, false), HTCLIENT);
        assert_eq!(test(&layout, 1, 1, false), HTCLIENT);
    }
}
