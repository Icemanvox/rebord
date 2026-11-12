//! Les fenêtres proposées à l'app : celles que montrerait Alt+Tab, dans un ordre stable.

use crate::platform::Rect;
use crate::proto::msg::WindowInfo;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Mutex;
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::Storage::FileSystem::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForWindow, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{BOOL, PCWSTR, PWSTR};

pub fn hwnd_of(id: u32) -> HWND {
    HWND(id as usize as *mut c_void)
}

fn id_of(h: HWND) -> u32 {
    h.0 as usize as u32
}

#[derive(Clone)]
struct App {
    name: String,
    id: String,
}

#[derive(Default)]
pub struct Catalog {
    apps: Mutex<HashMap<u32, App>>,
    order: Mutex<Vec<u32>>,
}

unsafe extern "system" fn collect(h: HWND, l: LPARAM) -> BOOL {
    let v = unsafe { &mut *(l.0 as *mut Vec<HWND>) };
    v.push(h);
    true.into()
}

fn title(h: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(h);
        if n <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; n as usize + 1];
        let got = GetWindowTextW(h, &mut buf);
        String::from_utf16_lossy(&buf[..got.max(0) as usize])
    }
}

fn class(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn cloaked(h: HWND) -> bool {
    let mut v = 0u32;
    unsafe { DwmGetWindowAttribute(h, DWMWA_CLOAKED, &mut v as *mut u32 as *mut c_void, 4).is_ok() && v != 0 }
}

/// Le cadre visible de la fenêtre (sans l'ombre), en pixels physiques.
pub fn frame_bounds(h: HWND) -> Option<RECT> {
    let mut r = RECT::default();
    unsafe {
        if DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut RECT as *mut c_void,
            std::mem::size_of::<RECT>() as u32,
        )
        .is_err()
        {
            GetWindowRect(h, &mut r).ok()?;
        }
    }
    Some(r)
}

pub fn window_scale(h: HWND) -> f64 {
    let dpi = unsafe { GetDpiForWindow(h) };
    if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 }
}

/// Une fenêtre qu'Alt+Tab montrerait.
fn is_candidate(h: HWND, own_pid: u32) -> bool {
    unsafe {
        if !IsWindowVisible(h).as_bool() || cloaked(h) {
            return false;
        }
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE) as u32;
        let app_window = ex & WS_EX_APPWINDOW.0 != 0;
        if (ex & WS_EX_TOOLWINDOW.0 != 0 || ex & WS_EX_NOACTIVATE.0 != 0) && !app_window {
            return false;
        }
        if let Ok(owner) = GetWindow(h, GW_OWNER) {
            if !owner.is_invalid() && !app_window {
                return false;
            }
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == own_pid {
            return false;
        }
    }
    if title(h).trim().is_empty() {
        return false;
    }
    !matches!(
        class(h).as_str(),
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "Windows.UI.Core.CoreWindow"
    )
}

fn exe_path(pid: u32) -> Option<String> {
    unsafe {
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = vec![0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len).is_ok();
        let _ = CloseHandle(p);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

/// La description du fichier (« Bloc-notes ») tirée de ses informations de version.
fn file_description(path: &str) -> Option<String> {
    let wpath: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let size = GetFileVersionInfoSizeW(PCWSTR(wpath.as_ptr()), None);
        if size == 0 {
            return None;
        }
        let mut data = vec![0u8; size as usize];
        GetFileVersionInfoW(PCWSTR(wpath.as_ptr()), None, size, data.as_mut_ptr() as *mut c_void).ok()?;
        let query = |q: &str| -> Option<(*mut c_void, u32)> {
            let wq: Vec<u16> = q.encode_utf16().chain(Some(0)).collect();
            let mut p: *mut c_void = std::ptr::null_mut();
            let mut n = 0u32;
            VerQueryValueW(data.as_ptr() as *const c_void, PCWSTR(wq.as_ptr()), &mut p, &mut n)
                .as_bool()
                .then_some((p, n))
        };
        let (tp, tn) = query("\\VarFileInfo\\Translation")?;
        let mut langs: Vec<(u16, u16)> = Vec::new();
        if tn >= 4 {
            let t = std::slice::from_raw_parts(tp as *const u16, (tn / 2) as usize);
            for pair in t.chunks(2) {
                if pair.len() == 2 {
                    langs.push((pair[0], pair[1]));
                }
            }
        }
        langs.push((0x0409, 0x04B0));
        for (lang, cp) in langs {
            if let Some((p, n)) = query(&format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription")) {
                if n > 1 {
                    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p as *const u16, n as usize - 1));
                    let s = s.trim().to_string();
                    if !s.is_empty() {
                        return Some(s);
                    }
                }
            }
        }
    }
    None
}

impl Catalog {
    fn app(&self, pid: u32, window_title: &str) -> App {
        if let Some(a) = self.apps.lock().unwrap().get(&pid) {
            return a.clone();
        }
        let path = exe_path(pid);
        let stem = path
            .as_deref()
            .and_then(|p| std::path::Path::new(p).file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| format!("pid {pid}"));
        let name = if stem.eq_ignore_ascii_case("ApplicationFrameHost") {
            // Une app du Store : son hôte ne dit rien, le titre dit tout.
            window_title.to_string()
        } else {
            path.as_deref().and_then(file_description).unwrap_or_else(|| stem.clone())
        };
        let app = App { name, id: path.unwrap_or(stem).to_lowercase() };
        self.apps.lock().unwrap().insert(pid, app.clone());
        app
    }

    pub fn list(&self) -> Vec<WindowInfo> {
        let mut handles: Vec<HWND> = Vec::new();
        unsafe {
            let _ = EnumWindows(Some(collect), LPARAM(&mut handles as *mut Vec<HWND> as isize));
        }
        let own = std::process::id();
        let mut out = Vec::new();
        let mut live_pids = Vec::new();
        for h in handles {
            if !is_candidate(h, own) {
                continue;
            }
            let t = title(h);
            let mut pid = 0u32;
            unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
            live_pids.push(pid);
            let app = self.app(pid, &t);
            let scale = window_scale(h);
            let (w, hgt) = if unsafe { IsIconic(h).as_bool() } {
                let mut wp =
                    WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
                unsafe {
                    let _ = GetWindowPlacement(h, &mut wp);
                }
                let r = wp.rcNormalPosition;
                (r.right - r.left, r.bottom - r.top)
            } else {
                let Some(r) = frame_bounds(h) else { continue };
                (r.right - r.left, r.bottom - r.top)
            };
            if w < 2 || hgt < 2 {
                continue;
            }
            out.push(WindowInfo {
                id: id_of(h),
                title: t,
                app_name: app.name,
                bundle_id: app.id,
                width: (w as f64 / scale).round() as i64,
                height: (hgt as f64 / scale).round() as i64,
            });
        }
        self.apps.lock().unwrap().retain(|pid, _| live_pids.contains(pid));
        // Ordre stable : la première fois vue d'abord, comme sur Mac.
        let mut order = self.order.lock().unwrap();
        order.retain(|id| out.iter().any(|w| w.id == *id));
        for w in &out {
            if !order.contains(&w.id) {
                order.push(w.id);
            }
        }
        out.sort_by_key(|w| order.iter().position(|id| *id == w.id).unwrap_or(usize::MAX));
        out
    }

    /// La fenêtre de cet identifiant, si elle existe toujours et reste proposable.
    pub fn window(&self, id: u32) -> Option<HWND> {
        let h = hwnd_of(id);
        (unsafe { IsWindow(Some(h)).as_bool() } && is_candidate(h, std::process::id())).then_some(h)
    }
}

pub fn window_rect(h: HWND) -> Option<Rect> {
    if unsafe { IsIconic(h).as_bool() } {
        return None;
    }
    let r = frame_bounds(h)?;
    Some(Rect {
        x: r.left as f64,
        y: r.top as f64,
        w: (r.right - r.left) as f64,
        h: (r.bottom - r.top) as f64,
        scale: window_scale(h),
    })
}

pub fn primary_monitor() -> (HMONITOR, Rect) {
    unsafe {
        let m = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(m, &mut info);
        let r = info.rcMonitor;
        let (mut dx, mut dy) = (96u32, 96u32);
        let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        (
            m,
            Rect {
                x: r.left as f64,
                y: r.top as f64,
                w: (r.right - r.left) as f64,
                h: (r.bottom - r.top) as f64,
                scale: dx as f64 / 96.0,
            },
        )
    }
}
