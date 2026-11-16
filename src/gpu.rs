//! The NVIDIA GPU from Windows' side: which processes hold it (they keep it awake on battery), and the
//! per-app "power saving GPU" preference that moves an app to the integrated GPU on its next start.
//! Reading the GPU Engine counters does not wake the GPU; nothing here talks to the NVIDIA driver.

use std::collections::BTreeMap;

use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
    PdhAddEnglishCounterW, PdhCollectQueryData, PdhGetFormattedCounterArrayW, PdhOpenQueryW,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_SZ, RRF_RT_REG_DWORD, RegGetValueW, RegSetKeyValueW,
};
use windows::core::{HSTRING, PCWSTR, w};

/// "luid_0x{high:08x}_0x{low:08x}" of the NVIDIA adapter, as the GPU Engine counter names write it.
pub fn nvidia_luid() -> Option<String> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut i = 0;
        while let Ok(a) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(d) = a.GetDesc1() else { continue };
            if d.VendorId == 0x10DE {
                return Some(format!(
                    "luid_0x{:08x}_0x{:08x}",
                    d.AdapterLuid.HighPart as u32, d.AdapterLuid.LowPart
                ));
            }
        }
        None
    }
}

pub struct GpuUsers {
    query: PDH_HQUERY,
    counter: PDH_HCOUNTER,
    luid: String,
}

unsafe impl Send for GpuUsers {}

impl GpuUsers {
    pub fn new() -> Option<Self> {
        let luid = nvidia_luid()?;
        unsafe {
            let mut query = PDH_HQUERY::default();
            if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0 {
                return None;
            }
            let mut counter = PDH_HCOUNTER::default();
            if PdhAddEnglishCounterW(
                query,
                w!("\\GPU Engine(*)\\Utilization Percentage"),
                0,
                &mut counter,
            ) != 0
            {
                return None;
            }
            let _ = PdhCollectQueryData(query);
            Some(Self {
                query,
                counter,
                luid,
            })
        }
    }

    /// Process ids holding the NVIDIA GPU, with their summed engine use in percent since the last call.
    pub fn poll(&mut self) -> BTreeMap<u32, f64> {
        let mut out = BTreeMap::new();
        unsafe {
            if PdhCollectQueryData(self.query) != 0 {
                return out;
            }
            let (mut size, mut count) = (0u32, 0u32);
            if PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                None,
            ) != PDH_MORE_DATA
            {
                return out;
            }
            let mut buf = vec![0u8; size as usize];
            let items = buf.as_mut_ptr() as *mut PDH_FMT_COUNTERVALUE_ITEM_W;
            if PdhGetFormattedCounterArrayW(
                self.counter,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                Some(items),
            ) != 0
            {
                return out;
            }
            for k in 0..count as usize {
                let item = &*items.add(k);
                let Ok(name) = item.szName.to_string() else {
                    continue;
                };
                if !name.contains(&self.luid) {
                    continue;
                }
                let Some(pid) = name
                    .strip_prefix("pid_")
                    .and_then(|r| r.split('_').next())
                    .and_then(|p| p.parse::<u32>().ok())
                else {
                    continue;
                };
                let v = if item.FmtValue.CStatus == 0 {
                    item.FmtValue.Anonymous.doubleValue
                } else {
                    0.0
                };
                *out.entry(pid).or_insert(0.0) += v;
            }
        }
        out
    }
}

/// Puts an app on the integrated GPU (Windows' per-app graphics preference, "Power saving").
/// Takes effect when the app next starts.
pub fn prefer_integrated(exe_path: &str) -> bool {
    unsafe {
        let data: Vec<u16> = "GpuPreference=1;".encode_utf16().chain([0]).collect();
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\DirectX\\UserGpuPreferences"),
            &HSTRING::from(exe_path),
            REG_SZ.0,
            Some(data.as_ptr() as _),
            (data.len() * 2) as u32,
        )
        .is_ok()
    }
}

/// Acer's own record of which GPU modes this model offers: bit 0 Optimus, bit 1 NVIDIA only, bit 2 auto.
pub fn mode_capability() -> Option<u32> {
    unsafe {
        let mut v = 0u32;
        let mut size = 4u32;
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\OEM\\AcerAgentService\\AdvanceSettings"),
            w!("dgpu_mode_capability"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut v as *mut _ as _),
            Some(&mut size),
        )
        .is_ok()
        .then_some(v)
    }
}
