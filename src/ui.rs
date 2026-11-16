//! The window. Reads a copy of the settings each frame and hands changes to `state::update`; never touches
//! hardware itself.
//!
//! Layout: three tabs. Main holds what is changed every day (power, fans, light on/off) and fits without
//! scrolling; Light the lighting detail; Settings everything set once.
//!
//! Colour rule: the chrome is black, grey and white. Hue appears only where it means something:
//! temperature (teal → red) and the user's own lighting colours. One emphasis mechanism: the chosen option
//! is a light plate with dark text; everything else is grey text without a frame.

use std::collections::VecDeque;
use std::sync::Arc;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Mesh, Pos2,
    Rect, RichText, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::acer::{USB_CHARGE_LEVELS, power};
use crate::lighting::temp_color;
use crate::settings::{Effect, FanKind, GameProfile, Rgb, Settings, default_curve};
use crate::state::{self, Caps, Cmd, Sample, Telemetry};
use crate::sys;
use crate::window;

/// Semantic colours. Contrast on `WINDOW`: TEXT 14:1, TEXT_DIM 5.3:1, TEXT_FAINT 2.4:1 (disabled only),
/// CHOSEN plate 14.5:1.
const WINDOW: Color32 = Color32::from_rgb(20, 20, 23);
const TEXT: Color32 = Color32::from_rgb(230, 230, 234);
const TEXT_DIM: Color32 = Color32::from_rgb(140, 140, 150);
const TEXT_FAINT: Color32 = Color32::from_rgb(85, 85, 95);
const CHOSEN: Color32 = Color32::from_rgb(232, 232, 236);
const ON_CHOSEN: Color32 = Color32::from_rgb(17, 17, 20);
const HOVER: Color32 = Color32::from_rgb(32, 32, 37);
const RULE: Color32 = Color32::from_rgb(42, 42, 48);
const TRACK: Color32 = Color32::from_rgb(52, 52, 60);
const PLOT_BG: Color32 = Color32::from_rgb(26, 26, 30);
/// Second series in plots (GPU): grey against the white first series.
const SERIES_2: Color32 = Color32::from_rgb(130, 130, 140);

fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

pub fn style(ctx: &egui::Context) {
    // Segoe UI, the system face, so the window reads as a Windows program.
    let mut fonts = FontDefinitions::default();
    let dir = std::path::Path::new("C:\\Windows\\Fonts");
    if let Ok(regular) = std::fs::read(dir.join("segoeui.ttf")) {
        fonts
            .font_data
            .insert("segoe".into(), Arc::new(FontData::from_owned(regular)));
        if let Some(f) = fonts.families.get_mut(&FontFamily::Proportional) {
            f.insert(0, "segoe".into());
        }
        let bold = std::fs::read(dir.join("seguisb.ttf")).ok();
        let mut fam = Vec::new();
        if let Some(b) = bold {
            fonts
                .font_data
                .insert("segoe_sb".into(), Arc::new(FontData::from_owned(b)));
            fam.push("segoe_sb".to_string());
        }
        fam.push("segoe".into());
        fonts.families.insert(semibold(), fam);
    } else {
        let p = fonts.families[&FontFamily::Proportional].clone();
        fonts.families.insert(semibold(), p);
    }
    ctx.set_fonts(fonts);

    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        let v = &mut s.visuals;
        v.panel_fill = WINDOW;
        v.window_fill = Color32::from_rgb(28, 28, 32);
        v.faint_bg_color = WINDOW;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = Color32::from_rgb(170, 170, 178);
        v.selection.stroke = Stroke::new(1.0, CHOSEN);
        v.hyperlink_color = TEXT;
        let r = CornerRadius::same(4);
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = r;
            w.bg_stroke = Stroke::NONE;
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, RULE);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(34, 34, 40);
        v.widgets.inactive.bg_fill = TRACK;
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(44, 44, 51);
        v.widgets.hovered.bg_fill = Color32::from_rgb(70, 70, 78);
        v.widgets.active.weak_bg_fill = Color32::from_rgb(52, 52, 60);
        v.widgets.active.bg_fill = Color32::from_rgb(90, 90, 98);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(14.0));
        s.text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(14.0));
        s.text_styles
            .insert(egui::TextStyle::Small, FontId::proportional(12.0));
    });
}

#[derive(Default, PartialEq, Clone, Copy)]
enum Tab {
    #[default]
    Main,
    Light,
    Settings,
}

#[derive(Default)]
pub struct App {
    dressed: bool,
    tab: Tab,
    show_history: bool,
    /// Swatch being edited: 0..=3 keyboard zones, 4 the lid logo.
    picking: Option<usize>,
    hsv: [f32; 3],
    hex: String,
    curve_gpu: bool,
    dragging: Option<usize>,
    candidates: Vec<String>,
    candidate: String,
    open_game: Option<usize>,
}

impl App {
    #[allow(dead_code)]
    /// For `examples/shots.rs`: a window on tab 0 (Main, with history open), 1 (Light) or 2 (Settings).
    pub fn set_tab(&mut self, tab: usize) {
        self.tab = [Tab::Main, Tab::Light, Tab::Settings][tab.min(2)];
        self.show_history = tab == 0;
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|i| i.viewport().close_requested()) {
            // The close button sends the app to the tray; Exit lives in the tray menu.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            window::hide();
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if !self.dressed {
            self.dressed = true;
            window::dress();
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) && !ui.ctx().egui_wants_keyboard_input() {
            window::hide();
        }
        let (mut s, tele, caps, history, log) = {
            let sh = state::shared();
            (
                sh.settings.clone(),
                sh.tele.clone(),
                sh.caps.clone(),
                sh.history.clone(),
                sh.log.clone(),
            )
        };
        let before = s.clone();

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(WINDOW))
            .show(ui, |ui| {
                egui::Frame::NONE
                    .inner_margin(egui::Margin {
                        left: 20,
                        right: 20,
                        top: 14,
                        bottom: 0,
                    })
                    .show(ui, |ui| {
                        let tabs = [Tab::Main, Tab::Light, Tab::Settings];
                        let at = tabs.iter().position(|t| *t == self.tab);
                        if let Some(i) = choice(ui, &["Main", "Light", "Settings"], at, &[]) {
                            self.tab = tabs[i];
                        }
                    });
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        egui::Frame::NONE
                            .inner_margin(egui::Margin {
                                left: 20,
                                right: 20,
                                top: 8,
                                bottom: 20,
                            })
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                match self.tab {
                                    Tab::Main => self.main_tab(ui, &mut s, &tele, &caps, &history),
                                    Tab::Light => self.light_tab(ui, &mut s, &caps, &tele),
                                    Tab::Settings => {
                                        self.settings_tab(ui, &mut s, &caps, &tele, &log)
                                    }
                                }
                            });
                    });
            });

        if s != before {
            state::update(|x| *x = s);
        }
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs(2));
    }
}

// ---------------------------------------------------------------------------------------------------------
// Building blocks

/// A group heading: small grey sentence-case text with space above. Space marks where a group starts.
fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(14.0);
    ui.label(RichText::new(text).size(12.5).color(TEXT_DIM));
}

fn rule(ui: &mut Ui) {
    ui.add_space(12.0);
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(w, 1.0), Sense::hover());
    ui.painter().rect_filled(r, 0, RULE);
}

fn note(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).size(12.5).color(TEXT_DIM));
}

/// One choice out of a few, as equal-width plates. The chosen one is a light plate with dark text.
fn choice(
    ui: &mut Ui,
    labels: &[&str],
    chosen: Option<usize>,
    disabled: &[usize],
) -> Option<usize> {
    let gap = 4.0;
    let n = labels.len() as f32;
    let w = ((ui.available_width() - gap * (n - 1.0)) / n).floor();
    let mut clicked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (i, label) in labels.iter().enumerate() {
            let on = chosen == Some(i);
            let off = disabled.contains(&i);
            let sense = if off { Sense::hover() } else { Sense::click() };
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 30.0), sense);
            let p = ui.painter();
            if on {
                p.rect_filled(rect, 4, CHOSEN);
            } else if resp.hovered() && !off {
                p.rect_filled(rect, 4, HOVER);
            }
            let color = if on {
                ON_CHOSEN
            } else if off {
                TEXT_FAINT
            } else if resp.hovered() {
                TEXT
            } else {
                TEXT_DIM
            };
            let font = if on {
                FontId::new(13.5, semibold())
            } else {
                FontId::proportional(13.5)
            };
            p.text(rect.center(), Align2::CENTER_CENTER, *label, font, color);
            if resp.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
}

/// A labelled on/off switch, label left, switch right. On is the light plate, as for a chosen option.
fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> egui::Response {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (rect, mut resp) = ui.allocate_exact_size(vec2(36.0, 20.0), Sense::click());
            if resp.clicked() {
                *on = !*on;
                resp.mark_changed();
            }
            let t = ui.ctx().animate_bool_with_time(resp.id, *on, 0.08);
            let p = ui.painter();
            p.rect_filled(rect, 10, mix(TRACK, CHOSEN, t));
            let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
            p.circle_filled(pos2(x, rect.center().y), 7.0, mix(TEXT_DIM, ON_CHOSEN, t));
            resp
        })
        .inner
    })
    .inner
}

fn slider<N: egui::emath::Numeric>(
    ui: &mut Ui,
    v: &mut N,
    range: std::ops::RangeInclusive<N>,
    suffix: &str,
) {
    ui.spacing_mut().slider_width = ui.available_width() - 70.0;
    ui.add(
        egui::Slider::new(v, range)
            .suffix(suffix)
            .trailing_fill(true),
    );
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(f(a.r(), b.r()), f(a.g(), b.g()), f(a.b(), b.b()))
}

fn rgb(c: Rgb) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

fn heat(t: u32) -> Color32 {
    if t == 0 {
        TEXT_FAINT
    } else {
        rgb(temp_color(t))
    }
}

fn power_combo(ui: &mut Ui, id: &str, value: &mut Option<u8>, choices: &[u8]) {
    let text = value.map(power::name).unwrap_or("Don't change");
    egui::ComboBox::from_id_salt(id)
        .selected_text(text)
        .width(140.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "Don't change");
            for &p in choices {
                ui.selectable_value(value, Some(p), power::name(p));
            }
        });
}

fn fan_combo(ui: &mut Ui, id: &str, value: &mut Option<FanKind>) {
    let text = value.map(|k| k.name()).unwrap_or("Don't change");
    egui::ComboBox::from_id_salt(id)
        .selected_text(text)
        .width(140.0)
        .show_ui(ui, |ui| {
            ui.selectable_value(value, None, "Don't change");
            for k in [
                FanKind::Auto,
                FanKind::AutoPlus,
                FanKind::Max,
                FanKind::Fixed,
                FanKind::Curve,
            ] {
                ui.selectable_value(value, Some(k), k.name());
            }
        });
}

/// A label left, a widget right, on one line.
fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add);
    });
}

// ---------------------------------------------------------------------------------------------------------
// Main

impl App {
    fn main_tab(
        &mut self,
        ui: &mut Ui,
        s: &mut Settings,
        t: &Telemetry,
        caps: &Caps,
        history: &VecDeque<Sample>,
    ) {
        if !caps.rivals.is_empty() {
            ui.add_space(6.0);
            for (_, path) in &caps.rivals {
                ui.label(RichText::new(format!("Also running: {path}")).strong());
            }
            ui.horizontal(|ui| {
                note(ui, "It drives the same fans and lights.");
                if ui.button("Close it").clicked() {
                    state::send(Cmd::CloseRivals);
                }
            });
            rule(ui);
        }
        if !caps.acer {
            ui.add_space(6.0);
            ui.label(RichText::new("Acer firmware interface not found").strong());
            note(ui, caps.error.clone().unwrap_or_default());
        }

        // Temperatures: the one large readout. A click shows the last ten minutes.
        ui.add_space(8.0);
        let resp = ui
            .horizontal(|ui| {
                for (label, temp) in [("CPU", t.cpu_temp), ("GPU", t.gpu_temp)] {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(label).size(12.5).color(TEXT_DIM));
                        let text = if temp == 0 {
                            "--°".to_string()
                        } else {
                            format!("{temp}°")
                        };
                        ui.label(
                            RichText::new(text)
                                .font(FontId::new(34.0, semibold()))
                                .color(heat(temp)),
                        );
                    });
                    ui.add_space(28.0);
                }
            })
            .response
            .interact(Sense::click())
            .on_hover_text("Last 10 minutes");
        if resp.clicked() {
            self.show_history = !self.show_history;
        }
        note(
            ui,
            format!(
                "Fans {} · {} rpm   {}",
                t.cpu_rpm,
                t.gpu_rpm,
                if t.on_ac { "" } else { "On battery" }
            ),
        );
        for n in &t.notes {
            note(ui, n);
        }
        if !t.on_ac && !t.rtx_apps.is_empty() {
            ui.horizontal(|ui| {
                let names: Vec<&str> = t.rtx_apps.iter().map(|a| a.1.as_str()).collect();
                note(ui, format!("Keeping the RTX awake: {}", names.join(", ")));
                if ui.link("Fix").clicked() {
                    self.tab = Tab::Settings;
                }
            });
        }
        if self.show_history {
            ui.add_space(6.0);
            history_plot(ui, history);
        }

        rule(ui);
        heading(ui, "Power");
        let modes: Vec<u8> = power::ALL
            .iter()
            .copied()
            .filter(|&m| power::allowed(m, t.on_ac))
            .collect();
        let labels: Vec<&str> = modes.iter().map(|&m| power::short(m)).collect();
        if let Some(i) = choice(ui, &labels, modes.iter().position(|&m| m == s.power), &[]) {
            s.power = modes[i];
        }
        if t.power != s.power {
            note(
                ui,
                format!(
                    "Running {} (game, night or battery rule)",
                    power::name(t.power)
                ),
            );
        }

        heading(ui, "Fans");
        let custom = matches!(s.fan, FanKind::Fixed | FanKind::Curve);
        let kinds = ["Auto", "Auto+", "Max", "Custom"];
        let at = match s.fan {
            FanKind::Auto => 0,
            FanKind::AutoPlus => 1,
            FanKind::Max => 2,
            _ => 3,
        };
        if let Some(i) = choice(ui, &kinds, Some(at), &[]) {
            s.fan = match i {
                0 => FanKind::Auto,
                1 => FanKind::AutoPlus,
                2 => FanKind::Max,
                _ if custom => s.fan,
                _ => FanKind::Fixed,
            };
        }
        match s.fan {
            FanKind::AutoPlus => {
                ui.add_space(2.0);
                note(ui, "Faster than Auto at the same temperature");
                slider(ui, &mut s.boost, 5..=50, " %");
                fan_test(ui, caps);
            }
            FanKind::Fixed | FanKind::Curve => {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.set_max_width(200.0);
                    let at = (s.fan == FanKind::Curve) as usize;
                    if let Some(i) = choice(ui, &["Fixed", "Curve"], Some(at), &[]) {
                        s.fan = if i == 0 {
                            FanKind::Fixed
                        } else {
                            FanKind::Curve
                        };
                    }
                });
                if s.fan == FanKind::Fixed {
                    note(ui, "CPU fan");
                    slider(ui, &mut s.fixed_cpu, 0..=100, " %");
                    note(ui, "GPU fan");
                    slider(ui, &mut s.fixed_gpu, 0..=100, " %");
                } else {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(!self.curve_gpu, "CPU").clicked() {
                            self.curve_gpu = false;
                        }
                        if ui.selectable_label(self.curve_gpu, "GPU").clicked() {
                            self.curve_gpu = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Reset").clicked() {
                                if self.curve_gpu {
                                    s.curve_gpu = default_curve();
                                } else {
                                    s.curve_cpu = default_curve();
                                }
                            }
                        });
                    });
                    let (curve, temp) = if self.curve_gpu {
                        (&mut s.curve_gpu, t.gpu_temp)
                    } else {
                        (&mut s.curve_cpu, t.cpu_temp)
                    };
                    curve_editor(ui, curve, temp, &mut self.dragging);
                }
            }
            _ => {}
        }

        rule(ui);
        ui.add_space(10.0);
        if caps.hid_lighting {
            switch(ui, &mut s.lighting.enabled, "Light");
            if s.lighting.enabled {
                slider(ui, &mut s.lighting.brightness, 0..=100, " %");
                ui.horizontal(|ui| {
                    let l = &s.lighting;
                    if l.effect.uses_color() {
                        let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        ui.painter().rect_filled(r, 3, rgb(l.zones[0]));
                    }
                    note(ui, l.effect.name());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.link("Colour and effects").clicked() {
                            self.tab = Tab::Light;
                        }
                    });
                });
            }
        } else {
            note(ui, "Lighting controller not found on this machine.");
        }

        rule(ui);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Turn off the screen").clicked() {
                state::send(Cmd::ScreenOff);
            }
            note(
                ui,
                "Music, downloads and games keep running. Any key wakes it.",
            );
        });
    }

    // -----------------------------------------------------------------------------------------------------
    // Light

    fn light_tab(&mut self, ui: &mut Ui, s: &mut Settings, caps: &Caps, t: &Telemetry) {
        if !caps.hid_lighting {
            ui.add_space(10.0);
            note(
                ui,
                "Lighting controller (ENE KB5130) not found on this machine.",
            );
            return;
        }
        ui.add_space(10.0);
        switch(ui, &mut s.lighting.enabled, "Light");
        if !s.lighting.enabled {
            return;
        }
        heading(ui, "Brightness");
        slider(ui, &mut s.lighting.brightness, 0..=100, " %");

        heading(ui, "Effect");
        let effects = Effect::ALL;
        let at = effects.iter().position(|e| *e == s.lighting.effect);
        for (r, chunk) in effects.chunks(3).enumerate() {
            let labels: Vec<&str> = chunk.iter().map(|e| e.name()).collect();
            let chosen = at.and_then(|i| (i / 3 == r).then_some(i % 3));
            if let Some(i) = choice(ui, &labels, chosen, &[]) {
                s.lighting.effect = chunk[i];
            }
        }
        let fx = s.lighting.effect;
        if fx.uses_speed() {
            heading(ui, "Speed");
            slider(ui, &mut s.lighting.speed, 1..=9, "");
        }
        if fx.uses_direction() {
            heading(ui, "Direction");
            ui.horizontal(|ui| {
                ui.set_max_width(200.0);
                let at = s.lighting.direction_right_to_left as usize;
                if let Some(i) = choice(ui, &["Left → right", "Right → left"], Some(at), &[]) {
                    s.lighting.direction_right_to_left = i == 1;
                }
            });
        }

        heading(ui, "Colour");
        self.colors(ui, s, t);

        rule(ui);
        ui.add_space(8.0);
        switch(ui, &mut s.lighting.logo_on, "Lid logo");
        switch(
            ui,
            &mut s.lighting.mode_key_shows_power,
            "Mode key shows the power mode",
        );
        if s.lighting.mode_key_shows_power {
            note(
                ui,
                "Quiet blue · Balanced white · Performance amber · Turbo red · Eco green",
            );
        }
        switch(ui, &mut s.lighting.idle_off, "Off when idle");
        if s.lighting.idle_off {
            slider(ui, &mut s.lighting.idle_minutes, 1..=30, " min");
        }
    }

    fn colors(&mut self, ui: &mut Ui, s: &mut Settings, t: &Telemetry) {
        let effect = s.lighting.effect;
        if effect == Effect::Temperature {
            ui.horizontal(|ui| {
                swatch(
                    ui,
                    temp_color(t.cpu_temp.max(t.gpu_temp)),
                    false,
                    vec2(22.0, 22.0),
                );
                note(ui, "Follows the hotter of CPU and GPU, red at 90°");
            });
            return;
        }
        if !effect.uses_color() {
            note(ui, format!("{} picks its own colours.", effect.name()));
            return;
        }
        presets(ui, s);
        if s.lighting.effect != effect {
            self.picking = None;
            return;
        }
        ui.add_space(6.0);
        let per_zone = effect == Effect::Static && !s.lighting.same_color;
        ui.horizontal(|ui| {
            let names = ["1", "2", "3", "4", "Logo"];
            for (i, name) in names.iter().enumerate() {
                let c = if s.lighting.same_color || (i < 4 && !per_zone) {
                    s.lighting.zones[0]
                } else if i < 4 {
                    s.lighting.zones[i]
                } else {
                    s.lighting.logo_color
                };
                ui.vertical(|ui| {
                    let r = swatch(ui, c, self.picking == Some(i), vec2(52.0, 28.0));
                    ui.label(RichText::new(*name).size(11.5).color(TEXT_DIM));
                    if r.clicked() {
                        if self.picking == Some(i) {
                            self.picking = None;
                        } else {
                            self.picking = Some(i);
                            self.hsv = rgb_to_hsv(c);
                            self.hex = hex(c);
                        }
                    }
                });
            }
        });
        let mut same = s.lighting.same_color;
        if switch(ui, &mut same, "One colour for all").changed() {
            s.lighting.same_color = same;
            if same {
                let c = s.lighting.zones[0];
                s.lighting.zones = [c; 4];
                s.lighting.logo_color = c;
            }
        }
        if !s.lighting.same_color && effect != Effect::Static {
            note(ui, "Zones differ only in Static; this effect uses zone 1.");
        }

        let Some(idx) = self.picking else {
            note(ui, "Click a swatch to change it.");
            return;
        };
        ui.add_space(6.0);
        let mut changed = sv_square(ui, &mut self.hsv);
        changed |= hue_bar(ui, &mut self.hsv);
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.hex)
                    .desired_width(76.0)
                    .char_limit(7),
            );
            if r.changed()
                && let Some(c) = parse_hex(&self.hex)
            {
                self.hsv = rgb_to_hsv(c);
                set_color(s, idx, c);
            }
            for c in s.saved_colors.clone() {
                if swatch(ui, c, false, vec2(20.0, 20.0)).clicked() {
                    self.hsv = rgb_to_hsv(c);
                    self.hex = hex(c);
                    set_color(s, idx, c);
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.small_button("Keep this colour").clicked() {
                let c = hsv_to_rgb(self.hsv);
                s.saved_colors.retain(|x| *x != c);
                s.saved_colors.insert(0, c);
                s.saved_colors.truncate(8);
            }
            if ui.small_button("Done").clicked() {
                self.picking = None;
            }
        });
        if changed {
            let c = hsv_to_rgb(self.hsv);
            self.hex = hex(c);
            set_color(s, idx, c);
        }
    }

    // -----------------------------------------------------------------------------------------------------
    // Settings

    fn settings_tab(
        &mut self,
        ui: &mut Ui,
        s: &mut Settings,
        caps: &Caps,
        t: &Telemetry,
        log: &VecDeque<String>,
    ) {
        heading(ui, "CPU turbo");
        switch(ui, &mut s.turbo_off_quiet, "Off in Quiet and Eco");
        switch(ui, &mut s.turbo_off_battery, "Off on battery");
        note(
            ui,
            if t.turbo_off {
                "Off now: cooler and quieter, a little slower in short bursts."
            } else {
                "On now."
            },
        );

        rule(ui);
        self.graphics(ui, s, caps, t);

        rule(ui);
        heading(ui, "When the charger is plugged in or pulled");
        egui::Grid::new("rules")
            .num_columns(3)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("");
                note(ui, "Plugged in");
                note(ui, "On battery");
                ui.end_row();
                ui.label("Power");
                power_combo(
                    ui,
                    "pac",
                    &mut s.power_on_ac,
                    &[
                        power::QUIET,
                        power::BALANCED,
                        power::PERFORMANCE,
                        power::TURBO,
                    ],
                );
                power_combo(
                    ui,
                    "pbat",
                    &mut s.power_on_battery,
                    &[power::ECO, power::QUIET, power::BALANCED],
                );
                ui.end_row();
                ui.label("Fans");
                fan_combo(ui, "fac", &mut s.fan_on_ac);
                fan_combo(ui, "fbat", &mut s.fan_on_battery);
                ui.end_row();
            });

        heading(ui, "Fan control");
        fan_test(ui, caps);

        rule(ui);
        heading(ui, "Night");
        let n = &mut s.night;
        switch(ui, &mut n.enabled, "Quiet hours");
        if n.enabled {
            row(ui, "From – to", |ui| {
                time_edit(ui, "ne", &mut n.end);
                ui.label("–");
                time_edit(ui, "ns", &mut n.start);
            });
            row(ui, "Power", |ui| {
                power_combo(
                    ui,
                    "npow",
                    &mut n.power,
                    &[power::QUIET, power::BALANCED, power::ECO],
                )
            });
            ui.checkbox(&mut n.fans_auto, "Fans on Auto");
            ui.checkbox(&mut n.lights_off, "Lights off");
        }

        rule(ui);
        heading(ui, "Screen");
        let max_label = format!("{} Hz", caps.max_hz.max(60));
        if let Some(i) = choice(
            ui,
            &["60 Hz", &max_label],
            Some(s.refresh_max as usize),
            &[],
        ) {
            s.refresh_max = i == 1;
        }

        heading(ui, "Battery");
        if caps.battery_limit {
            switch(ui, &mut s.battery_limit, "Stop charging at 80%");
        }
        note(ui, "USB charging while the laptop is off");
        let current = s.usb_charging.or(caps.usb_charging);
        let sel = current.and_then(|v| USB_CHARGE_LEVELS.iter().position(|&x| x == v));
        if let Some(i) = choice(ui, &["Off", "Down to 10%", "20%", "30%"], sel, &[]) {
            s.usb_charging = Some(USB_CHARGE_LEVELS[i]);
        }

        rule(ui);
        heading(ui, "Games");
        self.games(ui, s);

        rule(ui);
        heading(ui, "App");
        switch(ui, &mut s.start_with_windows, "Start with Windows");
        if s.start_with_windows && caps.startup == Some(false) {
            note(ui, "The logon task could not be created.");
        }
        switch(ui, &mut s.hotkeys, "Shortcuts");
        if s.hotkeys {
            note(
                ui,
                "Ctrl+Alt+P power · F fans · L light · O this window · D screen off",
            );
        }
        switch(
            ui,
            &mut s.key_cycles_power,
            "PredatorSense key changes power mode",
        );
        switch(ui, &mut s.tray_shows_temp, "Temperature in the tray");
        let mut boot = s.boot_effect.or(caps.boot_effect).unwrap_or(true);
        if (caps.boot_effect.is_some() || s.boot_effect.is_some())
            && switch(ui, &mut boot, "Boot animation and sound").changed()
        {
            s.boot_effect = Some(boot);
        }

        rule(ui);
        egui::CollapsingHeader::new(RichText::new("Firmware log").size(12.5).color(TEXT_DIM))
            .default_open(false)
            .show(ui, |ui| {
                if log.is_empty() {
                    note(
                        ui,
                        "Empty. Press the PredatorSense key to see its event here.",
                    );
                }
                for line in log.iter().rev() {
                    ui.label(RichText::new(line).monospace().size(11.5).color(TEXT_DIM));
                }
            });
    }

    fn graphics(&mut self, ui: &mut Ui, s: &mut Settings, caps: &Caps, t: &Telemetry) {
        heading(ui, "Graphics");
        if let Some(now) = caps.gpu_mode {
            // Offered modes per Acer's own record; without it, the two every MUX model has.
            let offered = caps.gpu_modes.unwrap_or(0b011);
            let mut modes: Vec<(u8, &str)> = vec![(1, "Optimus"), (2, "NVIDIA only")];
            if offered & 0b100 != 0 {
                modes.push((3, "Automatic"));
            }
            let target = caps.gpu_mode_next.unwrap_or(now);
            let labels: Vec<&str> = modes.iter().map(|m| m.1).collect();
            if let Some(i) = choice(ui, &labels, modes.iter().position(|m| m.0 == target), &[])
                && modes[i].0 != target
            {
                state::send(Cmd::SetGpuMode(modes[i].0));
            }
            if target != now {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Takes effect after a restart.").strong());
                    if ui.button("Restart now").clicked() {
                        sys::restart_windows();
                    }
                });
            } else {
                note(
                    ui,
                    "Optimus saves battery; NVIDIA only gives more frames in games. A restart switches.",
                );
            }
        } else {
            note(ui, "GPU switching is not offered by this firmware.");
        }
        if caps.overdrive.is_some() || s.overdrive.is_some() {
            let mut on = s.overdrive.or(caps.overdrive).unwrap_or(false);
            if switch(ui, &mut on, "Panel overdrive (less ghosting)").changed() {
                s.overdrive = Some(on);
            }
        }
        ui.add_space(4.0);
        if t.rtx_apps.is_empty() {
            note(ui, "No app holds the RTX right now.");
        } else {
            note(
                ui,
                "Apps holding the RTX (it stays awake and drains the battery):",
            );
            for (_, name, path, use_pct) in &t.rtx_apps {
                ui.horizontal(|ui| {
                    ui.label(name);
                    if *use_pct >= 1.0 {
                        note(ui, format!("{use_pct:.0}%"));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("Use integrated GPU")
                            .on_hover_text("From the app's next start")
                            .clicked()
                        {
                            state::send(Cmd::PreferIntegrated(path.clone()));
                        }
                    });
                });
            }
        }
    }

    fn games(&mut self, ui: &mut Ui, s: &mut Settings) {
        switch(
            ui,
            &mut s.games_enabled,
            "Change settings while a game runs",
        );
        if !s.games_enabled {
            return;
        }
        let mut remove = None;
        for (i, g) in s.games.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let open = self.open_game == Some(i);
                if ui.selectable_label(open, &g.exe).clicked() {
                    self.open_game = if open { None } else { Some(i) };
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            });
            if self.open_game == Some(i) {
                game_editor(ui, i, g);
            }
        }
        if let Some(i) = remove {
            s.games.remove(i);
            self.open_game = None;
        }
        ui.horizontal(|ui| {
            if self.candidates.is_empty() {
                self.candidates = sys::windowed_process_names();
            }
            let text = if self.candidate.is_empty() {
                "Pick a running app"
            } else {
                self.candidate.as_str()
            };
            egui::ComboBox::from_id_salt("cand")
                .selected_text(text)
                .width(190.0)
                .show_ui(ui, |ui| {
                    for c in &self.candidates {
                        ui.selectable_value(&mut self.candidate, c.clone(), c);
                    }
                });
            if ui.small_button("Refresh").clicked() {
                self.candidates = sys::windowed_process_names();
            }
            if ui
                .add_enabled(!self.candidate.is_empty(), egui::Button::new("Add"))
                .clicked()
            {
                let exe = std::mem::take(&mut self.candidate);
                if !s.games.iter().any(|g| g.exe.eq_ignore_ascii_case(&exe)) {
                    s.games.push(GameProfile {
                        exe,
                        power: Some(power::TURBO),
                        fan: Some(FanKind::AutoPlus),
                        ..Default::default()
                    });
                    self.open_game = Some(s.games.len() - 1);
                }
            }
        });
    }
}

fn game_editor(ui: &mut Ui, i: usize, g: &mut GameProfile) {
    egui::Grid::new(("game", i))
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Power");
            power_combo(
                ui,
                &format!("gp{i}"),
                &mut g.power,
                &[
                    power::QUIET,
                    power::BALANCED,
                    power::PERFORMANCE,
                    power::TURBO,
                ],
            );
            ui.end_row();
            ui.label("Fans");
            fan_combo(ui, &format!("gf{i}"), &mut g.fan);
            ui.end_row();
            ui.label("Screen");
            let text = match g.refresh_max {
                None => "Don't change",
                Some(true) => "Max Hz",
                Some(false) => "60 Hz",
            };
            egui::ComboBox::from_id_salt(format!("gr{i}"))
                .selected_text(text)
                .width(140.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut g.refresh_max, None, "Don't change");
                    ui.selectable_value(&mut g.refresh_max, Some(true), "Max Hz");
                    ui.selectable_value(&mut g.refresh_max, Some(false), "60 Hz");
                });
            ui.end_row();
            ui.label("Light");
            let text = g.effect.map(|e| e.name()).unwrap_or("Don't change");
            egui::ComboBox::from_id_salt(format!("ge{i}"))
                .selected_text(text)
                .width(140.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut g.effect, None, "Don't change");
                    for e in Effect::ALL {
                        ui.selectable_value(&mut g.effect, Some(e), e.name());
                    }
                });
            ui.end_row();
            ui.label("Colour");
            ui.horizontal(|ui| {
                let mut on = g.color.is_some();
                if ui.checkbox(&mut on, "").changed() {
                    g.color = on.then_some([255, 30, 30]);
                }
                if let Some(c) = &mut g.color {
                    let mut c32 = rgb(*c);
                    if ui.color_edit_button_srgba(&mut c32).changed() {
                        *c = [c32.r(), c32.g(), c32.b()];
                    }
                }
            });
            ui.end_row();
        });
}

/// Runs custom duty 20% then 100% for 12 s each and shows whether the RPM followed. Auto+, Fixed and
/// Curve depend on it; some firmware takes the writes and ignores them.
fn fan_test(ui: &mut Ui, caps: &Caps) {
    let running = caps
        .fan_test
        .as_deref()
        .is_some_and(|t| t.starts_with("Testing"));
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!running, egui::Button::new("Check fan control").small())
            .clicked()
        {
            state::send(Cmd::FanTest);
        }
        if caps.fan_test.is_none() {
            note(ui, "24 s: 20%, then 100%");
        }
    });
    if let Some(t) = &caps.fan_test {
        note(ui, t);
    }
}

/// Ready-made schemes: four keyboard zones left to right, then the lid logo.
const PRESETS: [(&str, [Rgb; 4], Rgb); 8] = [
    (
        "Sunset",
        [
            [120, 40, 200],
            [230, 50, 140],
            [255, 110, 40],
            [255, 190, 40],
        ],
        [255, 110, 40],
    ),
    (
        "Aurora",
        [
            [40, 230, 120],
            [0, 200, 170],
            [40, 120, 255],
            [150, 70, 255],
        ],
        [0, 200, 170],
    ),
    (
        "Vapor",
        [[255, 60, 200], [190, 60, 255], [90, 90, 255], [0, 220, 255]],
        [190, 60, 255],
    ),
    (
        "Ember",
        [[255, 30, 0], [255, 70, 0], [255, 120, 0], [255, 170, 20]],
        [255, 70, 0],
    ),
    (
        "Ice",
        [
            [255, 255, 255],
            [170, 230, 255],
            [80, 190, 255],
            [30, 120, 255],
        ],
        [170, 230, 255],
    ),
    (
        "Toxic",
        [[180, 255, 0], [90, 255, 30], [0, 240, 90], [0, 210, 150]],
        [90, 255, 30],
    ),
    (
        "Ocean",
        [[0, 60, 255], [0, 130, 255], [0, 200, 220], [0, 255, 200]],
        [0, 200, 220],
    ),
    ("White", [[255, 255, 255]; 4], [255, 255, 255]),
];

/// The preset chips: a strip of the four zone colours with the name under it. A click sets Static with
/// those colours (a gradient across zones exists only in Static).
fn presets(ui: &mut Ui, s: &mut Settings) {
    let gap = 6.0;
    let w = ((ui.available_width() - gap * 3.0) / 4.0).floor();
    for row in PRESETS.chunks(4) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (name, zones, logo) in row {
                ui.vertical(|ui| {
                    ui.set_width(w);
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, 18.0), Sense::click());
                    let stripe = rect.width() / 4.0;
                    for (i, z) in zones.iter().enumerate() {
                        let r = Rect::from_min_size(
                            pos2(rect.left() + stripe * i as f32, rect.top()),
                            vec2(stripe + 0.5, rect.height()),
                        );
                        ui.painter().rect_filled(r, 0, rgb(*z));
                    }
                    let current = s.lighting.effect == Effect::Static
                        && !s.lighting.same_color
                        && s.lighting.zones == *zones;
                    if current {
                        ui.painter().rect_stroke(
                            rect,
                            2,
                            Stroke::new(2.0, CHOSEN),
                            StrokeKind::Outside,
                        );
                    } else if resp.hovered() {
                        ui.painter().rect_stroke(
                            rect,
                            2,
                            Stroke::new(1.0, TEXT_DIM),
                            StrokeKind::Outside,
                        );
                    }
                    ui.label(RichText::new(*name).size(11.5).color(if current {
                        TEXT
                    } else {
                        TEXT_DIM
                    }));
                    if resp.clicked() {
                        let l = &mut s.lighting;
                        l.effect = Effect::Static;
                        l.same_color = *zones == [zones[0]; 4];
                        l.zones = *zones;
                        l.logo_color = *logo;
                    }
                });
            }
        });
    }
}

fn set_color(s: &mut Settings, idx: usize, c: Rgb) {
    let l = &mut s.lighting;
    if l.same_color {
        l.zones = [c; 4];
        l.logo_color = c;
    } else if idx < 4 {
        if l.effect == Effect::Static {
            l.zones[idx] = c;
        } else {
            l.zones = [c; 4];
        }
    } else {
        l.logo_color = c;
    }
}

/// Right-to-left layout: minutes first, then hours.
fn time_edit(ui: &mut Ui, id: &str, minutes: &mut u16) {
    let mut h = *minutes / 60;
    let mut m = *minutes % 60;
    ui.push_id(id, |ui| {
        ui.add(
            egui::DragValue::new(&mut m)
                .range(0..=59)
                .speed(0.5)
                .custom_formatter(|v, _| format!("{v:02}")),
        );
        ui.label(":");
        ui.add(
            egui::DragValue::new(&mut h)
                .range(0..=23)
                .custom_formatter(|v, _| format!("{v:02}")),
        );
    });
    *minutes = h * 60 + m;
}

// ---------------------------------------------------------------------------------------------------------
// Colour picker

fn swatch(ui: &mut Ui, c: Rgb, selected: bool, size: Vec2) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    p.rect_filled(rect, 4, rgb(c));
    if selected {
        p.rect_stroke(rect, 4, Stroke::new(2.0, CHOSEN), StrokeKind::Outside);
    } else if resp.hovered() {
        p.rect_stroke(rect, 4, Stroke::new(1.0, TEXT_DIM), StrokeKind::Outside);
    }
    resp
}

/// Saturation (x) × value (y) square for the current hue.
fn sv_square(ui: &mut Ui, hsv: &mut [f32; 3]) -> bool {
    let size = vec2(ui.available_width(), 140.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    const N: usize = 16;
    let mut mesh = Mesh::default();
    for yi in 0..=N {
        for xi in 0..=N {
            let (sx, vy) = (xi as f32 / N as f32, yi as f32 / N as f32);
            let c = hsv_to_rgb([hsv[0], sx, 1.0 - vy]);
            mesh.colored_vertex(
                pos2(
                    rect.left() + sx * rect.width(),
                    rect.top() + vy * rect.height(),
                ),
                rgb(c),
            );
        }
    }
    let row = (N + 1) as u32;
    for yi in 0..N as u32 {
        for xi in 0..N as u32 {
            let i = yi * row + xi;
            mesh.add_triangle(i, i + 1, i + row);
            mesh.add_triangle(i + 1, i + row + 1, i + row);
        }
    }
    ui.painter().add(Shape::mesh(mesh));
    let mut changed = false;
    if let Some(p) = resp.interact_pointer_pos()
        && (resp.dragged() || resp.clicked())
    {
        hsv[1] = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        hsv[2] = 1.0 - ((p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
        changed = true;
    }
    let marker = pos2(
        rect.left() + hsv[1] * rect.width(),
        rect.top() + (1.0 - hsv[2]) * rect.height(),
    );
    ui.painter()
        .circle_stroke(marker, 6.0, Stroke::new(2.0, Color32::WHITE));
    ui.painter()
        .circle_stroke(marker, 7.5, Stroke::new(1.0, Color32::BLACK));
    changed
}

fn hue_bar(ui: &mut Ui, hsv: &mut [f32; 3]) -> bool {
    let size = vec2(ui.available_width(), 16.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    const N: usize = 36;
    let mut mesh = Mesh::default();
    for i in 0..=N {
        let h = i as f32 / N as f32;
        let c = rgb(hsv_to_rgb([h, 1.0, 1.0]));
        let x = rect.left() + h * rect.width();
        mesh.colored_vertex(pos2(x, rect.top()), c);
        mesh.colored_vertex(pos2(x, rect.bottom()), c);
    }
    for i in 0..N as u32 {
        let a = i * 2;
        mesh.add_triangle(a, a + 1, a + 2);
        mesh.add_triangle(a + 1, a + 3, a + 2);
    }
    ui.painter().add(Shape::mesh(mesh));
    let mut changed = false;
    if let Some(p) = resp.interact_pointer_pos()
        && (resp.dragged() || resp.clicked())
    {
        hsv[0] = ((p.x - rect.left()) / rect.width()).clamp(0.0, 0.999);
        changed = true;
    }
    let x = rect.left() + hsv[0] * rect.width();
    ui.painter().rect_stroke(
        Rect::from_center_size(pos2(x, rect.center().y), vec2(6.0, rect.height() + 4.0)),
        2,
        Stroke::new(2.0, Color32::WHITE),
        StrokeKind::Middle,
    );
    changed
}

fn hsv_to_rgb([h, s, v]: [f32; 3]) -> Rgb {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    ]
}

fn rgb_to_hsv(c: Rgb) -> [f32; 3] {
    let [r, g, b] = c.map(|x| x as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    [h, if max == 0.0 { 0.0 } else { d / max }, max]
}

fn hex(c: Rgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

// ---------------------------------------------------------------------------------------------------------
// Fan curve and history

fn curve_editor(ui: &mut Ui, curve: &mut [[u8; 2]], temp: u32, dragging: &mut Option<usize>) {
    let (resp, p) = ui.allocate_painter(vec2(ui.available_width(), 160.0), Sense::click_and_drag());
    let r = resp.rect.shrink2(vec2(24.0, 12.0));
    let (t0, t1) = (20.0f32, 100.0f32);
    let to_screen = |t: f32, pct: f32| {
        pos2(
            r.left() + (t - t0) / (t1 - t0) * r.width(),
            r.bottom() - pct / 100.0 * r.height(),
        )
    };
    p.rect_filled(resp.rect, 4, PLOT_BG);
    for pct in [0, 50, 100] {
        let y = to_screen(t0, pct as f32).y;
        p.line_segment(
            [pos2(r.left(), y), pos2(r.right(), y)],
            Stroke::new(1.0, RULE),
        );
        p.text(
            pos2(r.left() - 4.0, y),
            Align2::RIGHT_CENTER,
            format!("{pct}"),
            FontId::proportional(10.5),
            TEXT_DIM,
        );
    }
    for t in (30..=100).step_by(10) {
        let x = to_screen(t as f32, 0.0).x;
        p.text(
            pos2(x, r.bottom() + 1.0),
            Align2::CENTER_TOP,
            format!("{t}°"),
            FontId::proportional(10.0),
            TEXT_DIM,
        );
    }
    if temp > 0 {
        let x = to_screen(temp as f32, 0.0).x.clamp(r.left(), r.right());
        p.line_segment(
            [pos2(x, r.top()), pos2(x, r.bottom())],
            Stroke::new(1.0, heat(temp)),
        );
    }

    let pts: Vec<Pos2> = curve
        .iter()
        .map(|q| to_screen(q[0] as f32, q[1] as f32))
        .collect();
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        *dragging = pts
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.distance(pos).total_cmp(&b.1.distance(pos)))
            .map(|(i, _)| i);
    }
    if resp.drag_stopped() {
        *dragging = None;
    }
    if let (Some(i), Some(pos)) = (*dragging, resp.interact_pointer_pos()) {
        let t = t0 + (pos.x - r.left()) / r.width() * (t1 - t0);
        let pct = (r.bottom() - pos.y) / r.height() * 100.0;
        let lo = if i == 0 {
            20.0
        } else {
            curve[i - 1][0] as f32 + 1.0
        };
        let hi = if i + 1 == curve.len() {
            100.0
        } else {
            curve[i + 1][0] as f32 - 1.0
        };
        curve[i] = [
            t.clamp(lo, hi).round() as u8,
            pct.clamp(0.0, 100.0).round() as u8,
        ];
    }
    let pts: Vec<Pos2> = curve
        .iter()
        .map(|q| to_screen(q[0] as f32, q[1] as f32))
        .collect();
    p.add(Shape::line(pts.clone(), Stroke::new(2.0, CHOSEN)));
    for (i, q) in pts.iter().enumerate() {
        let active = *dragging == Some(i);
        p.circle_filled(*q, if active { 6.0 } else { 4.0 }, CHOSEN);
        if active {
            let c = curve[i];
            p.text(
                *q + vec2(0.0, -12.0),
                Align2::CENTER_BOTTOM,
                format!("{}° {}%", c[0], c[1]),
                FontId::proportional(11.5),
                TEXT,
            );
        }
    }
}

fn history_plot(ui: &mut Ui, h: &VecDeque<Sample>) {
    if h.len() < 2 {
        note(ui, "Collecting…");
        return;
    }
    let width = ui.available_width();
    let plot = |ui: &mut Ui, title: &str, a: Vec<f32>, b: Vec<f32>, lo: f32, hi: f32| {
        let (resp, p) = ui.allocate_painter(vec2(width, 64.0), Sense::hover());
        let r = resp.rect;
        p.rect_filled(r, 4, PLOT_BG);
        let n = crate::state::HISTORY_LEN as f32;
        let line = |vals: &[f32], c: Color32| {
            let off = n - vals.len() as f32;
            let pts: Vec<Pos2> = vals
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let x = r.left() + (off + i as f32) / (n - 1.0) * r.width();
                    let y = r.bottom()
                        - ((v - lo) / (hi - lo)).clamp(0.0, 1.0) * (r.height() - 8.0)
                        - 4.0;
                    pos2(x, y)
                })
                .collect();
            p.add(Shape::line(pts, Stroke::new(1.5, c)));
        };
        line(&b, SERIES_2);
        line(&a, CHOSEN);
        p.text(
            r.left_top() + vec2(6.0, 4.0),
            Align2::LEFT_TOP,
            title,
            FontId::proportional(11.0),
            TEXT_DIM,
        );
        p.text(
            r.right_top() + vec2(-6.0, 4.0),
            Align2::RIGHT_TOP,
            format!("{hi:.0}"),
            FontId::proportional(10.5),
            TEXT_DIM,
        );
        p.text(
            r.right_bottom() + vec2(-6.0, -4.0),
            Align2::RIGHT_BOTTOM,
            format!("{lo:.0}"),
            FontId::proportional(10.5),
            TEXT_DIM,
        );
    };
    let ct: Vec<f32> = h.iter().map(|s| s.cpu_temp as f32).collect();
    let gt: Vec<f32> = h.iter().map(|s| s.gpu_temp as f32).collect();
    let cr: Vec<f32> = h.iter().map(|s| s.cpu_rpm as f32).collect();
    let gr: Vec<f32> = h.iter().map(|s| s.gpu_rpm as f32).collect();
    let max_rpm = cr.iter().chain(&gr).cloned().fold(1000.0f32, f32::max);
    plot(ui, "°C   CPU white, GPU grey", ct, gt, 30.0, 100.0);
    plot(ui, "rpm", cr, gr, 0.0, (max_rpm / 500.0).ceil() * 500.0);
}
