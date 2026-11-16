//! "Screen off" that is not a display power-off. On Modern Standby laptops (this one) SC_MONITORPOWER
//! puts the whole machine to sleep within a second, and a display that is off stops games presenting
//! frames. So the panel goes to brightness 0 under a black overlay instead: Windows still sees the display
//! on, nothing sleeps, games keep rendering.
//!
//! The overlay is layered at alpha 254, not 255: DWM treats a fully opaque topmost window as hiding the
//! windows under it, and games told they are occluded stop drawing. It is click-through, never takes
//! focus and stays off the taskbar, so the game keeps its input.

use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetSystemMetrics, HWND_TOPMOST, LWA_ALPHA, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SetLayeredWindowAttributes, SetWindowPos, ShowWindow,
    TranslateMessage, WM_CLOSE, WM_DESTROY, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

use crate::settings::data_dir;

static OVERLAY: AtomicIsize = AtomicIsize::new(0);

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// Covers every monitor with the overlay, on a thread of its own (it needs a message loop).
pub fn show_overlay() {
    if OVERLAY.load(Ordering::SeqCst) != 0 {
        return;
    }
    std::thread::Builder::new()
        .name("dark-overlay".into())
        .spawn(|| unsafe {
            let Ok(module) = GetModuleHandleW(None) else {
                return;
            };
            let class = w!("KelvinDark");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: module.into(),
                lpszClassName: class,
                hbrBackground: HBRUSH(CreateSolidBrush(COLORREF(0)).0),
                ..Default::default()
            };
            RegisterClassW(&wc);
            let (x, y) = (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
            );
            let (cx, cy) = (
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            );
            let Ok(hwnd) = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW,
                class,
                w!("Kelvin dark"),
                WS_POPUP,
                x,
                y,
                cx,
                cy,
                None,
                None,
                Some(module.into()),
                None,
            ) else {
                return;
            };
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 254, LWA_ALPHA);
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, cx, cy, SWP_NOACTIVATE);
            OVERLAY.store(hwnd.0 as isize, Ordering::SeqCst);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            OVERLAY.store(0, Ordering::SeqCst);
        })
        .ok();
}

pub fn hide_overlay() {
    let h = OVERLAY.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// The brightness to come back to, kept on disk while dark so a crash or kill is undone on next start.
pub fn save_brightness(v: u8) {
    let _ = std::fs::write(data_dir().join("brightness_before_dark.txt"), v.to_string());
}

pub fn take_saved_brightness() -> Option<u8> {
    let path = data_dir().join("brightness_before_dark.txt");
    let v = std::fs::read_to_string(&path).ok()?.trim().parse().ok();
    let _ = std::fs::remove_file(path);
    v
}
