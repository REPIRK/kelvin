//! The main window's native handle: shown and hidden from the tray thread with plain Win32 calls, because a
//! hidden eframe window runs no frames to act on viewport commands.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetWindowThreadProcessId, ICON_BIG, ICON_SMALL, IMAGE_ICON, IsIconic,
    IsWindowVisible, LR_DEFAULTCOLOR, LoadImageW, SW_HIDE, SW_RESTORE, SW_SHOW, SendMessageW,
    SetForegroundWindow, ShowWindow, WM_SETICON,
};
use windows::core::{PCWSTR, w};

use crate::state;

pub const TITLE: &str = "Kelvin";

/// Our window, or with `any_process` the window of another running copy.
fn find(any_process: bool) -> Option<HWND> {
    let me = unsafe { GetCurrentProcessId() };
    let mut after = None;
    loop {
        let h = unsafe { FindWindowExW(None, after, PCWSTR::null(), w!("Kelvin")) }.ok()?;
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if any_process != (pid == me) {
            return Some(h);
        }
        after = Some(h);
    }
}

pub fn show() {
    if let Some(h) = find(false) {
        unsafe {
            let _ = ShowWindow(
                h,
                if IsIconic(h).as_bool() {
                    SW_RESTORE
                } else {
                    SW_SHOW
                },
            );
            let _ = SetForegroundWindow(h);
        }
        state::shared().window_visible = true;
        state::repaint();
    }
}

pub fn hide() {
    if let Some(h) = find(false) {
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
    state::shared().window_visible = false;
}

pub fn toggle() {
    let visible = find(false)
        .is_some_and(|h| unsafe { IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() });
    if visible { hide() } else { show() }
}

/// A second launch brings the first copy forward.
pub fn show_other_instance() {
    if let Some(h) = find(true) {
        unsafe {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
        }
    }
}

/// The exe's own icon instead of eframe's, and a dark title bar.
pub fn dress() {
    let Some(h) = find(false) else { return };
    unsafe {
        let dark: i32 = 1;
        let _ = DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
        if let Ok(module) = GetModuleHandleW(None) {
            for (which, size) in [(ICON_BIG, 32), (ICON_SMALL, 16)] {
                if let Ok(icon) = LoadImageW(
                    Some(module.into()),
                    PCWSTR(1 as _),
                    IMAGE_ICON,
                    size,
                    size,
                    LR_DEFAULTCOLOR,
                ) {
                    SendMessageW(
                        h,
                        WM_SETICON,
                        Some(WPARAM(which as usize)),
                        Some(LPARAM(icon.0 as isize)),
                    );
                }
            }
        }
    }
}
