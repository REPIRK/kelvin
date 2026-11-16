//! Firmware events (`AcerGenericEvent`, 8 bytes: function, key, device state...). Forwarded to the hardware
//! thread, which logs them and acts on the mode / PredatorSense key.

use std::time::Duration;

use crate::state::{self, Cmd};
use crate::wmi::{self, Wmi};

pub fn spawn() {
    std::thread::Builder::new()
        .name("firmware-events".into())
        .spawn(run)
        .ok();
}

fn run() {
    wmi::init_com();
    loop {
        match Wmi::connect("ROOT\\WMI").and_then(|w| {
            w.subscribe("SELECT * FROM AcerGenericEvent")
                .map(|e| (w, e))
        }) {
            Ok((_wmi, events)) => loop {
                match wmi::next_event(&events, 1000, "EventDetail") {
                    Ok(Some(bytes)) => state::send(Cmd::FirmwareEvent(bytes)),
                    Ok(None) => {}
                    Err(_) => {
                        // Dead after sleep or a WMI restart: subscribe again.
                        std::thread::sleep(Duration::from_secs(5));
                        break;
                    }
                }
            },
            Err(e) => {
                state::log(format!("Firmware events unavailable: {e}"));
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    }
}
