#![windows_subsystem = "windows"]

mod acer;
mod core;
mod diag;
mod ene;
mod events;
mod fans;
mod gpu;
mod lighting;
mod settings;
mod state;
mod sys;
mod tray;
mod turbo;
mod ui;
mod window;
mod wmi;

use std::sync::mpsc;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::w;

fn main() {
    // One copy at a time; a second launch shows the first.
    let _mutex = unsafe { CreateMutexW(None, true, w!("Local\\Kelvin")) };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        window::show_other_instance();
        return;
    }

    std::panic::set_hook(Box::new(|info| {
        let _ = std::fs::write(settings::data_dir().join("crash.txt"), format!("{info}"));
        // The fans may be in a mode only this app steers: give them back to the firmware.
        wmi::init_com();
        acer::Acer::new().set_fan_behavior(acer::FanHw::Auto);
    }));

    diag::log(format!("Start {}", env!("CARGO_PKG_VERSION")));
    let hidden = std::env::args().any(|a| a.eq_ignore_ascii_case("-hidden"));
    let (tx, rx) = mpsc::channel();
    state::init(settings::Settings::load(), tx);
    state::shared().window_visible = !hidden;

    std::thread::Builder::new()
        .name("hardware".into())
        .spawn(move || core::run(rx))
        .expect("hardware thread");
    events::spawn();
    tray::spawn();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(window::TITLE)
            .with_inner_size([440.0, 660.0])
            .with_min_inner_size([400.0, 420.0])
            .with_visible(!hidden),
        ..Default::default()
    };
    let _ = eframe::run_native(
        window::TITLE,
        options,
        Box::new(|cc| {
            state::set_ctx(cc.egui_ctx.clone());
            ui::style(&cc.egui_ctx);
            Ok(Box::new(ui::App::default()))
        }),
    );
    // eframe returns when the window really closes (Exit from the tray ends the process itself).
    tray::quit();
}
