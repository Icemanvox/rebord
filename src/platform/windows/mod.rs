//! La plateforme Windows réelle.

mod capture;
mod catalog;
mod convert;
mod cursor;
mod encoder;
mod inject;
pub mod mdns;
mod stream;

use super::{Action, FrameSink, Platform, Rect, StreamConfig, VideoStream};
use crate::log;
use crate::proto::msg::{StreamSource, WindowInfo};
use capture::{Gpu, Mailbox, Target};
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::RECT;
use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext};
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

pub struct WindowsPlatform {
    catalog: catalog::Catalog,
    gpu: Mutex<Option<Arc<Gpu>>>,
    /// La source diffusée et sa boîte à images, pour connaître la taille réellement capturée.
    current: Mutex<Option<(StreamSource, Mailbox)>>,
}

impl WindowsPlatform {
    pub fn new() -> Result<Self, String> {
        unsafe {
            // Pixels physiques partout : capture, rectangles et SendInput parlent la même langue.
            if SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2).is_err() {
                log!("DPI : le processus n'a pas pu passer en Per-Monitor V2");
            }
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| format!("MFStartup : {e}"))?;
        }
        Ok(WindowsPlatform { catalog: catalog::Catalog::default(), gpu: Mutex::new(None), current: Mutex::new(None) })
    }

    fn gpu(&self) -> Result<Arc<Gpu>, String> {
        let mut g = self.gpu.lock().unwrap();
        if g.is_none() {
            *g = Some(Arc::new(capture::create_gpu()?));
        }
        Ok(g.as_ref().unwrap().clone())
    }

    fn captured_size(&self, source: StreamSource) -> Option<(f64, f64)> {
        let cur = self.current.lock().unwrap();
        let (s, mb) = cur.as_ref()?;
        if *s != source {
            return None;
        }
        let l = mb.0.lock().unwrap();
        (l.seq > 0).then_some((l.width as f64, l.height as f64))
    }
}

impl Platform for WindowsPlatform {
    fn host_name(&self) -> String {
        mdns::computer_name()
    }

    fn windows(&self) -> Vec<WindowInfo> {
        self.catalog.list()
    }

    fn source_rect(&self, source: StreamSource) -> Option<Rect> {
        match source {
            StreamSource::None => None,
            StreamSource::Desktop => Some(catalog::primary_monitor().1),
            StreamSource::Window(id) => {
                let h = self.catalog.window(id)?;
                let mut r = catalog::window_rect(h)?;
                // Selon les versions de Windows, la capture d'une fenêtre suit son cadre visible ou
                // son rectangle complet (bordures invisibles comprises) : on prend celui dont la
                // taille correspond à l'image réellement capturée.
                if let Some((cw, ch)) = self.captured_size(source) {
                    let mut full = RECT::default();
                    if unsafe { GetWindowRect(h, &mut full) }.is_ok() {
                        let fw = (full.right - full.left) as f64;
                        let fh = (full.bottom - full.top) as f64;
                        if (fw - cw).abs() + (fh - ch).abs() < (r.w - cw).abs() + (r.h - ch).abs() {
                            r.x = full.left as f64;
                            r.y = full.top as f64;
                        }
                    }
                    r.w = cw;
                    r.h = ch;
                }
                Some(r)
            }
        }
    }

    fn bring_forward(&self, source: StreamSource) {
        if let StreamSource::Window(id) = source {
            if let Some(h) = self.catalog.window(id) {
                inject::bring_forward(h);
            }
        }
    }

    fn start_stream(
        &self,
        source: StreamSource,
        cfg: StreamConfig,
        sink: FrameSink,
    ) -> Result<Box<dyn VideoStream>, String> {
        let target = match source {
            StreamSource::None => return Err("aucune source".into()),
            StreamSource::Desktop => Target::Monitor(catalog::primary_monitor().0),
            StreamSource::Window(id) => Target::Window(self.catalog.window(id).ok_or("fenêtre introuvable")?),
        };
        let gpu = self.gpu()?;
        let mailbox = capture::mailbox();
        let shared = Arc::new(stream::Shared {
            stop: AtomicBool::new(false),
            keyframe: AtomicBool::new(false),
            fps: AtomicU32::new(cfg.fps),
        });
        let capture = match capture::start(&gpu, target, 240, cfg.cursor_in_video, mailbox.clone()) {
            Ok(c) => c,
            Err(e) => {
                // Un périphérique perdu (pilote mis à jour, veille) : le prochain essai en recrée un.
                *self.gpu.lock().unwrap() = None;
                return Err(e);
            }
        };
        let (g, m, s) = (gpu.clone(), mailbox.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("encodage".into())
            .spawn(move || stream::run(g, m, s, cfg, sink))
            .map_err(|e| e.to_string())?;
        *self.current.lock().unwrap() = Some((source, mailbox.clone()));
        Ok(Box::new(stream::WinStream { shared, mailbox, capture: Some(capture), thread: Some(thread) }))
    }

    fn inject(&self, actions: &[Action]) {
        inject::send(actions);
    }

    fn window_command(&self, id: u32, action: &str) -> bool {
        self.catalog.window(id).map(|h| inject::window_command(h, action)).unwrap_or(false)
    }

    fn cursor(&self) -> Option<super::CursorState> {
        cursor::state()
    }

    fn cursor_image(&self) -> Option<super::CursorImage> {
        cursor::image()
    }
}
