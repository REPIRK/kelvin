//! `cargo run --example probe`: finds the lighting controller and the Acer WMI class without the app.
//! WMI answers administrators only; run elevated to see sensor values.

#[path = "../src/ene.rs"]
#[allow(dead_code)]
mod ene;
#[path = "../src/gpu.rs"]
#[allow(dead_code)]
mod gpu;
#[path = "../src/wmi.rs"]
#[allow(dead_code)]
mod wmi;

fn main() {
    println!(
        "nvidia luid: {:?}, gpu modes (Acer): {:?}",
        gpu::nvidia_luid(),
        gpu::mode_capability()
    );
    if let Some(mut u) = gpu::GpuUsers::new() {
        std::thread::sleep(std::time::Duration::from_millis(500));
        println!("pids on the RTX: {:?}", u.poll());
    }
    match ene::Ene::open() {
        Some(_) => println!("lighting controller: found"),
        None => println!("lighting controller: not found"),
    }
    wmi::init_com();
    match wmi::Wmi::connect("ROOT\\WMI") {
        Ok(mut w) => {
            println!(
                "AcerGamingFunction instance: {}",
                w.has_instance("AcerGamingFunction")
            );
            let r = w.call(
                "AcerGamingFunction",
                "GetGamingSysInfo",
                &[("gmInput", wmi::Arg::U32(0x0101))],
                &["gmOutput"],
            );
            println!("CPU temperature call: {r:?}");
        }
        Err(e) => println!("WMI connect: {e}"),
    }
}
