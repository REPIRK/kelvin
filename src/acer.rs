//! The Acer firmware interfaces in `root\WMI`:
//! - `AcerGamingFunction`: power profiles, fans, sensors, misc settings (method ids per Linux acer-wmi).
//! - `AcerGenericMethod`: the "function" interface (USB charging while off).
//! - `BatteryControl`: the 80% charge limit.

use crate::wmi::{Arg, Value, Wmi};

const GAMING: &str = "AcerGamingFunction";
const GENERIC: &str = "AcerGenericMethod";
const BATTERY: &str = "BatteryControl";

/// Power profiles (misc setting 0x0B values).
pub mod power {
    pub const QUIET: u8 = 0x00;
    pub const BALANCED: u8 = 0x01;
    pub const PERFORMANCE: u8 = 0x04;
    pub const TURBO: u8 = 0x05;
    pub const ECO: u8 = 0x06;
    pub const ALL: [u8; 5] = [QUIET, BALANCED, PERFORMANCE, TURBO, ECO];

    pub fn name(mode: u8) -> &'static str {
        match mode {
            QUIET => "Quiet",
            PERFORMANCE => "Performance",
            TURBO => "Turbo",
            ECO => "Eco",
            _ => "Balanced",
        }
    }
    pub fn short(mode: u8) -> &'static str {
        match mode {
            QUIET => "Quiet",
            PERFORMANCE => "Perf",
            TURBO => "Turbo",
            ECO => "Eco",
            _ => "Balanced",
        }
    }
    pub fn allowed(mode: u8, on_ac: bool) -> bool {
        match mode {
            PERFORMANCE | TURBO => on_ac,
            ECO => !on_ac,
            _ => true,
        }
    }
}

/// EC fan behaviour per fan group.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FanHw {
    Auto = 1,
    Max = 2,
    Custom = 3,
}

/// USB charging while the laptop is off: off, or until the battery is down to 10/20/30%.
pub const USB_CHARGE_LEVELS: [u8; 4] = [0, 10, 20, 30];

pub struct Acer {
    wmi: Option<Wmi>,
    pub last_error: Option<String>,
}

impl Acer {
    pub fn new() -> Self {
        let mut a = Self {
            wmi: None,
            last_error: None,
        };
        a.reconnect();
        a
    }

    pub fn reconnect(&mut self) {
        match Wmi::connect("ROOT\\WMI") {
            Ok(w) => {
                self.wmi = Some(w);
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(e);
                self.wmi = None;
            }
        }
    }

    pub fn available(&mut self) -> bool {
        self.wmi.as_mut().is_some_and(|w| w.has_instance(GAMING))
    }

    fn call(
        &mut self,
        class: &str,
        method: &str,
        args: &[(&str, Arg)],
        outputs: &[&str],
    ) -> Option<Vec<Value>> {
        for attempt in 0..2 {
            let Some(w) = self.wmi.as_mut() else {
                self.reconnect();
                continue;
            };
            match w.call(class, method, args, outputs) {
                Ok(v) => return Some(v),
                Err(e) => {
                    if self.last_error.as_ref() != Some(&e) {
                        crate::diag::log(format!("WMI {method}: {e}"));
                    }
                    self.last_error = Some(e);
                    if attempt == 0 {
                        self.reconnect();
                    }
                }
            }
        }
        None
    }

    /// A gaming call with a 64-bit input; returns the raw output when the call went through.
    /// The status byte is logged but not trusted: on the PHN16-73 the set methods can report a non-zero
    /// status while they work, and treating that as failure resent the fan mode every tick (which restarts
    /// the fans' ramp). The C# app ignored it for the same reason.
    fn gaming_set(&mut self, method: &str, input: u64) -> Option<u64> {
        let out = self.call(
            GAMING,
            method,
            &[("gmInput", Arg::U64(input))],
            &["gmOutput"],
        )?;
        let v = out[0].int().unwrap_or(0);
        if method != "SetGamingFanSpeed" || v & 0xFF != 0 {
            crate::diag::log(format!("{method}({input:#X}) -> {v:#X}"));
        }
        Some(v)
    }

    fn gaming_get(&mut self, method: &str, input: u32) -> Option<u64> {
        let out = self.call(
            GAMING,
            method,
            &[("gmInput", Arg::U32(input))],
            &["gmOutput"],
        )?;
        let v = out[0].int()?;
        (v & 0xFF == 0).then_some(v)
    }

    pub fn set_power(&mut self, mode: u8) -> bool {
        self.gaming_set("SetGamingMiscSetting", 0x0B | ((mode as u64) << 8))
            .is_some()
    }

    pub fn get_power(&mut self) -> Option<u8> {
        self.gaming_get("GetGamingMiscSetting", 0x0B)
            .map(|v| ((v >> 8) & 0xFF) as u8)
    }

    /// Both fan groups (CPU bit 0x1, GPU bit 0x8) to one behaviour.
    pub fn set_fan_behavior(&mut self, hw: FanHw) -> bool {
        let m = hw as u64;
        self.gaming_set("SetGamingFanBehavior", 0x09 | (m << 16) | (m << 22))
            .is_some()
    }

    /// Duty in percent for the CPU (id 1) and GPU (id 4) fans; only takes effect in custom behaviour.
    pub fn set_fan_speed(&mut self, cpu: u8, gpu: u8) -> bool {
        let a = self
            .gaming_set("SetGamingFanSpeed", 0x01 | ((cpu.min(100) as u64) << 8))
            .is_some();
        let b = self
            .gaming_set("SetGamingFanSpeed", 0x04 | ((gpu.min(100) as u64) << 8))
            .is_some();
        a && b
    }

    fn sensor(&mut self, id: u32) -> u32 {
        self.gaming_get("GetGamingSysInfo", 0x01 | (id << 8))
            .map(|v| ((v >> 8) & 0xFFFF) as u32)
            .unwrap_or(0)
    }

    pub fn cpu_temp(&mut self) -> u32 {
        self.sensor(0x01)
    }
    pub fn gpu_temp(&mut self) -> u32 {
        self.sensor(0x0A)
    }
    pub fn cpu_fan_rpm(&mut self) -> u32 {
        self.sensor(0x02)
    }
    pub fn gpu_fan_rpm(&mut self) -> u32 {
        self.sensor(0x06)
    }

    /// GPU mode (misc setting 2): 1 Optimus, 2 NVIDIA only, 3 automatic. Takes effect after a restart.
    /// From Linuwu-Sense PR #154, which reads it from Acer's agent service.
    pub fn get_gpu_mode(&mut self) -> Option<u8> {
        self.gaming_get("GetGamingMiscSetting", 0x02)
            .map(|v| ((v >> 8) & 0xFF) as u8)
            .filter(|m| (1..=3).contains(m))
    }

    pub fn set_gpu_mode(&mut self, mode: u8) -> bool {
        self.gaming_set("SetGamingMiscSetting", 0x02 | ((mode as u64) << 8))
            .is_some_and(|v| v & 0xFF == 0)
    }

    /// Panel overdrive (faster pixel response). Values from Linuwu-Sense's lcd_override.
    pub fn get_overdrive(&mut self) -> Option<bool> {
        match self.gaming_get("GetGamingProfile", 0x00)? {
            0x1000001000000 => Some(true),
            0x1000000 => Some(false),
            v if v & 0x1000000 != 0 => Some(v & 0x1_0000_0000_0000 != 0),
            _ => None,
        }
    }

    pub fn set_overdrive(&mut self, on: bool) -> bool {
        self.gaming_set("SetGamingProfile", if on { 0x1000000000010 } else { 0x10 })
            .is_some()
    }

    /// Panel brightness 0-100, through Windows' own WMI brightness classes (same namespace).
    pub fn get_brightness(&mut self) -> Option<u8> {
        self.wmi
            .as_mut()?
            .get_u64("WmiMonitorBrightness", "CurrentBrightness")
            .ok()
            .map(|v| v.min(100) as u8)
    }

    pub fn set_brightness(&mut self, v: u8) -> bool {
        self.call(
            "WmiMonitorBrightnessMethods",
            "WmiSetBrightness",
            &[
                ("Timeout", Arg::U32(0)),
                ("Brightness", Arg::U8(v.min(100))),
            ],
            &[],
        )
        .is_some()
    }

    /// Boot animation and sound (misc setting 6). None when the firmware does not answer.
    pub fn get_boot_effect(&mut self) -> Option<bool> {
        self.gaming_get("GetGamingMiscSetting", 0x06)
            .map(|v| (v >> 8) & 0xFF != 0)
    }

    pub fn set_boot_effect(&mut self, on: bool) -> bool {
        self.gaming_set("SetGamingMiscSetting", if on { 0x106 } else { 0x06 })
            .is_some()
    }

    /// USB charging while off, as 0/10/20/30. The firmware's encoding (from Linuwu-Sense) is opaque:
    /// these exact values are what it returns and accepts.
    pub fn get_usb_charging(&mut self) -> Option<u8> {
        let out = self.call(
            GENERIC,
            "GetFunction",
            &[("uiInput", Arg::U32(0x4))],
            &["uiOutput"],
        )?;
        match out[0].int()? {
            663296 => Some(0),
            659200 => Some(10),
            1314560 => Some(20),
            1969920 => Some(30),
            _ => None,
        }
    }

    pub fn set_usb_charging(&mut self, level: u8) -> bool {
        let input: u64 = match level {
            10 => 659204,
            20 => 1314564,
            30 => 1969924,
            _ => 663300,
        };
        self.call(
            GENERIC,
            "SetFunction",
            &[("uiInput", Arg::U64(input))],
            &["uiOutput"],
        )
        .and_then(|o| o[0].int())
        .is_some_and(|v| v & 0xFF == 0)
    }

    pub fn battery_limit_supported(&mut self) -> bool {
        self.wmi.as_mut().is_some_and(|w| w.has_instance(BATTERY))
    }

    pub fn set_battery_limit(&mut self, on: bool) -> bool {
        let out = self.call(
            BATTERY,
            "SetBatteryHealthControl",
            &[
                ("uBatteryNo", Arg::U8(1)),
                ("uFunctionMask", Arg::U8(1)),
                ("uFunctionStatus", Arg::U8(on as u8)),
                ("uReservedIn", Arg::Bytes(vec![0; 5])),
            ],
            &["uReturn"],
        );
        out.and_then(|o| o[0].int()).is_some_and(|v| v == 0)
    }

    /// Windows' own power slider follows the Acer profile (efficiency / balanced / performance overlay).
    pub fn sync_windows_overlay(mode: u8) {
        use windows::core::GUID;
        let guid = match mode {
            power::QUIET | power::ECO => GUID::from_u128(0x961cc777_2547_4f9d_8174_7d86181b8a7a),
            power::PERFORMANCE | power::TURBO => {
                GUID::from_u128(0xded574b5_45a0_4f42_8737_46345c09c238)
            }
            _ => GUID::zeroed(),
        };
        // Undocumented and missing from the import library: looked up at run time.
        use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
        use windows::core::{s, w};
        type SetOverlay = unsafe extern "system" fn(*const GUID) -> u32;
        unsafe {
            let Ok(lib) = LoadLibraryW(w!("powrprof.dll")) else {
                return;
            };
            if let Some(f) = GetProcAddress(lib, s!("PowerSetActiveOverlayScheme")) {
                let f: SetOverlay = std::mem::transmute(f);
                f(&guid);
            }
        }
    }
}
