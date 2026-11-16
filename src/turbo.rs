//! CPU turbo on or off through the active Windows power scheme ("Processor performance boost mode").
//! Off makes Quiet and Eco really quiet: no turbo clocks, a cooler CPU, fans that stay low.
//! The scheme's own values are saved to a file before the first change and written back when turbo is
//! allowed again, so a crash or kill of this app is undone on its next start.

use std::collections::HashMap;

use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Power::{
    PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, PowerSetActiveScheme,
    PowerWriteACValueIndex, PowerWriteDCValueIndex,
};
use windows::core::GUID;

use crate::settings::data_dir;

const SUB_PROCESSOR: GUID = GUID::from_u128(0x54533251_82be_4824_96c1_47eeee0b6c10);
const PERF_BOOST_MODE: GUID = GUID::from_u128(0xbe337238_0d82_4146_a960_4f3749d470c7);

fn originals_path() -> std::path::PathBuf {
    data_dir().join("turbo_original.json")
}

fn load() -> HashMap<String, [u32; 2]> {
    std::fs::read_to_string(originals_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn store(map: &HashMap<String, [u32; 2]>) {
    if map.is_empty() {
        let _ = std::fs::remove_file(originals_path());
    } else if let Ok(json) = serde_json::to_string(map) {
        let _ = std::fs::write(originals_path(), json);
    }
}

fn active_scheme() -> Option<GUID> {
    unsafe {
        let mut p: *mut GUID = std::ptr::null_mut();
        if PowerGetActiveScheme(None, &mut p).is_err() || p.is_null() {
            return None;
        }
        let g = *p;
        CoTaskMemFree(Some(p as _));
        Some(g)
    }
}

/// Allows or forbids turbo on the active scheme, for both AC and battery. Returns false on failure.
pub fn set_allowed(allowed: bool) -> bool {
    let Some(scheme) = active_scheme() else {
        return false;
    };
    let key = format!("{scheme:?}");
    let mut originals = load();
    unsafe {
        if allowed {
            let Some([ac, dc]) = originals.remove(&key) else {
                return true;
            };
            let ok = PowerWriteACValueIndex(
                None,
                &scheme,
                Some(&SUB_PROCESSOR),
                Some(&PERF_BOOST_MODE),
                ac,
            )
            .is_ok()
                & (PowerWriteDCValueIndex(
                    None,
                    &scheme,
                    Some(&SUB_PROCESSOR),
                    Some(&PERF_BOOST_MODE),
                    dc,
                ) == 0);
            let _ = PowerSetActiveScheme(None, Some(&scheme));
            store(&originals);
            ok
        } else {
            if let std::collections::hash_map::Entry::Vacant(slot) = originals.entry(key) {
                let (mut ac, mut dc) = (2u32, 2u32);
                let _ = PowerReadACValueIndex(
                    None,
                    Some(&scheme),
                    Some(&SUB_PROCESSOR),
                    Some(&PERF_BOOST_MODE),
                    &mut ac,
                );
                let _ = PowerReadDCValueIndex(
                    None,
                    Some(&scheme),
                    Some(&SUB_PROCESSOR),
                    Some(&PERF_BOOST_MODE),
                    &mut dc,
                );
                if ac == 0 && dc == 0 {
                    // Already off in the scheme itself: nothing of ours to restore later.
                    return true;
                }
                slot.insert([ac, dc]);
                store(&originals);
            }
            let ok = PowerWriteACValueIndex(
                None,
                &scheme,
                Some(&SUB_PROCESSOR),
                Some(&PERF_BOOST_MODE),
                0,
            )
            .is_ok()
                & (PowerWriteDCValueIndex(
                    None,
                    &scheme,
                    Some(&SUB_PROCESSOR),
                    Some(&PERF_BOOST_MODE),
                    0,
                ) == 0);
            let _ = PowerSetActiveScheme(None, Some(&scheme));
            ok
        }
    }
}
