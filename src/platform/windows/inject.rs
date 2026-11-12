//! Injection d'entrées (SendInput), premier plan et commandes de fenêtre.

use crate::log;
use crate::platform::Action;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Marque nos événements, pour les reconnaître s'il le faut un jour.
const TAG: usize = 0x5245_424F; // "REBO"

fn virtual_desk() -> (i32, i32, i32, i32) {
    unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        )
    }
}

fn mouse(dx: i32, dy: i32, data: i32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: TAG },
        },
    }
}

fn keyboard(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: TAG },
        },
    }
}

static WARNED_BLOCKED: AtomicBool = AtomicBool::new(false);

pub fn send(actions: &[Action]) {
    let (vx, vy, vw, vh) = virtual_desk();
    let mut inputs = Vec::with_capacity(actions.len());
    for a in actions {
        match *a {
            Action::MoveTo { x, y } => {
                // Coordonnées normalisées 0…65535 sur le bureau virtuel.
                let nx = ((x - vx) as f64 * 65535.0 / (vw - 1).max(1) as f64).round() as i32;
                let ny = ((y - vy) as f64 * 65535.0 / (vh - 1).max(1) as f64).round() as i32;
                inputs.push(mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK));
            }
            Action::Button { right, down } => {
                let f = match (right, down) {
                    (false, true) => MOUSEEVENTF_LEFTDOWN,
                    (false, false) => MOUSEEVENTF_LEFTUP,
                    (true, true) => MOUSEEVENTF_RIGHTDOWN,
                    (true, false) => MOUSEEVENTF_RIGHTUP,
                };
                inputs.push(mouse(0, 0, 0, f));
            }
            Action::Wheel { vertical, horizontal } => {
                if vertical != 0 {
                    inputs.push(mouse(0, 0, vertical, MOUSEEVENTF_WHEEL));
                }
                if horizontal != 0 {
                    inputs.push(mouse(0, 0, horizontal, MOUSEEVENTF_HWHEEL));
                }
            }
            Action::Key { scan, extended, down } => {
                let mut f = KEYEVENTF_SCANCODE;
                if extended {
                    f |= KEYEVENTF_EXTENDEDKEY;
                }
                if !down {
                    f |= KEYEVENTF_KEYUP;
                }
                inputs.push(keyboard(0, scan, f));
            }
            Action::Unicode { unit, down } => {
                let mut f = KEYEVENTF_UNICODE;
                if !down {
                    f |= KEYEVENTF_KEYUP;
                }
                inputs.push(keyboard(0, unit, f));
            }
        }
    }
    if inputs.is_empty() {
        return;
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if (sent as usize) < inputs.len() && !WARNED_BLOCKED.swap(true, Ordering::SeqCst) {
        log!(
            "SendInput : {sent}/{} événements acceptés. Une fenêtre lancée en administrateur ne reçoit rien \
             d'un programme qui ne l'est pas (UIPI), et le bureau sécurisé (UAC, verrouillage) non plus.",
            inputs.len()
        );
    }
}

/// Met la fenêtre au premier plan, en contournant le verrou de premier plan de Windows par
/// l'attachement temporaire au fil de la fenêtre active.
pub fn bring_forward(h: HWND) {
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let fg = GetForegroundWindow();
        if fg == h {
            return;
        }
        // AttachThreadInput exige une file de messages : ce fil n'en a pas tant qu'il n'a rien lu.
        let mut msg = MSG::default();
        let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        let me = GetCurrentThreadId();
        let theirs = if fg.is_invalid() { 0 } else { GetWindowThreadProcessId(fg, None) };
        let attached = theirs != 0 && theirs != me && AttachThreadInput(me, theirs, true).as_bool();
        let _ = BringWindowToTop(h);
        let _ = SetForegroundWindow(h);
        if attached {
            let _ = AttachThreadInput(me, theirs, false);
        }
        if GetForegroundWindow() != h {
            // Dernier recours : une frappe injectée lève le verrou de premier plan.
            send(&[
                Action::Key { scan: 0x38, extended: false, down: true },
                Action::Key { scan: 0x38, extended: false, down: false },
            ]);
            let _ = SetForegroundWindow(h);
        }
    }
}

pub fn window_command(h: HWND, action: &str) -> bool {
    unsafe {
        match action {
            "close" => PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok(),
            "minimize" => {
                let _ = ShowWindow(h, SW_MINIMIZE);
                true
            }
            "fullScreen" => {
                let _ = ShowWindow(h, if IsZoomed(h).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
                true
            }
            _ => false,
        }
    }
}
