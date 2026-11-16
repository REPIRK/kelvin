//! The hardware thread. Owns the WMI connection and the lighting controller; every 100 ms it handles
//! commands and the lighting (idle fade must react at once), every 2 s it reads the sensors and steers the
//! fans. What runs is the user's settings overlaid by the night schedule and then the active game; only
//! what differs from what was last applied is sent.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use crate::acer::{Acer, FanHw, power};
use crate::ene::{Ene, Report};
use crate::fans::Fans;
use crate::lighting::{self, Moment};
use crate::settings::{Effect, FanKind, GameProfile, Settings, curve_at};
use crate::state::{self, Cmd, HISTORY_LEN, Sample};
use crate::sys;

const TELEMETRY_EVERY: Duration = Duration::from_secs(2);
const GAMES_EVERY: Duration = Duration::from_secs(3);
const BATTERY_LIMIT_EVERY: Duration = Duration::from_secs(300);

/// What was last sent to the hardware; None means "unknown, send".
#[derive(Default)]
struct Applied {
    power: Option<u8>,
    fan_hw: Option<FanHw>,
    duty: Option<[u8; 2]>,
    refresh_max: Option<bool>,
    battery_limit: Option<bool>,
    battery_limit_at: Option<Instant>,
    usb_charging: Option<u8>,
    boot_effect: Option<bool>,
    overdrive: Option<bool>,
    turbo: Option<bool>,
    lights: [Option<Vec<Report>>; 3],
}

struct Effective {
    power: u8,
    fan: FanKind,
    refresh_max: bool,
    lights_off: bool,
    effect: Effect,
    color: Option<[u8; 3]>,
}

struct Core {
    acer: Acer,
    ene: Option<Ene>,
    ene_retry: Instant,
    s: Settings,
    applied: Applied,
    fans: Fans,
    on_ac: Option<bool>,
    ac_pending: (Option<bool>, u32),
    temps: [u32; 2],
    rpm: [u32; 2],
    smoothed: f32,
    last_tele: Option<Instant>,
    last_games: Option<Instant>,
    last_wall: SystemTime,
    game: Option<GameProfile>,
    level: f32,
    save_due: Option<Instant>,
    fans_save_at: Instant,
    sensor_fail: u32,
    fan_test: Option<FanTest>,
    light_fails: [u32; 3],
    dead_lights: [bool; 3],
    /// The fan mode that ran last tick (after night and game), to reset Auto+ when it changes.
    last_fan: Option<FanKind>,
    gpu_users: Option<crate::gpu::GpuUsers>,
    gpu_users_tried: bool,
    last_gpu_poll: Option<Instant>,
    /// Screen-off request time; the display goes off a moment later so releasing the hotkey does not wake it.
    screen_off: Option<Instant>,
    /// Input tick when the display went off; any newer input means the user is back.
    screen_off_input: Option<u32>,
}

/// Custom duty 20% then 100%, 12 s each, to see whether the fans follow duty writes at all.
struct FanTest {
    phase: u8,
    ticks: u32,
    low: [u32; 2],
}

pub fn run(rx: Receiver<Cmd>) {
    crate::wmi::init_com();
    let mut c = Core::new();
    c.probe();
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(cmd) => {
                if c.handle(cmd) {
                    return;
                }
                while let Ok(cmd) = rx.try_recv() {
                    if c.handle(cmd) {
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        c.tick();
    }
}

impl Core {
    fn new() -> Self {
        let s = state::shared().settings.clone();
        Self {
            acer: Acer::new(),
            ene: Ene::open(),
            ene_retry: Instant::now(),
            s,
            applied: Applied::default(),
            fans: Fans::load(),
            on_ac: None,
            ac_pending: (None, 0),
            temps: [0; 2],
            rpm: [0; 2],
            smoothed: 0.0,
            last_tele: None,
            last_games: None,
            last_wall: SystemTime::now(),
            game: None,
            level: 1.0,
            save_due: None,
            fans_save_at: Instant::now(),
            sensor_fail: 0,
            fan_test: None,
            light_fails: [0; 3],
            dead_lights: [false; 3],
            last_fan: None,
            gpu_users: None,
            gpu_users_tried: false,
            last_gpu_poll: None,
            screen_off: None,
            screen_off_input: None,
        }
    }

    /// Reads what the machine has and what the firmware currently holds.
    fn probe(&mut self) {
        let acer = self.acer.available();
        // A dark screen left behind by a crash or kill: bring the brightness and the taskbar back.
        if let Some(b) = crate::dark::take_saved_brightness() {
            self.acer.set_brightness(b);
        }
        if crate::dark::taskbar_left_hidden() {
            crate::dark::taskbars(true);
        }
        let boot = if acer {
            self.acer.get_boot_effect()
        } else {
            None
        };
        let usb = if acer {
            self.acer.get_usb_charging()
        } else {
            None
        };
        let battery = acer && self.acer.battery_limit_supported();
        let overdrive = if acer {
            self.acer.get_overdrive()
        } else {
            None
        };
        let gpu_mode = if acer { self.acer.get_gpu_mode() } else { None };
        if acer {
            // Start from the firmware's profile so the first apply does not fight a mode chosen elsewhere.
            if let Some(p) = self.acer.get_power()
                && power::ALL.contains(&p)
                && p != self.s.power
            {
                state::log(format!("Firmware profile at start: {}", power::name(p)));
            }
        }
        let startup = sync_startup(self.s.start_with_windows);
        let rivals = sys::rivals();
        let mut sh = state::shared();
        sh.caps.rivals = rivals;
        sh.caps.acer = acer;
        sh.caps.hid_lighting = self.ene.is_some();
        sh.caps.boot_effect = boot;
        sh.caps.usb_charging = usb;
        sh.caps.battery_limit = battery;
        sh.caps.overdrive = overdrive;
        sh.caps.gpu_mode = gpu_mode;
        sh.caps.gpu_modes = crate::gpu::mode_capability();
        let max_hz = sys::max_hz();
        sh.caps.max_hz = max_hz;
        sh.caps.startup = Some(startup);
        // The refresh rate is changed only when asked (or by a game), never on start: take what runs now.
        let at_max = sys::current_hz() >= max_hz;
        sh.settings.refresh_max = at_max;
        self.s.refresh_max = at_max;
        self.applied.refresh_max = Some(at_max);
        sh.caps.error = if acer {
            None
        } else {
            self.acer.last_error.clone()
        };
        // Firmware values the user never chose stay as they are.
        if sh.settings.boot_effect.is_none() {
            self.applied.boot_effect = boot;
        }
        if sh.settings.overdrive.is_none() {
            self.applied.overdrive = overdrive;
        }
        if sh.settings.usb_charging.is_none() {
            self.applied.usb_charging = usb;
        }
    }

    /// Returns true to stop the thread.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::SettingsChanged => {
                let new = state::shared().settings.clone();
                if new.start_with_windows != self.s.start_with_windows {
                    let ours = sync_startup(new.start_with_windows);
                    state::shared().caps.startup = Some(ours);
                }
                self.s = new;
                self.save_due = Some(Instant::now() + Duration::from_secs(1));
            }
            Cmd::FirmwareEvent(bytes) => self.firmware_event(&bytes),
            Cmd::CyclePower => self.cycle_power(),
            Cmd::ScreenOff => {
                // Already dark: a second request must not save brightness 0 as the one to restore.
                if self.screen_off.is_none() {
                    self.screen_off = Some(Instant::now());
                    self.screen_off_input = None;
                }
            }
            Cmd::SetGpuMode(m) => {
                let ok = self.acer.set_gpu_mode(m);
                crate::diag::log(format!(
                    "GPU mode {m} for the next start: {}",
                    if ok { "set" } else { "refused" }
                ));
                let mut sh = state::shared();
                if ok {
                    sh.caps.gpu_mode_next = Some(m);
                }
            }
            Cmd::PreferIntegrated(path) => {
                let ok = crate::gpu::prefer_integrated(&path);
                state::log(format!(
                    "{}: {}",
                    path.rsplit('\\').next().unwrap_or(&path),
                    if ok {
                        "integrated GPU from its next start"
                    } else {
                        "could not set the GPU preference"
                    }
                ));
            }
            Cmd::FanTest => {
                if self.fan_test.is_none() {
                    self.fan_test = Some(FanTest {
                        phase: 0,
                        ticks: 0,
                        low: [0; 2],
                    });
                    state::shared().caps.fan_test = Some("Testing: 20%…".into());
                }
            }
            Cmd::CloseRivals => {
                let rivals = std::mem::take(&mut state::shared().caps.rivals);
                for (pid, path) in rivals {
                    let ok = sys::terminate(pid);
                    state::log(format!(
                        "{} {path}",
                        if ok { "Closed" } else { "Could not close" }
                    ));
                }
                // Whatever they left the hardware in, send everything again.
                self.applied = Applied::default();
                self.fans.reset_duty();
                state::shared().caps.rivals = sys::rivals();
            }
            Cmd::Shutdown(ack) => {
                self.shutdown();
                let _ = ack.send(());
                return true;
            }
        }
        false
    }

    fn shutdown(&mut self) {
        // Auto+ and the curve are steered by this app; left in custom mode the fans would hold one speed.
        let eff = self.effective();
        if matches!(eff.fan, FanKind::AutoPlus | FanKind::Curve) || self.fans.guard {
            self.acer.set_fan_behavior(FanHw::Auto);
        }
        if self.screen_off.is_some() {
            self.end_dark();
        }
        // Turbo back to the scheme's own value: Windows keeps the power scheme after this app is gone.
        crate::turbo::set_allowed(true);
        self.s.save();
        self.fans.save();
    }

    fn firmware_event(&mut self, b: &[u8]) {
        let hex: Vec<String> = b.iter().take(8).map(|x| format!("{x:02X}")).collect();
        state::log(format!("Firmware event {}", hex.join(" ")));
        // Linux acer-wmi: function 0x07 is the gaming key event; key 0x04/0x05 the mode / PredatorSense key.
        if b.len() >= 2 && b[0] == 0x07 && (b[1] == 0x04 || b[1] == 0x05) && self.s.key_cycles_power
        {
            self.cycle_power();
        }
    }

    fn cycle_power(&mut self) {
        {
            let on_ac = self.on_ac.unwrap_or(true);
            let order: &[u8] = if on_ac {
                &[
                    power::QUIET,
                    power::BALANCED,
                    power::PERFORMANCE,
                    power::TURBO,
                ]
            } else {
                &[power::ECO, power::QUIET, power::BALANCED]
            };
            let cur = self.effective().power;
            let next = order
                .iter()
                .position(|&p| p == cur)
                .map(|i| order[(i + 1) % order.len()])
                .unwrap_or(order[0]);
            state::log(format!("Power mode: {}", power::name(next)));
            state::update(|s| s.power = next);
        }
    }

    fn effective(&self) -> Effective {
        let s = &self.s;
        let mut e = Effective {
            power: s.power,
            fan: s.fan,
            refresh_max: s.refresh_max,
            lights_off: !s.lighting.enabled || self.screen_off.is_some(),
            effect: s.lighting.effect,
            color: None,
        };
        if s.night.active_at(sys::local_minute()) {
            if let Some(p) = s.night.power {
                e.power = p;
            }
            if s.night.fans_auto {
                e.fan = FanKind::Auto;
            }
            e.lights_off |= s.night.lights_off;
        }
        if let Some(g) = &self.game {
            if let Some(p) = g.power {
                e.power = p;
            }
            if let Some(f) = g.fan {
                e.fan = f;
            }
            if let Some(r) = g.refresh_max {
                e.refresh_max = r;
            }
            if let Some(fx) = g.effect {
                e.effect = fx;
            }
            e.color = g.color;
        }
        let on_ac = self.on_ac.unwrap_or(true);
        if !power::allowed(e.power, on_ac) {
            e.power = power::BALANCED;
        }
        e
    }

    fn tick(&mut self) {
        // A jump in wall time means the machine slept: reconnect and resend everything.
        let now_wall = SystemTime::now();
        if now_wall
            .duration_since(self.last_wall)
            .map(|d| d > Duration::from_secs(15))
            .unwrap_or(false)
        {
            state::log("Resumed from sleep: reapplying");
            self.acer.reconnect();
            self.ene = Ene::open();
            self.applied = Applied::default();
            self.fans.reset_duty();
            self.last_tele = None;
            self.light_fails = [0; 3];
            self.dead_lights = [false; 3];
        }
        self.last_wall = now_wall;

        if self.ene.is_none() && self.ene_retry.elapsed() > Duration::from_secs(30) {
            self.ene_retry = Instant::now();
            self.ene = Ene::open();
            if self.ene.is_some() {
                state::shared().caps.hid_lighting = true;
            }
        }

        let now = Instant::now();
        if self.last_games.is_none_or(|t| now - t >= GAMES_EVERY) {
            self.last_games = Some(now);
            self.poll_games();
        }
        if self.last_tele.is_none_or(|t| now - t >= TELEMETRY_EVERY) {
            self.last_tele = Some(now);
            self.telemetry();
            self.apply_slow();
            let visible = state::shared().window_visible;
            self.poll_gpu_users(visible);
        }
        self.screen_off_step();
        self.apply_lighting();

        if self.save_due.is_some_and(|t| now >= t) {
            self.save_due = None;
            self.s.save();
        }
        if self.fans_save_at.elapsed() > Duration::from_secs(120) {
            self.fans_save_at = Instant::now();
            self.fans.save();
        }
    }

    fn poll_games(&mut self) {
        if !self.s.games_enabled || self.s.games.is_empty() {
            if self.game.take().is_some() {
                state::log("Game Sync: off");
            }
            return;
        }
        let running = sys::process_names();
        let found = self
            .s
            .games
            .iter()
            .find(|g| running.contains(&g.exe.to_lowercase()))
            .cloned();
        if found.as_ref().map(|g| &g.exe) != self.game.as_ref().map(|g| &g.exe) {
            match &found {
                Some(g) => state::log(format!("Game Sync: {}", g.exe)),
                None => state::log("Game Sync: game closed, back to your settings"),
            }
            self.fans.reset_duty();
        }
        self.game = found;
    }

    fn telemetry(&mut self) {
        // Debounced AC state; a confirmed change applies the AC / battery rules.
        if let Some(ac) = sys::on_ac() {
            if self.ac_pending.0 != Some(ac) {
                self.ac_pending = (Some(ac), 1);
            } else {
                self.ac_pending.1 += 1;
            }
            if self.ac_pending.1 >= 2 && self.on_ac != Some(ac) {
                self.on_ac = Some(ac);
                let (p, f) = if ac {
                    (self.s.power_on_ac, self.s.fan_on_ac)
                } else {
                    (self.s.power_on_battery, self.s.fan_on_battery)
                };
                if p.is_some() || f.is_some() {
                    state::update(|s| {
                        if let Some(p) = p {
                            s.power = p;
                        }
                        if let Some(f) = f {
                            s.fan = f;
                        }
                    });
                }
            }
        }

        self.temps = [self.acer.cpu_temp(), self.acer.gpu_temp()];
        self.rpm = [self.acer.cpu_fan_rpm(), self.acer.gpu_fan_rpm()];
        let hot = self.temps[0].max(self.temps[1]);
        let eff = self.effective();
        self.smoothed = self.fans.observe(hot, eff.fan.is_steered());
        self.sensor_fail = if hot == 0 { self.sensor_fail + 1 } else { 0 };

        let rivals = sys::rivals();
        let mut sh = state::shared();
        sh.caps.rivals = rivals;
        if sh.history.len() >= HISTORY_LEN {
            sh.history.pop_front();
        }
        sh.history.push_back(Sample {
            cpu_temp: self.temps[0] as u16,
            gpu_temp: self.temps[1] as u16,
            cpu_rpm: self.rpm[0] as u16,
            gpu_rpm: self.rpm[1] as u16,
        });
        let t = &mut sh.tele;
        t.cpu_temp = self.temps[0];
        t.gpu_temp = self.temps[1];
        t.cpu_rpm = self.rpm[0];
        t.gpu_rpm = self.rpm[1];
        t.on_ac = self.on_ac.unwrap_or(true);
        t.power = eff.power;
        t.fan_text = match eff.fan {
            FanKind::AutoPlus => format!("Auto +{}%", self.s.boost),
            k => k.name().to_string(),
        };
        t.turbo_off = !self.turbo_allowed(eff.power);
        t.notes.clear();
        if eff.fan == FanKind::AutoPlus && self.fans.ignored {
            t.notes
                .push("The fans ignore set speeds here: Auto+ runs as Auto".into());
        }
        if self.fans.guard {
            t.notes.push("Over 95 °C: fans at Max until 85 °C".into());
        }
        if let Some(g) = &self.game {
            t.notes.push(format!("Game Sync: {}", g.exe));
        }
        if self.s.night.active_at(sys::local_minute()) {
            t.notes.push("Night schedule".into());
        }
        if eff.fan == FanKind::AutoPlus && !self.fans.knows(eff.power, self.smoothed) {
            t.notes
                .push("Auto+ is still learning this temperature: curve + boost for now".into());
        }
        drop(sh);
        state::repaint();
    }

    /// Power, fans, refresh rate, battery and firmware switches.
    fn apply_slow(&mut self) {
        let eff = self.effective();

        if self.applied.power != Some(eff.power) && self.acer.set_power(eff.power) {
            Acer::sync_windows_overlay(eff.power);
            self.applied.power = Some(eff.power);
        }

        if self.last_fan != Some(eff.fan) {
            if eff.fan == FanKind::Auto {
                self.fans.enter_auto();
            }
            self.fans.reset_duty();
            self.last_fan = Some(eff.fan);
        }

        if self.fan_test_step() {
            return;
        }

        // Fans. A failed sensor in a steered mode hands the fans back to the EC, and so does Auto+ when the
        // fans turned out not to follow duty writes.
        let sensor_lost = eff.fan.is_steered() && self.sensor_fail >= 3;
        let not_obeyed = eff.fan == FanKind::AutoPlus && self.fans.ignored;
        let hw = if self.fans.guard {
            FanHw::Max
        } else if sensor_lost || not_obeyed {
            FanHw::Auto
        } else {
            match eff.fan {
                FanKind::Auto => FanHw::Auto,
                FanKind::Max => FanHw::Max,
                _ => FanHw::Custom,
            }
        };
        if self.applied.fan_hw != Some(hw) && self.acer.set_fan_behavior(hw) {
            crate::diag::log(format!(
                "Fans: {:?} for {}{}",
                hw,
                eff.fan.name(),
                if self.fans.guard { " (heat guard)" } else { "" }
            ));
            self.applied.fan_hw = Some(hw);
            self.applied.duty = None;
        }
        if hw == FanHw::Auto && eff.fan == FanKind::Auto {
            self.fans
                .learn(eff.power, self.temps[0].max(self.temps[1]), self.rpm);
        }
        if hw == FanHw::Custom {
            let t = self.smoothed;
            let fallback = [
                curve_at(&self.s.curve_cpu, t),
                curve_at(&self.s.curve_gpu, t),
            ];
            let duty = match eff.fan {
                FanKind::Fixed => [self.s.fixed_cpu, self.s.fixed_gpu],
                FanKind::Curve => fallback,
                _ => self
                    .fans
                    .boost_step(eff.power, t, self.rpm, self.s.boost, fallback),
            };
            if self.applied.duty != Some(duty) && self.acer.set_fan_speed(duty[0], duty[1]) {
                let jump = self
                    .applied
                    .duty
                    .is_none_or(|d| d[0].abs_diff(duty[0]) >= 5 || d[1].abs_diff(duty[1]) >= 5);
                if jump {
                    crate::diag::log(format!(
                        "Duty {}/{}% at {}/{} rpm, {:.0}°C",
                        duty[0], duty[1], self.rpm[0], self.rpm[1], t
                    ));
                }
                self.applied.duty = Some(duty);
            }
        }

        if self.applied.refresh_max != Some(eff.refresh_max) {
            let hz = if eff.refresh_max { sys::max_hz() } else { 60 };
            sys::set_hz(hz);
            self.applied.refresh_max = Some(eff.refresh_max);
        }

        // The limit is resent now and then: firmware updates and battery swaps have been seen to clear it.
        let due = self
            .applied
            .battery_limit_at
            .is_none_or(|t| t.elapsed() > BATTERY_LIMIT_EVERY);
        // Off is written only once the user has turned it off here, never at start: a limit set in
        // PredatorSense would otherwise be cleared.
        if self.applied.battery_limit.is_none() && !self.s.battery_limit {
            self.applied.battery_limit = Some(false);
        }
        if self.applied.battery_limit != Some(self.s.battery_limit) || (self.s.battery_limit && due)
        {
            if self.acer.set_battery_limit(self.s.battery_limit) {
                self.applied.battery_limit = Some(self.s.battery_limit);
            }
            self.applied.battery_limit_at = Some(Instant::now());
        }

        if let Some(level) = self.s.usb_charging
            && self.applied.usb_charging != Some(level)
            && self.acer.set_usb_charging(level)
        {
            self.applied.usb_charging = Some(level);
            state::shared().caps.usb_charging = Some(level);
        }
        if let Some(on) = self.s.boot_effect
            && self.applied.boot_effect != Some(on)
            && self.acer.set_boot_effect(on)
        {
            self.applied.boot_effect = Some(on);
            state::shared().caps.boot_effect = Some(on);
        }
        if let Some(on) = self.s.overdrive
            && self.applied.overdrive != Some(on)
            && self.acer.set_overdrive(on)
        {
            self.applied.overdrive = Some(on);
            state::shared().caps.overdrive = Some(on);
        }

        let turbo = self.turbo_allowed(eff.power);
        if self.applied.turbo != Some(turbo) {
            if crate::turbo::set_allowed(turbo) {
                crate::diag::log(format!(
                    "CPU turbo {}",
                    if turbo { "allowed" } else { "off" }
                ));
            }
            self.applied.turbo = Some(turbo);
        }
    }

    /// Darkens the screen 0.7 s after the request (so releasing the hotkey does not end it): brightness 0
    /// under a black overlay, display and machine kept on. Not a display power-off: see dark.rs.
    /// Ends at the next keyboard or mouse input.
    fn screen_off_step(&mut self) {
        let Some(since) = self.screen_off else { return };
        match self.screen_off_input {
            None if since.elapsed() >= Duration::from_millis(700) => {
                // Brightness is touched only when it reads as a real value: a panel that reports 0 would
                // otherwise be "restored" to black. Without it the overlay alone darkens the screen.
                if let Some(b) = self.acer.get_brightness().filter(|&b| b > 0) {
                    crate::dark::save_brightness(b);
                    self.acer.set_brightness(0);
                }
                crate::dark::show_overlay();
                crate::dark::taskbars(false);
                sys::keep_awake(true);
                self.screen_off_input = Some(sys::last_input_tick());
                state::shared().tele.screen_off = true;
                crate::diag::log("Screen dark");
            }
            Some(at) if sys::last_input_tick() != at => self.end_dark(),
            _ => {}
        }
    }

    fn end_dark(&mut self) {
        crate::dark::hide_overlay();
        crate::dark::taskbars(true);
        if let Some(b) = crate::dark::take_saved_brightness() {
            self.acer.set_brightness(b);
        }
        sys::keep_awake(false);
        self.screen_off = None;
        self.screen_off_input = None;
        state::shared().tele.screen_off = false;
    }

    fn turbo_allowed(&self, power: u8) -> bool {
        let quiet = matches!(power, power::QUIET | power::ECO) && self.s.turbo_off_quiet;
        let battery = self.on_ac == Some(false) && self.s.turbo_off_battery;
        !(quiet || battery)
    }

    /// Processes holding the NVIDIA GPU, every 10 s while on battery or while the window is open.
    fn poll_gpu_users(&mut self, window_visible: bool) {
        let on_battery = self.on_ac == Some(false);
        if !on_battery && !window_visible {
            return;
        }
        if self
            .last_gpu_poll
            .is_some_and(|t| t.elapsed() < Duration::from_secs(10))
        {
            return;
        }
        self.last_gpu_poll = Some(Instant::now());
        if self.gpu_users.is_none() && !self.gpu_users_tried {
            self.gpu_users_tried = true;
            self.gpu_users = crate::gpu::GpuUsers::new();
        }
        let Some(users) = self.gpu_users.as_mut() else {
            return;
        };
        // The compositor and NVIDIA's own services hold a context without keeping the GPU busy.
        const QUIET: [&str; 6] = [
            "dwm.exe",
            "csrss.exe",
            "nvcontainer.exe",
            "nvdisplay.container.exe",
            "system",
            "kelvin.exe",
        ];
        let apps: Vec<(u32, String, String, f64)> = users
            .poll()
            .into_iter()
            .filter_map(|(pid, use_pct)| {
                let path = sys::process_path(pid)?;
                let name = path.rsplit('\\').next().unwrap_or(&path).to_string();
                (!QUIET.contains(&name.to_lowercase().as_str()))
                    .then_some((pid, name, path, use_pct))
            })
            .collect();
        state::shared().tele.rtx_apps = apps;
    }

    /// One 2 s step of the fan test. Returns true while the test owns the fans.
    fn fan_test_step(&mut self) -> bool {
        let Some(t) = self.fan_test.as_mut() else {
            return false;
        };
        t.ticks += 1;
        let rpm = self.rpm;
        match t.phase {
            0 => {
                self.acer.set_fan_behavior(FanHw::Custom);
                self.acer.set_fan_speed(20, 20);
                t.phase = 1;
                t.ticks = 0;
            }
            1 if t.ticks >= 6 => {
                t.low = rpm;
                self.acer.set_fan_speed(100, 100);
                t.phase = 2;
                t.ticks = 0;
                state::shared().caps.fan_test = Some(format!(
                    "Testing: at 20% {} / {} rpm, now 100%…",
                    rpm[0], rpm[1]
                ));
            }
            2 if t.ticks >= 6 => {
                let low = t.low;
                let follows =
                    (0..2).all(|f| rpm[f] > low[f] + 800 && rpm[f] as f32 > low[f] as f32 * 1.3);
                let text = format!(
                    "CPU {} → {} rpm, GPU {} → {} rpm: {}",
                    low[0],
                    rpm[0],
                    low[1],
                    rpm[1],
                    if follows {
                        "the fans follow set speeds."
                    } else {
                        "the fans ignore set speeds here. Auto+ falls back to Auto; Max still works."
                    }
                );
                crate::diag::log(format!("Fan test: {text}"));
                state::shared().caps.fan_test = Some(text);
                self.fans.ignored = !follows;
                self.fans.reset_duty();
                self.fan_test = None;
                self.applied.fan_hw = None;
                self.applied.duty = None;
                return false;
            }
            _ => {}
        }
        true
    }

    fn apply_lighting(&mut self) {
        let Some(ene) = &self.ene else { return };
        let eff = self.effective();
        let l = &self.s.lighting;

        // No idle fade while a Game Sync game runs: a gamepad does not count as input for Windows.
        let idle =
            l.idle_off && self.game.is_none() && sys::idle_seconds() >= l.idle_minutes as u32 * 60;
        let target = if idle || eff.lights_off { 0.0 } else { 1.0 };
        // Fade out over about a second; back on at once.
        self.level = if target > self.level {
            target
        } else {
            (self.level - 0.1).max(target)
        };

        let moment = Moment {
            effect: eff.effect,
            color: eff.color,
            level: self.level,
            temp: self.temps[0].max(self.temps[1]),
            power: eff.power,
        };
        let wanted = lighting::reports(l, &moment);
        let mut failed = [false; 3];
        for (i, list) in wanted.into_iter().enumerate() {
            if list.is_empty()
                || self.dead_lights[i]
                || self.applied.lights[i].as_ref() == Some(&list)
            {
                continue;
            }
            if list.iter().all(|r| ene.send(r)) {
                self.applied.lights[i] = Some(list);
                self.light_fails[i] = 0;
            } else {
                failed[i] = true;
            }
        }
        // The handle goes stale across sleep or a driver restart: reopen, at most every 5 s. A device that
        // still refuses after three reopens is a part this model lacks: stop writing to it.
        if failed.contains(&true) && self.ene_retry.elapsed() > Duration::from_secs(5) {
            for (i, &f) in failed.iter().enumerate() {
                if f {
                    self.light_fails[i] += 1;
                    if self.light_fails[i] >= 3 {
                        self.dead_lights[i] = true;
                        crate::diag::log(format!("Lighting device {i} refuses writes; left alone"));
                    }
                }
            }
            self.ene_retry = Instant::now();
            self.ene = Ene::open();
            self.applied.lights = Default::default();
        }
    }
}

/// Keeps the logon task in line with the setting: created or repointed at this exe when on (the exe may
/// have moved, or the task may still start the C# app this replaces), removed when off. Returns whether
/// the task now starts this exe.
fn sync_startup(want: bool) -> bool {
    if sys::remove_legacy_task() {
        state::log("Removed the old PredatorControl logon task");
    }
    let (exists, ours) = sys::startup_state();
    if want && !ours {
        if sys::set_startup(true) {
            state::log("Start with Windows: logon task set to this exe");
        } else {
            state::log("Could not create the logon task");
        }
    } else if !want && exists && ours {
        sys::set_startup(false);
    }
    sys::startup_state().1
}
