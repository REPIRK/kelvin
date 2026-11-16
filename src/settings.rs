//! What the user chose. Saved as JSON in %LOCALAPPDATA%\Kelvin\settings.json.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::acer::power;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum FanKind {
    Auto,
    /// The EC's Auto, run NL faster (learned, see `fans.rs`).
    AutoPlus,
    Max,
    Fixed,
    Curve,
}

impl FanKind {
    pub fn name(self) -> &'static str {
        match self {
            FanKind::Auto => "Auto",
            FanKind::AutoPlus => "Auto+",
            FanKind::Max => "Max",
            FanKind::Fixed => "Fixed",
            FanKind::Curve => "Curve",
        }
    }
    /// Modes this app steers in the EC's custom behaviour (and therefore guards).
    pub fn is_steered(self) -> bool {
        matches!(self, FanKind::AutoPlus | FanKind::Fixed | FanKind::Curve)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect {
    Static,
    Breathing,
    Neon,
    Wave,
    Shifting,
    Zoom,
    Meteor,
    Twinkling,
    /// Static colour from the hotter of CPU/GPU.
    Temperature,
}

impl Effect {
    pub const ALL: [Effect; 9] = [
        Effect::Static,
        Effect::Breathing,
        Effect::Neon,
        Effect::Wave,
        Effect::Shifting,
        Effect::Zoom,
        Effect::Meteor,
        Effect::Twinkling,
        Effect::Temperature,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Effect::Static => "Static",
            Effect::Breathing => "Breathing",
            Effect::Neon => "Neon",
            Effect::Wave => "Wave",
            Effect::Shifting => "Shifting",
            Effect::Zoom => "Zoom",
            Effect::Meteor => "Meteor",
            Effect::Twinkling => "Twinkling",
            Effect::Temperature => "Temperature",
        }
    }
    /// Effects that take a colour (Acer's own mode flags: HAS_MODE_SPECIFIC_COLOR).
    pub fn uses_color(self) -> bool {
        matches!(
            self,
            Effect::Static
                | Effect::Breathing
                | Effect::Shifting
                | Effect::Zoom
                | Effect::Meteor
                | Effect::Twinkling
        )
    }
    pub fn uses_speed(self) -> bool {
        !matches!(self, Effect::Static | Effect::Temperature)
    }
    pub fn uses_direction(self) -> bool {
        matches!(self, Effect::Wave | Effect::Shifting)
    }
}

pub type Rgb = [u8; 3];

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Lighting {
    /// Keyboard, logo and mode key lit at all.
    pub enabled: bool,
    pub effect: Effect,
    pub brightness: u8,
    /// 1..=9.
    pub speed: u8,
    pub direction_right_to_left: bool,
    /// Keyboard zones, left to right.
    pub zones: [Rgb; 4],
    pub logo_color: Rgb,
    /// Editing one swatch sets all zones and the logo.
    pub same_color: bool,
    pub logo_on: bool,
    /// The mode key's LED shows the power mode.
    pub mode_key_shows_power: bool,
    pub idle_off: bool,
    pub idle_minutes: u16,
}

impl Default for Lighting {
    fn default() -> Self {
        let c = [0, 200, 160];
        Self {
            enabled: true,
            effect: Effect::Static,
            brightness: 100,
            speed: 5,
            direction_right_to_left: false,
            zones: [c; 4],
            logo_color: c,
            same_color: true,
            logo_on: false,
            mode_key_shows_power: true,
            idle_off: true,
            idle_minutes: 5,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Night {
    pub enabled: bool,
    /// Minutes after midnight.
    pub start: u16,
    pub end: u16,
    pub power: Option<u8>,
    pub fans_auto: bool,
    pub lights_off: bool,
}

impl Default for Night {
    fn default() -> Self {
        Self {
            enabled: false,
            start: 23 * 60,
            end: 8 * 60,
            power: Some(power::QUIET),
            fans_auto: true,
            lights_off: true,
        }
    }
}

impl Night {
    pub fn active_at(&self, minute: u16) -> bool {
        if !self.enabled || self.start == self.end {
            return false;
        }
        if self.start < self.end {
            minute >= self.start && minute < self.end
        } else {
            minute >= self.start || minute < self.end
        }
    }
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug, Default)]
#[serde(default)]
pub struct GameProfile {
    /// Executable file name, e.g. "cs2.exe".
    pub exe: String,
    pub power: Option<u8>,
    pub fan: Option<FanKind>,
    pub refresh_max: Option<bool>,
    pub effect: Option<Effect>,
    pub color: Option<Rgb>,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    pub power: u8,
    pub power_on_ac: Option<u8>,
    pub power_on_battery: Option<u8>,

    pub fan: FanKind,
    pub fan_on_ac: Option<FanKind>,
    pub fan_on_battery: Option<FanKind>,
    /// Auto+ percent.
    pub boost: u8,
    pub fixed_cpu: u8,
    pub fixed_gpu: u8,
    /// (°C, L) points.
    pub curve_cpu: Vec<[u8; 2]>,
    pub curve_gpu: Vec<[u8; 2]>,

    pub lighting: Lighting,
    pub saved_colors: Vec<Rgb>,
    pub night: Night,

    pub refresh_max: bool,
    pub battery_limit: bool,
    /// None: leave the firmware's value.
    pub usb_charging: Option<u8>,
    pub boot_effect: Option<bool>,
    /// The PredatorSense / mode key cycles power modes.
    pub key_cycles_power: bool,
    pub tray_shows_temp: bool,
    /// No CPU turbo in Quiet and Eco.
    pub turbo_off_quiet: bool,
    /// No CPU turbo on battery.
    pub turbo_off_battery: bool,
    /// Panel overdrive; None leaves the firmware's value.
    pub overdrive: Option<bool>,
    /// A logon task starts the app hidden in the tray; kept pointing at this exe.
    pub start_with_windows: bool,
    /// Ctrl+Alt+P / F / L / O.
    pub hotkeys: bool,

    pub games_enabled: bool,
    pub games: Vec<GameProfile>,
}

pub fn default_curve() -> Vec<[u8; 2]> {
    vec![
        [30, 10],
        [45, 15],
        [55, 30],
        [65, 50],
        [72, 65],
        [80, 80],
        [88, 92],
        [95, 100],
    ]
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            power: power::BALANCED,
            power_on_ac: None,
            power_on_battery: None,
            fan: FanKind::Auto,
            fan_on_ac: None,
            fan_on_battery: None,
            boost: 10,
            fixed_cpu: 50,
            fixed_gpu: 50,
            curve_cpu: default_curve(),
            curve_gpu: default_curve(),
            lighting: Lighting::default(),
            saved_colors: vec![
                [0, 200, 160],
                [255, 255, 255],
                [255, 60, 60],
                [255, 140, 0],
                [255, 220, 50],
                [60, 220, 90],
                [0, 120, 255],
                [170, 80, 255],
            ],
            night: Night::default(),
            refresh_max: true,
            battery_limit: false,
            usb_charging: None,
            boot_effect: None,
            key_cycles_power: true,
            tray_shows_temp: true,
            turbo_off_quiet: true,
            turbo_off_battery: false,
            overdrive: None,
            start_with_windows: true,
            hotkeys: true,
            games_enabled: false,
            games: Vec::new(),
        }
    }
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("Kelvin");
    if !dir.exists() {
        // Builds before the rename kept their files under PredatorControl (shared with the C# app's
        // game_sync.json, which stays). Copy ours over once.
        let _ = std::fs::create_dir_all(&dir);
        let old = base.join("PredatorControl");
        for f in ["settings.json", "fan_learning.json", "turbo_original.json"] {
            let _ = std::fs::copy(old.join(f), dir.join(f));
        }
    }
    dir
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(data_dir().join("settings.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = data_dir().join("settings.json");
        let tmp = path.with_extension("json.tmp");
        if let Ok(json) = serde_json::to_string_pretty(self)
            && std::fs::write(&tmp, json).is_ok()
        {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Linear interpolation over (°C, L) points.
pub fn curve_at(curve: &[[u8; 2]], temp: f32) -> u8 {
    let Some(first) = curve.first() else {
        return 50;
    };
    let last = curve[curve.len() - 1];
    if temp <= first[0] as f32 {
        return first[1];
    }
    if temp >= last[0] as f32 {
        return last[1];
    }
    for w in curve.windows(2) {
        let (a, b) = (w[0], w[1]);
        if temp >= a[0] as f32 && temp <= b[0] as f32 {
            let span = (b[0] as f32 - a[0] as f32).max(1.0);
            let t = (temp - a[0] as f32) / span;
            return (a[1] as f32 + t * (b[1] as f32 - a[1] as f32)).round() as u8;
        }
    }
    last[1]
}
