//! State shared between the hardware thread (`core`), the window and the tray.
//! Settings are changed only through [`update`], which wakes the hardware thread.

use std::collections::VecDeque;
use std::sync::mpsc::Sender;
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::settings::Settings;

pub const HISTORY_LEN: usize = 300; // 10 minutes at one sample per 2 s

#[derive(Clone, Copy, Default)]
pub struct Sample {
    pub cpu_temp: u16,
    pub gpu_temp: u16,
    pub cpu_rpm: u16,
    pub gpu_rpm: u16,
}

#[derive(Clone, Default)]
pub struct Telemetry {
    pub cpu_temp: u32,
    pub gpu_temp: u32,
    pub cpu_rpm: u32,
    pub gpu_rpm: u32,
    pub on_ac: bool,
    /// What runs now, after game, night and battery rules.
    pub power: u8,
    pub fan_text: String,
    pub notes: Vec<String>,
    /// Processes holding the NVIDIA GPU: (pid, exe name, full path, engine use %).
    pub rtx_apps: Vec<(u32, String, String, f64)>,
    pub turbo_off: bool,
}

#[derive(Clone, Default)]
pub struct Caps {
    pub acer: bool,
    pub hid_lighting: bool,
    pub battery_limit: bool,
    pub boot_effect: Option<bool>,
    pub usb_charging: Option<u8>,
    pub max_hz: u32,
    pub startup: Option<bool>,
    pub error: Option<String>,
    /// Other programs that also drive the fans and lighting: (pid, exe path).
    pub rivals: Vec<(u32, String)>,
    /// Progress or result of the fan test.
    pub fan_test: Option<String>,
    pub overdrive: Option<bool>,
    /// GPU mode now (1 Optimus, 2 NVIDIA only, 3 automatic) and the one set for the next start.
    pub gpu_mode: Option<u8>,
    pub gpu_mode_next: Option<u8>,
    /// Acer's bitmask of offered GPU modes (bit 0 Optimus, 1 NVIDIA only, 2 automatic).
    pub gpu_modes: Option<u32>,
}

pub enum Cmd {
    SettingsChanged,
    /// Raw payload of an `AcerGenericEvent`.
    FirmwareEvent(Vec<u8>),
    /// Next power mode (mode key, hotkey).
    CyclePower,
    /// Custom duty 20% then 100%: do the fans follow duty writes?
    FanTest,
    /// GPU mode for the next start.
    SetGpuMode(u8),
    /// Move an app (full path) to the integrated GPU.
    PreferIntegrated(String),
    /// End the programs in `Caps::rivals`.
    CloseRivals,
    /// Restore fans and save; the sender is answered when done.
    Shutdown(Sender<()>),
}

pub struct Shared {
    pub settings: Settings,
    pub version: u64,
    pub tele: Telemetry,
    pub history: VecDeque<Sample>,
    pub caps: Caps,
    /// Recent firmware events and notable actions, newest last.
    pub log: VecDeque<String>,
    pub window_visible: bool,
}

static SHARED: OnceLock<Mutex<Shared>> = OnceLock::new();
static CMD: OnceLock<Sender<Cmd>> = OnceLock::new();
static CTX: OnceLock<eframe::egui::Context> = OnceLock::new();

pub fn init(settings: Settings, tx: Sender<Cmd>) {
    let _ = SHARED.set(Mutex::new(Shared {
        settings,
        version: 0,
        tele: Telemetry::default(),
        history: VecDeque::with_capacity(HISTORY_LEN),
        caps: Caps::default(),
        log: VecDeque::new(),
        window_visible: false,
    }));
    let _ = CMD.set(tx);
}

pub fn shared() -> MutexGuard<'static, Shared> {
    SHARED
        .get()
        .expect("state::init")
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

pub fn send(cmd: Cmd) {
    if let Some(tx) = CMD.get() {
        let _ = tx.send(cmd);
    }
}

/// Changes the settings and lets the hardware thread apply them.
pub fn update(f: impl FnOnce(&mut Settings)) {
    {
        let mut s = shared();
        let before = s.settings.clone();
        f(&mut s.settings);
        if s.settings == before {
            return;
        }
        s.version += 1;
    }
    send(Cmd::SettingsChanged);
    repaint();
}

pub fn log(line: impl Into<String>) {
    let line = line.into();
    let mut s = shared();
    if s.log.len() >= 30 {
        s.log.pop_front();
    }
    s.log.push_back(line);
}

pub fn set_ctx(ctx: eframe::egui::Context) {
    let _ = CTX.set(ctx);
}

pub fn repaint() {
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}
