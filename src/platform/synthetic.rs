//! Une plateforme sans écran : deux fenêtres fictives, une mire HEVC enregistrée, des entrées
//! notées au lieu d'être injectées. Sert aux tests sous Linux et, sous Windows, à vérifier la
//! liaison avec l'app sans dépendre de la capture ni de l'encodeur (`--synthetic`).

use super::{Action, FrameSink, Platform, Rect, StreamConfig, StreamEvent, VideoStream};
use crate::proto::hevc;
use crate::proto::msg::{StreamSource, WindowInfo};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Une courte mire HEVC (Annex B, sans images B, une image clé en tête), produite par
/// `tools/make-test-pattern.sh`.
static TEST_PATTERN: &[u8] = include_bytes!("../../assets/test-pattern.hevc");

pub struct Synthetic {
    pub injected: Arc<Mutex<Vec<Action>>>,
    pub windows: Mutex<Vec<WindowInfo>>,
    /// Le pointeur simulé : les mouvements injectés le déplacent, un test peut le bouger « à la
    /// souris du PC ».
    pub cursor: Mutex<super::CursorState>,
}

impl Synthetic {
    pub fn new() -> Self {
        let w = |id, title: &str, app: &str| WindowInfo {
            id,
            title: title.into(),
            app_name: app.into(),
            bundle_id: format!("synthetic.{}", app.to_lowercase()),
            width: 800,
            height: 600,
        };
        Synthetic {
            cursor: Mutex::new(super::CursorState { x: 5.0, y: 5.0, visible: true, shape: 1 }),
            injected: Arc::new(Mutex::new(Vec::new())),
            windows: Mutex::new(vec![
                w(101, "Sans titre - Bloc-notes", "Bloc-notes"),
                w(102, "Calculatrice", "Calculatrice"),
            ]),
        }
    }
}

impl Default for Synthetic {
    fn default() -> Self {
        Self::new()
    }
}

/// Les unités d'accès de la mire, en boucle.
pub fn pattern_units() -> Vec<hevc::AccessUnit> {
    // Une unité commence à chaque NAL d'image précédée d'un délimiteur ou d'ensembles de paramètres :
    // ici, la mire n'a qu'une tranche par image, donc chaque NAL d'image ferme une unité.
    let mut units = Vec::new();
    let mut pending: Vec<u8> = Vec::new();
    for nal in hevc::split_annexb(TEST_PATTERN) {
        pending.extend_from_slice(&[0, 0, 0, 1]);
        pending.extend_from_slice(nal);
        if hevc::nal_type(nal).map(|t| t < 32).unwrap_or(false) {
            if let Some(au) = hevc::from_annexb(&pending) {
                units.push(au);
            }
            pending.clear();
        }
    }
    units
}

struct SyntheticStream {
    stop: Arc<AtomicBool>,
    keyframe: Arc<AtomicBool>,
    fps: Arc<AtomicU32>,
}

impl VideoStream for SyntheticStream {
    fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::SeqCst);
    }
    fn set_fps(&self, fps: u32) {
        self.fps.store(fps.max(1), Ordering::SeqCst);
    }
}

impl Drop for SyntheticStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Platform for Synthetic {
    fn host_name(&self) -> String {
        "Rebord (mire de test)".into()
    }

    fn windows(&self) -> Vec<WindowInfo> {
        self.windows.lock().unwrap().clone()
    }

    fn source_rect(&self, source: StreamSource) -> Option<Rect> {
        match source {
            StreamSource::None => None,
            StreamSource::Desktop => Some(Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0, scale: 1.0 }),
            StreamSource::Window(id) => {
                let known = self.windows.lock().unwrap().iter().any(|w| w.id == id);
                known.then_some(Rect { x: 100.0, y: 100.0, w: 800.0, h: 600.0, scale: 1.0 })
            }
        }
    }

    fn bring_forward(&self, _source: StreamSource) {}

    fn start_stream(
        &self,
        _source: StreamSource,
        cfg: StreamConfig,
        sink: FrameSink,
    ) -> Result<Box<dyn VideoStream>, String> {
        let units = pattern_units();
        if units.is_empty() || !units[0].keyframe {
            return Err("mire de test illisible".into());
        }
        let stop = Arc::new(AtomicBool::new(false));
        let keyframe = Arc::new(AtomicBool::new(false));
        let fps = Arc::new(AtomicU32::new(cfg.fps.clamp(1, 120)));
        let (s, k, f) = (stop.clone(), keyframe.clone(), fps.clone());
        thread::Builder::new()
            .name("mire".into())
            .spawn(move || {
                let mut i = 0usize;
                while !s.load(Ordering::SeqCst) {
                    // Une image clé demandée, ou la fin de la mire : on repart de son début, qui en est une.
                    if k.swap(false, Ordering::SeqCst) || i >= units.len() {
                        i = 0;
                    }
                    let u = &units[i];
                    sink(StreamEvent::Frame(hevc::AccessUnit {
                        parameter_sets: u.parameter_sets.clone(),
                        data: u.data.clone(),
                        keyframe: u.keyframe,
                    }));
                    i += 1;
                    thread::sleep(Duration::from_secs_f64(1.0 / f.load(Ordering::SeqCst) as f64));
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Box::new(SyntheticStream { stop, keyframe, fps }))
    }

    fn inject(&self, actions: &[Action]) {
        for a in actions {
            if let Action::MoveTo { x, y } = *a {
                let mut c = self.cursor.lock().unwrap();
                c.x = x as f64;
                c.y = y as f64;
            }
        }
        self.injected.lock().unwrap().extend_from_slice(actions);
    }

    fn cursor(&self) -> Option<super::CursorState> {
        Some(*self.cursor.lock().unwrap())
    }

    fn cursor_image(&self) -> Option<super::CursorImage> {
        // Une flèche de 2×2 points, assez pour les tests.
        let rgba = [0u8, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255, 0, 0, 0, 0];
        Some(super::CursorImage { hot_x: 0.0, hot_y: 0.0, width: 2.0, height: 2.0, png: super::rgba_png(2, 2, &rgba)? })
    }

    fn window_command(&self, id: u32, action: &str) -> bool {
        let mut w = self.windows.lock().unwrap();
        if action == "close" {
            let before = w.len();
            w.retain(|x| x.id != id);
            return w.len() != before;
        }
        w.iter().any(|x| x.id == id)
    }
}
