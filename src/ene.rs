//! The ENE KB5130 lighting controller (I2C HID, ACPI id ENEK5130) on 2025 Predator models such as the PHN16-73.
//! These ignore the WMI lighting calls; keyboard, lid logo and mode key are driven by one 11-byte feature
//! report, 0xA4, on usage page 0xFF5A. Fields (usages), as Acer's AcerLightingService fills them:
//! 0x21 device, 0x41 effect, 0x42 speed, 0x43 brightness 0-100, 0x44 direction, 0x45-0x47 R/G/B,
//! 0x48 zone mask (16 bit). The report is built with HidP_SetUsageValue, so the byte order comes from
//! the device's own descriptor.

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces,
    SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HidD_FreePreparsedData, HidD_GetHidGuid, HidD_GetPreparsedData, HidD_SetFeature,
    HidP_Feature, HidP_GetCaps, HidP_InitializeReportForID, HidP_SetUsageValue,
    PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::core::{GUID, PCWSTR};

pub const KEYBOARD: u8 = 0x21;
pub const LID_LOGO: u8 = 0x83;
pub const MODE_KEY: u8 = 0x65;

pub const KEYBOARD_ZONES: u16 = 0x0F;

pub mod effect {
    pub const STATIC: u8 = 0x02;
    pub const BREATHING: u8 = 0x04;
    pub const NEON: u8 = 0x05;
    pub const WAVE: u8 = 0x07;
    pub const SHIFTING: u8 = 0x08;
    pub const ZOOM: u8 = 0x09;
    pub const METEOR: u8 = 0x0A;
    pub const TWINKLING: u8 = 0x0B;
}

const USAGE_PAGE: u16 = 0xFF5A;
const REPORT_ID: u8 = 0xA4;
const HIDP_STATUS_SUCCESS: i32 = 0x0011_0000;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Report {
    pub device: u8,
    pub effect: u8,
    pub speed: u8,
    pub brightness: u8,
    pub direction: u8,
    pub rgb: [u8; 3],
    pub zones: u16,
}

pub struct Ene {
    handle: HANDLE,
    preparsed: PHIDP_PREPARSED_DATA,
    length: usize,
}

unsafe impl Send for Ene {}

impl Ene {
    pub fn open() -> Option<Self> {
        unsafe {
            let guid: GUID = HidD_GetHidGuid();
            let set = SetupDiGetClassDevsW(
                Some(&guid),
                PCWSTR::null(),
                None,
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
            .ok()?;
            let mut found = None;
            let mut iface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            let mut i = 0;
            while SetupDiEnumDeviceInterfaces(set, None, &guid, i, &mut iface).is_ok() {
                i += 1;
                if let Some(path) = device_path(set, &iface)
                    && let Some(ene) = Self::try_open(&path)
                {
                    found = Some(ene);
                    break;
                }
            }
            let _ = SetupDiDestroyDeviceInfoList(set);
            found
        }
    }

    unsafe fn try_open(path: &[u16]) -> Option<Self> {
        unsafe {
            // Look at every HID device with query access first: keyboards and mice refuse read/write opens.
            let q = CreateFileW(
                PCWSTR(path.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
            .ok()?;
            let page_ok = caps(q).is_some_and(|c| c.UsagePage == USAGE_PAGE);
            let _ = CloseHandle(q);
            if !page_ok {
                return None;
            }

            let h = CreateFileW(
                PCWSTR(path.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            )
            .ok()?;
            let mut pp = PHIDP_PREPARSED_DATA::default();
            if !HidD_GetPreparsedData(h, &mut pp) {
                let _ = CloseHandle(h);
                return None;
            }
            let mut c = HIDP_CAPS::default();
            let ok = HidP_GetCaps(pp, &mut c).0 == HIDP_STATUS_SUCCESS
                && c.FeatureReportByteLength >= 11;
            let ene = Self {
                handle: h,
                preparsed: pp,
                length: c.FeatureReportByteLength as usize,
            };
            // The page alone is vendor-wide; report 0xA4 with a zone field marks the lighting controller.
            if !ok || ene.build(&Report::default()).is_none() {
                return None; // Drop closes the handle.
            }
            Some(ene)
        }
    }

    fn build(&self, r: &Report) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; self.length];
        unsafe {
            if HidP_InitializeReportForID(HidP_Feature, REPORT_ID, self.preparsed, &mut buf).0
                != HIDP_STATUS_SUCCESS
            {
                return None;
            }
            let fields: [(u16, u32); 9] = [
                (0x21, r.device as u32),
                (0x41, r.effect as u32),
                (0x42, r.speed as u32),
                (0x43, r.brightness as u32),
                (0x44, r.direction as u32),
                (0x45, r.rgb[0] as u32),
                (0x46, r.rgb[1] as u32),
                (0x47, r.rgb[2] as u32),
                (0x48, r.zones as u32),
            ];
            for (usage, value) in fields {
                if HidP_SetUsageValue(
                    HidP_Feature,
                    USAGE_PAGE,
                    None,
                    usage,
                    value,
                    self.preparsed,
                    &mut buf,
                )
                .0 != HIDP_STATUS_SUCCESS
                {
                    return None;
                }
            }
        }
        Some(buf)
    }

    pub fn send(&self, r: &Report) -> bool {
        let Some(buf) = self.build(r) else {
            return false;
        };
        unsafe { HidD_SetFeature(self.handle, buf.as_ptr() as _, buf.len() as u32) }
    }
}

impl Drop for Ene {
    fn drop(&mut self) {
        unsafe {
            if self.preparsed.0 != 0 {
                let _ = HidD_FreePreparsedData(self.preparsed);
            }
            let _ = CloseHandle(self.handle);
        }
    }
}

unsafe fn caps(h: HANDLE) -> Option<HIDP_CAPS> {
    unsafe {
        let mut pp = PHIDP_PREPARSED_DATA::default();
        if !HidD_GetPreparsedData(h, &mut pp) {
            return None;
        }
        let mut c = HIDP_CAPS::default();
        let ok = HidP_GetCaps(pp, &mut c).0 == HIDP_STATUS_SUCCESS;
        let _ = HidD_FreePreparsedData(pp);
        ok.then_some(c)
    }
}

unsafe fn device_path(
    set: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
    iface: &SP_DEVICE_INTERFACE_DATA,
) -> Option<Vec<u16>> {
    unsafe {
        let mut size = 0u32;
        let _ = SetupDiGetDeviceInterfaceDetailW(set, iface, None, 0, Some(&mut size), None);
        if size == 0 {
            return None;
        }
        // u64 storage keeps the struct aligned.
        let mut storage = vec![0u64; (size as usize).div_ceil(8)];
        let detail = storage.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        SetupDiGetDeviceInterfaceDetailW(set, iface, Some(detail), size, None, None).ok()?;
        let p = (*detail).DevicePath.as_ptr();
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        let mut path = std::slice::from_raw_parts(p, len).to_vec();
        path.push(0);
        Some(path)
    }
}
