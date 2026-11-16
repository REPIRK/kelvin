//! `cargo run --example shots`: renders the three tabs with demo data, off-screen, to docs/*.png.
//! Needs no administrator rights and touches no hardware.

#![allow(dead_code)]

#[path = "../src/acer.rs"]
mod acer;
#[path = "../src/diag.rs"]
mod diag;
#[path = "../src/ene.rs"]
mod ene;
#[path = "../src/lighting.rs"]
mod lighting;
#[path = "../src/settings.rs"]
mod settings;
#[path = "../src/state.rs"]
mod state;
#[path = "../src/sys.rs"]
mod sys;
#[path = "../src/ui.rs"]
mod ui;
#[path = "../src/window.rs"]
mod window;
#[path = "../src/wmi.rs"]
mod wmi;

use eframe::egui;
use settings::{Effect, FanKind, Settings};
use state::Sample;

const NAMES: [&str; 3] = ["main", "light", "settings"];

fn demo_state() {
    let mut s = Settings::default();
    s.fan = FanKind::AutoPlus;
    s.boost = 20;
    s.lighting.effect = Effect::Static;
    s.lighting.same_color = false;
    s.lighting.zones = [
        [40, 230, 120],
        [0, 200, 170],
        [40, 120, 255],
        [150, 70, 255],
    ];
    s.lighting.logo_color = [0, 200, 170];
    s.battery_limit = true;
    let (tx, _rx) = std::sync::mpsc::channel();
    state::init(s, tx);
    let mut sh = state::shared();
    let t = &mut sh.tele;
    t.cpu_temp = 71;
    t.gpu_temp = 64;
    t.cpu_rpm = 3420;
    t.gpu_rpm = 3360;
    t.on_ac = true;
    t.power = acer::power::BALANCED;
    t.fan_text = "Auto +20%".into();
    let c = &mut sh.caps;
    c.acer = true;
    c.hid_lighting = true;
    c.battery_limit = true;
    c.boot_effect = Some(true);
    c.usb_charging = Some(0);
    c.max_hz = 240;
    c.startup = Some(true);
    c.gpu_mode = Some(1);
    c.gpu_modes = Some(7);
    c.overdrive = Some(true);
    // Ten minutes of a game starting and stopping.
    for i in 0..state::HISTORY_LEN {
        let x = i as f32 / state::HISTORY_LEN as f32;
        let load = if (0.25..0.75).contains(&x) { 1.0 } else { 0.0 };
        let wobble = (i as f32 * 0.37).sin() * 2.0;
        sh.history.push_back(Sample {
            cpu_temp: (52.0 + 30.0 * load + wobble) as u16,
            gpu_temp: (46.0 + 26.0 * load + wobble * 0.7) as u16,
            cpu_rpm: (2300.0 + 2600.0 * load + wobble * 40.0) as u16,
            gpu_rpm: (2200.0 + 2500.0 * load + wobble * 35.0) as u16,
        });
    }
}

struct Shots {
    app: ui::App,
    tab: usize,
    frames: u32,
    asked: bool,
    tx: std::sync::mpsc::Sender<std::sync::Arc<egui::ColorImage>>,
    rx: std::sync::mpsc::Receiver<std::sync::Arc<egui::ColorImage>>,
}

impl eframe::App for Shots {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        eframe::App::ui(&mut self.app, ui, frame);
        let ctx = ui.ctx().clone();
        if let Ok(image) = self.rx.try_recv() {
            {
                save(&image, NAMES[self.tab]);
                self.tab += 1;
                if self.tab == NAMES.len() {
                    std::process::exit(0);
                }
                self.app.set_tab(self.tab);
                self.frames = 0;
                self.asked = false;
            }
        }
        self.frames += 1;
        if self.frames > 15 && !self.asked {
            self.asked = true;
            let tx = self.tx.clone();
            ctx.request_screenshot(move |img| {
                let _ = tx.send(img);
            });
        }
        ctx.request_repaint();
    }
}

fn save(img: &egui::ColorImage, name: &str) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs");
    let _ = std::fs::create_dir_all(&dir);
    let file = std::fs::File::create(dir.join(format!("{name}.png"))).expect("docs/*.png");
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        img.size[0] as u32,
        img.size[1] as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().expect("png header");
    let bytes: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
    w.write_image_data(&bytes).expect("png data");
}

fn main() {
    demo_state();
    let mut app = ui::App::default();
    app.set_tab(0);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kelvin shots")
            .with_inner_size([440.0, 900.0])
            .with_position([-6000.0, 50.0])
            .with_taskbar(false),
        persist_window: false,
        ..Default::default()
    };
    let _ = eframe::run_native(
        "Kelvin shots",
        options,
        Box::new(|cc| {
            ui::style(&cc.egui_ctx);
            let (tx, rx) = std::sync::mpsc::channel();
            Ok(Box::new(Shots {
                app,
                tab: 0,
                frames: 0,
                asked: false,
                tx,
                rx,
            }))
        }),
    );
}
