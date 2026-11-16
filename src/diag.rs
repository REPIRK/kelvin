//! A small log file, %LOCALAPPDATA%\Kelvin\log.txt: firmware writes and WMI errors, so a report of
//! "X does nothing" can be checked against what was sent and what came back. Cut at 256 KB.

use std::io::Write;

use windows::Win32::System::SystemInformation::GetLocalTime;

pub fn log(line: impl AsRef<str>) {
    let path = crate::settings::data_dir().join("log.txt");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::rename(&path, path.with_extension("old.txt"));
    }
    let t = unsafe { GetLocalTime() };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(
            f,
            "{:02}-{:02} {:02}:{:02}:{:02} {}",
            t.wMonth,
            t.wDay,
            t.wHour,
            t.wMinute,
            t.wSecond,
            line.as_ref()
        );
    }
}
