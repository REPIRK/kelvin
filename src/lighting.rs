//! What the lighting controller should show, from the settings and the moment (temperature, idle fade,
//! night, game, power mode). Pure: the hardware thread sends only what changed.

use crate::acer::power;
use crate::ene::{self, Report};
use crate::settings::{Effect, Lighting, Rgb};

pub struct Moment {
    pub effect: Effect,
    /// A game's colour, replacing every swatch.
    pub color: Option<Rgb>,
    /// 0..=1, multiplies brightness (idle fade, night).
    pub level: f32,
    pub temp: u32,
    pub power: u8,
}

/// Reports per device: keyboard, lid logo, mode key. An empty list leaves that device alone.
pub fn reports(l: &Lighting, m: &Moment) -> [Vec<Report>; 3] {
    let bri = ((l.brightness as f32) * m.level).round() as u8;
    let dark = bri == 0;
    let zones: [Rgb; 4] = match (m.color, m.effect) {
        (Some(c), _) => [c; 4],
        (None, Effect::Temperature) => [temp_color(m.temp); 4],
        _ => l.zones,
    };
    let logo_color = match (m.color, m.effect) {
        (Some(c), _) => c,
        (None, Effect::Temperature) => temp_color(m.temp),
        _ if l.same_color => zones[0],
        _ => l.logo_color,
    };
    let direction = if l.direction_right_to_left { 2 } else { 1 };
    let speed = l.speed.clamp(1, 9);

    let keyboard = if dark {
        vec![off(ene::KEYBOARD, ene::KEYBOARD_ZONES)]
    } else {
        match m.effect {
            Effect::Static | Effect::Temperature => {
                if zones.iter().all(|z| *z == zones[0]) {
                    vec![solid(ene::KEYBOARD, zones[0], bri, ene::KEYBOARD_ZONES)]
                } else {
                    (0..4)
                        .map(|i| solid(ene::KEYBOARD, zones[i], bri, 1 << i))
                        .collect()
                }
            }
            e => vec![Report {
                device: ene::KEYBOARD,
                effect: effect_byte(e),
                speed,
                brightness: bri,
                direction,
                rgb: zones[0],
                zones: ene::KEYBOARD_ZONES,
            }],
        }
    };

    let logo = if dark || !l.logo_on {
        vec![off(ene::LID_LOGO, 1)]
    } else {
        // The logo has Static, Breathing and Neon only; other keyboard effects show on it as Neon.
        let effect = match m.effect {
            Effect::Static | Effect::Temperature => ene::effect::STATIC,
            Effect::Breathing => ene::effect::BREATHING,
            _ => ene::effect::NEON,
        };
        vec![Report {
            device: ene::LID_LOGO,
            effect,
            speed: speed.div_ceil(2),
            brightness: bri,
            direction: 1,
            rgb: logo_color,
            zones: 1,
        }]
    };

    let key_bri = (100.0 * m.level).round() as u8;
    let mode_key = if !l.mode_key_shows_power || key_bri == 0 {
        vec![off(ene::MODE_KEY, 1)]
    } else {
        vec![solid(ene::MODE_KEY, power_color(m.power), key_bri, 1)]
    };

    [keyboard, logo, mode_key]
}

/// Dark: black at brightness 1. A report with brightness 0 alone left the colour lit on this controller
/// (kbrgb found the same and turns zones off the same way).
fn off(device: u8, zones: u16) -> Report {
    solid(device, [0; 3], 1, zones)
}

fn solid(device: u8, rgb: Rgb, brightness: u8, zones: u16) -> Report {
    Report {
        device,
        effect: ene::effect::STATIC,
        speed: 0,
        brightness,
        direction: 1,
        rgb,
        zones,
    }
}

fn effect_byte(e: Effect) -> u8 {
    match e {
        Effect::Breathing => ene::effect::BREATHING,
        Effect::Neon => ene::effect::NEON,
        Effect::Wave => ene::effect::WAVE,
        Effect::Shifting => ene::effect::SHIFTING,
        Effect::Zoom => ene::effect::ZOOM,
        Effect::Meteor => ene::effect::METEOR,
        Effect::Twinkling => ene::effect::TWINKLING,
        _ => ene::effect::STATIC,
    }
}

pub fn power_color(mode: u8) -> Rgb {
    match mode {
        power::QUIET => [0, 120, 255],
        power::PERFORMANCE => [255, 150, 0],
        power::TURBO => [255, 30, 30],
        power::ECO => [40, 220, 90],
        _ => [255, 255, 255],
    }
}

/// Teal below 50 °C, through yellow (70) and orange (80) to red at 90. Stepped by 2 °C so the controller
/// is not rewritten for every degree.
pub fn temp_color(temp: u32) -> Rgb {
    let t = (temp / 2 * 2) as f32;
    let stops: [(f32, [f32; 3]); 4] = [
        (50.0, [0.0, 200.0, 160.0]),
        (70.0, [255.0, 220.0, 50.0]),
        (80.0, [255.0, 140.0, 0.0]),
        (90.0, [255.0, 40.0, 40.0]),
    ];
    if t <= stops[0].0 {
        return stops[0].1.map(|v| v as u8);
    }
    for w in stops.windows(2) {
        let ((a, ca), (b, cb)) = (w[0], w[1]);
        if t <= b {
            let k = (t - a) / (b - a);
            return [0, 1, 2].map(|i| (ca[i] + (cb[i] - ca[i]) * k) as u8);
        }
    }
    stops[3].1.map(|v| v as u8)
}
