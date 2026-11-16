//! Small Windows facts and actions: AC power, idle time, local time, processes, refresh rate, the startup task.

use std::collections::HashSet;
use std::os::windows::process::CommandExt;
use std::process::Command;

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Graphics::Gdi::{
    CDS_TEST, CDS_UPDATEREGISTRY, ChangeDisplaySettingsW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
    DM_BITSPERPEL, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH, ENUM_CURRENT_SETTINGS,
    ENUM_DISPLAY_SETTINGS_MODE, EnumDisplaySettingsW,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::core::PCWSTR;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Some(true) on AC, Some(false) on battery, None when Windows does not know.
pub fn on_ac() -> Option<bool> {
    let mut s = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut s).ok()? };
    match s.ACLineStatus {
        1 => Some(true),
        0 => Some(false),
        _ => None,
    }
}

/// Other programs that drive the same fans and lights: Predator Control (C#, and this app before its rename) and PredatorSense.
pub fn rivals() -> Vec<(u32, String)> {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    const NAMES: [&str; 3] = [
        "predatorcontrol.exe",
        "predatorcontrolapp.exe",
        "predatorsense.exe",
    ];
    let me = unsafe { GetCurrentProcessId() };
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(e.szExeFile.len());
                let name = String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase();
                if e.th32ProcessID != me && NAMES.contains(&name.as_str()) {
                    out.push((
                        e.th32ProcessID,
                        process_path(e.th32ProcessID).unwrap_or(name),
                    ));
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

pub fn process_path(pid: u32) -> Option<String> {
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            h,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

pub fn terminate(pid: u32) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) else {
            return false;
        };
        let ok = TerminateProcess(h, 0).is_ok();
        let _ = CloseHandle(h);
        ok
    }
}

pub fn idle_seconds() -> u32 {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return 0;
        }
        GetTickCount().wrapping_sub(info.dwTime) / 1000
    }
}

pub fn local_minute() -> u16 {
    let t = unsafe { GetLocalTime() };
    t.wHour * 60 + t.wMinute
}

/// Lower-case executable names of running processes.
pub fn process_names() -> HashSet<String> {
    let mut out = HashSet::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e
                    .szExeFile
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(e.szExeFile.len());
                out.insert(String::from_utf16_lossy(&e.szExeFile[..len]).to_lowercase());
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

fn devmode() -> DEVMODEW {
    DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    }
}

fn current_mode() -> Option<DEVMODEW> {
    let mut dm = devmode();
    unsafe {
        EnumDisplaySettingsW(PCWSTR::null(), ENUM_CURRENT_SETTINGS, &mut dm)
            .as_bool()
            .then_some(dm)
    }
}

fn same_geometry(a: &DEVMODEW, b: &DEVMODEW) -> bool {
    a.dmPelsWidth == b.dmPelsWidth
        && a.dmPelsHeight == b.dmPelsHeight
        && a.dmBitsPerPel == b.dmBitsPerPel
}

fn modes() -> Vec<DEVMODEW> {
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        let mut dm = devmode();
        if !unsafe { EnumDisplaySettingsW(PCWSTR::null(), ENUM_DISPLAY_SETTINGS_MODE(i), &mut dm) }
            .as_bool()
        {
            break;
        }
        out.push(dm);
        i += 1;
    }
    out
}

pub fn current_hz() -> u32 {
    current_mode().map(|m| m.dmDisplayFrequency).unwrap_or(60)
}

/// Highest refresh rate at the current resolution.
pub fn max_hz() -> u32 {
    let Some(cur) = current_mode() else { return 60 };
    modes()
        .iter()
        .filter(|m| same_geometry(m, &cur))
        .map(|m| m.dmDisplayFrequency)
        .max()
        .unwrap_or(60)
        .max(60)
}

pub fn set_hz(hz: u32) -> bool {
    let Some(cur) = current_mode() else {
        return false;
    };
    if cur.dmDisplayFrequency == hz {
        return true;
    }
    let Some(mut target) = modes()
        .into_iter()
        .find(|m| same_geometry(m, &cur) && m.dmDisplayFrequency == hz)
    else {
        return false;
    };
    target.dmSize = size_of::<DEVMODEW>() as u16;
    target.dmFields = DM_BITSPERPEL | DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY;
    unsafe {
        ChangeDisplaySettingsW(Some(&target), CDS_TEST) == DISP_CHANGE_SUCCESSFUL
            && ChangeDisplaySettingsW(Some(&target), CDS_UPDATEREGISTRY) == DISP_CHANGE_SUCCESSFUL
    }
}

const TASK: &str = "Kelvin";

fn schtasks(args: &[&str]) -> Option<std::process::Output> {
    Command::new("schtasks.exe")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .ok()
}

/// The logon task (shared name with the C# app it replaces) and whether it starts this exe.
pub fn startup_state() -> (bool, bool) {
    match schtasks(&["/Query", "/TN", TASK, "/XML"]) {
        Some(o) if o.status.success() => {
            let xml = String::from_utf8_lossy(&o.stdout).to_lowercase();
            let exe = std::env::current_exe()
                .map(|p| p.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            // schtasks prints UTF-16 when redirected on some systems; compare without the NULs.
            let xml: String = xml.chars().filter(|&c| c != '\0').collect();
            (true, xml.contains(&exe))
        }
        _ => (false, false),
    }
}

/// The logon task of the app this one replaces (Predator Control, C# and the Rust build before its
/// rename). Two apps starting at logon would fight over the fans.
pub fn remove_legacy_task() -> bool {
    let exists =
        schtasks(&["/Query", "/TN", "PredatorControl"]).is_some_and(|o| o.status.success());
    exists
        && schtasks(&["/Delete", "/TN", "PredatorControl", "/F"])
            .is_some_and(|o| o.status.success())
}

pub fn set_startup(enable: bool) -> bool {
    if !enable {
        return schtasks(&["/Delete", "/TN", TASK, "/F"]).is_some_and(|o| o.status.success());
    }
    let exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let user = std::env::var("USERDOMAIN")
        .map(|d| format!("{d}\\{}", std::env::var("USERNAME").unwrap_or_default()))
        .unwrap_or_default();
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts Kelvin at logon with administrator rights.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId><Delay>PT10S</Delay></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>false</AllowHardTerminate>
    <StartWhenAvailable>false</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>-hidden</Arguments></Exec></Actions>
</Task>"#,
        user = esc(&user),
        exe = esc(&exe)
    );
    let path = std::env::temp_dir().join("KelvinStartup.xml");
    let mut bytes = vec![0xFF, 0xFE];
    for u in xml.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    if std::fs::write(&path, bytes).is_err() {
        return false;
    }
    let ok = schtasks(&[
        "/Create",
        "/TN",
        TASK,
        "/XML",
        &path.to_string_lossy(),
        "/F",
    ])
    .is_some_and(|o| o.status.success());
    let _ = std::fs::remove_file(&path);
    ok
}

/// Executable names of processes that own a visible top-level window (the Game Sync picker).
pub fn windowed_process_names() -> Vec<String> {
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible,
    };
    use windows::core::BOOL;

    unsafe extern "system" fn each(hwnd: HWND, lp: LPARAM) -> BOOL {
        unsafe {
            if IsWindowVisible(hwnd).as_bool() {
                let mut pid = 0;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                (*(lp.0 as *mut HashSet<u32>)).insert(pid);
            }
        }
        true.into()
    }

    let mut pids: HashSet<u32> = HashSet::new();
    unsafe {
        let _ = EnumWindows(Some(each), LPARAM(&mut pids as *mut _ as isize));
    }
    let mut names: Vec<String> = pids
        .into_iter()
        .filter_map(|pid| unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buf = [0u16; 520];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(
                h,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(h);
            if !ok {
                return None;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            let name = path.rsplit('\\').next()?.to_string();
            let lower = path.to_lowercase();
            (!lower.contains("\\windows\\")).then_some(name)
        })
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup_by_key(|n| n.to_lowercase());
    names
}

pub fn restart_windows() {
    let _ = Command::new("shutdown.exe")
        .args(["/r", "/t", "0"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// Tick count of the last keyboard or mouse input in the session.
pub fn last_input_tick() -> u32 {
    let mut info = LASTINPUTINFO {
        cbSize: size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    unsafe {
        let _ = GetLastInputInfo(&mut info);
    }
    info.dwTime
}

/// Turns the display off the way Windows' own display timeout does. Apps keep running.
pub fn display_off() {
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, PostMessageW, SC_MONITORPOWER, WM_SYSCOMMAND,
    };
    unsafe {
        // Posted, not sent: a hung window must not stall the caller.
        let _ = PostMessageW(
            Some(HWND_BROADCAST),
            WM_SYSCOMMAND,
            WPARAM(SC_MONITORPOWER as usize),
            LPARAM(2),
        );
    }
}

/// Keeps the machine from sleeping on idle while the screen is off on purpose. Per calling thread.
pub fn keep_awake(on: bool) {
    use windows::Win32::System::Power::{
        ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
    };
    unsafe {
        let _ = SetThreadExecutionState(if on {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        });
    }
}
