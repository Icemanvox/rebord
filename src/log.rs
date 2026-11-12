//! Journal minimal : console et, si configuré, un fichier.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn to_file(path: &std::path::Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(f) = OpenOptions::new().create(true).append(true).open(path) {
        *FILE.lock().unwrap() = Some(f);
    }
}

#[cfg(windows)]
fn stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    )
}

#[cfg(not(windows))]
fn stamp() -> String {
    let secs = crate::proto::now();
    let day = secs % 86_400.0;
    format!("{:02}:{:02}:{:06.3} UTC", (day / 3600.0) as u32, ((day % 3600.0) / 60.0) as u32, day % 60.0)
}

pub fn line(text: &str) {
    let stamp = stamp();
    let l = format!("{stamp} {text}");
    eprintln!("{l}");
    if let Some(f) = FILE.lock().unwrap().as_mut() {
        let _ = writeln!(f, "{l}");
    }
}

#[macro_export]
macro_rules! log {
    ($($t:tt)*) => { $crate::log::line(&format!($($t)*)) };
}
