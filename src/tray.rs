//! The tray icon, on its own thread with its own message loop, so it keeps working while the window is
//! hidden (eframe does not run frames for a hidden window). The icon shows the hotter of CPU/GPU.

use std::time::Duration;

use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, QS_ALLINPUT,
    TranslateMessage, WM_HOTKEY,
};

use crate::acer::power;
use crate::lighting::temp_color;
use crate::settings::FanKind;
use crate::state::{self, Cmd};
use crate::window;

pub fn spawn() {
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(run)
        .ok();
}

fn run() {
    let power_items: Vec<(u8, CheckMenuItem)> = power::ALL
        .iter()
        .map(|&p| (p, CheckMenuItem::new(power::name(p), true, false, None)))
        .collect();
    let fan_kinds = [
        FanKind::Auto,
        FanKind::AutoPlus,
        FanKind::Max,
        FanKind::Fixed,
        FanKind::Curve,
    ];
    let fan_items: Vec<(FanKind, CheckMenuItem)> = fan_kinds
        .iter()
        .map(|&k| (k, CheckMenuItem::new(k.name(), true, false, None)))
        .collect();
    let lights_off = CheckMenuItem::new("Keyboard light off", true, false, None);
    let logo = CheckMenuItem::new("Lid logo light", true, false, None);
    let open = MenuItem::new("Open Kelvin", true, None);
    let exit = MenuItem::new("Exit", true, None);

    let power_menu = Submenu::new("Power mode", true);
    for (_, i) in &power_items {
        let _ = power_menu.append(i);
    }
    let fan_menu = Submenu::new("Fans", true);
    for (_, i) in &fan_items {
        let _ = fan_menu.append(i);
    }
    let light_menu = Submenu::new("Lighting", true);
    let _ = light_menu.append(&lights_off);
    let _ = light_menu.append(&logo);

    let menu = Menu::new();
    let _ = menu.append(&open);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&power_menu);
    let _ = menu.append(&fan_menu);
    let _ = menu.append(&light_menu);
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&exit);

    let app_icon = || Icon::from_resource(1, Some((32, 32))).ok();
    let Ok(tray) = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Kelvin")
        .with_icon(app_icon().unwrap_or_else(|| temp_icon(0)))
        .build()
    else {
        return;
    };

    let mut shown_temp: Option<u32> = None;
    let mut shown_version = u64::MAX;
    let mut shown_tip = String::new();
    let mut hotkeys_on = false;
    loop {
        unsafe {
            MsgWaitForMultipleObjects(None, false, 500, QS_ALLINPUT);
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                if msg.message == WM_HOTKEY {
                    hotkey(msg.wParam.0 as i32);
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let want_hotkeys = state::shared().settings.hotkeys;
        if want_hotkeys != hotkeys_on {
            hotkeys_on = want_hotkeys;
            set_hotkeys(want_hotkeys);
        }

        while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = ev
            {
                window::toggle();
            }
        }
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let id = &ev.id;
            if id == open.id() {
                window::show();
            } else if id == exit.id() {
                quit();
            } else if id == lights_off.id() {
                state::update(|s| s.lighting.enabled = !s.lighting.enabled);
            } else if id == logo.id() {
                state::update(|s| s.lighting.logo_on = !s.lighting.logo_on);
            } else if let Some((p, _)) = power_items.iter().find(|(_, i)| i.id() == id) {
                let p = *p;
                state::update(|s| s.power = p);
            } else if let Some((k, _)) = fan_items.iter().find(|(_, i)| i.id() == id) {
                let k = *k;
                state::update(|s| s.fan = k);
            }
            // A clicked check item toggles itself; force a resync below.
            shown_version = u64::MAX;
        }

        let (version, settings, tele) = {
            let s = state::shared();
            (s.version, s.settings.clone(), s.tele.clone())
        };
        if version != shown_version {
            shown_version = version;
            for (p, i) in &power_items {
                i.set_checked(*p == settings.power);
                i.set_enabled(power::allowed(*p, tele.on_ac));
            }
            for (k, i) in &fan_items {
                i.set_checked(*k == settings.fan);
            }
            lights_off.set_checked(!settings.lighting.enabled);
            logo.set_checked(settings.lighting.logo_on);
        }

        let hot = tele.cpu_temp.max(tele.gpu_temp);
        let want = (settings.tray_shows_temp && hot > 0).then_some(hot.min(99));
        if want != shown_temp {
            shown_temp = want;
            let icon = match want {
                Some(t) => Some(temp_icon(t)),
                None => app_icon(),
            };
            let _ = tray.set_icon(icon);
        }
        let tip = format!(
            "Kelvin\nCPU {}°C  GPU {}°C\n{} · fans {}",
            tele.cpu_temp,
            tele.gpu_temp,
            power::name(tele.power),
            tele.fan_text
        );
        if tip != shown_tip {
            let _ = tray.set_tooltip(Some(&tip));
            shown_tip = tip;
        }
    }
}

/// Ctrl+Alt+ P power mode, F fans (Auto → Auto+ → Max), L lights, O window. Bound to this thread.
const HOTKEYS: [(i32, u32); 4] = [(1, 0x50), (2, 0x46), (3, 0x4C), (4, 0x4F)];

fn set_hotkeys(on: bool) {
    for (id, vk) in HOTKEYS {
        unsafe {
            if on {
                if RegisterHotKey(None, id, MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, vk).is_err() {
                    state::log(format!(
                        "Shortcut Ctrl+Alt+{} is taken by another app",
                        vk as u8 as char
                    ));
                }
            } else {
                let _ = UnregisterHotKey(None, id);
            }
        }
    }
}

fn hotkey(id: i32) {
    match id {
        1 => state::send(Cmd::CyclePower),
        2 => state::update(|s| {
            s.fan = match s.fan {
                FanKind::Auto => FanKind::AutoPlus,
                FanKind::AutoPlus => FanKind::Max,
                _ => FanKind::Auto,
            }
        }),
        3 => state::update(|s| s.lighting.enabled = !s.lighting.enabled),
        4 => window::toggle(),
        _ => {}
    }
}

/// Restores the fans, saves, and ends the process (the window may be hidden and not running frames).
pub fn quit() {
    let (tx, rx) = std::sync::mpsc::channel();
    state::send(Cmd::Shutdown(tx));
    let _ = rx.recv_timeout(Duration::from_secs(5));
    std::process::exit(0);
}

/// 5×7 digits, one row per byte, bit 4 = left column.
const DIGITS: [[u8; 7]; 10] = [
    [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
    [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
    [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
    [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
    [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
    [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
    [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
    [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
    [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
    [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
];

/// 32×32: two digits at 3× scale in the temperature's colour on a dark rounded square.
fn temp_icon(temp: u32) -> Icon {
    const N: usize = 32;
    let mut px = vec![0u8; N * N * 4];
    let bg = [24u8, 24, 28, 255];
    for y in 0..N {
        for x in 0..N {
            // Rounded corners (radius 5).
            let cx = if x < 5 {
                5 - x
            } else if x >= N - 5 {
                x - (N - 6)
            } else {
                0
            };
            let cy = if y < 5 {
                5 - y
            } else if y >= N - 5 {
                y - (N - 6)
            } else {
                0
            };
            if cx * cx + cy * cy <= 25 {
                px[(y * N + x) * 4..][..4].copy_from_slice(&bg);
            }
        }
    }
    let c = temp_color(temp);
    let fg = [c[0], c[1], c[2], 255];
    let digits = [(temp / 10 % 10) as usize, (temp % 10) as usize];
    let scale = 3;
    let top = (N - 7 * scale) / 2;
    for (d, &digit) in digits.iter().enumerate() {
        let left = 1 + d * (5 * scale + 1);
        for (row, bits) in DIGITS[digit].iter().enumerate() {
            for col in 0..5 {
                if bits & (0x10 >> col) != 0 {
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let (x, y) = (left + col * scale + dx, top + row * scale + dy);
                            px[(y * N + x) * 4..][..4].copy_from_slice(&fg);
                        }
                    }
                }
            }
        }
    }
    Icon::from_rgba(px, N as u32, N as u32).expect("32x32 RGBA")
}
