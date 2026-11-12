//! Ce que le serveur demande au système : fenêtres, capture encodée, injection d'entrées.
//! `windows` est l'implémentation réelle ; `synthetic` sert aux tests et au diagnostic.

pub mod synthetic;
#[cfg(windows)]
pub mod windows;

use crate::proto::hevc::AccessUnit;
use crate::proto::msg::{StreamSource, WindowInfo};

/// Rectangle d'une source à l'écran, en pixels physiques du bureau virtuel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// Facteur d'échelle de l'écran (DPI / 96).
    pub scale: f64,
}

/// Une action d'entrée élémentaire, prête pour SendInput.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    MoveTo {
        x: i32,
        y: i32,
    },
    Button {
        right: bool,
        down: bool,
    },
    /// En unités WHEEL_DELTA (120 par cran) ; positif = haut / droite.
    Wheel {
        vertical: i32,
        horizontal: i32,
    },
    /// Code de balayage (jeu 1), avec le préfixe E0 noté par `extended`.
    Key {
        scan: u16,
        extended: bool,
        down: bool,
    },
    /// Une unité UTF-16 tapée telle quelle.
    Unicode {
        unit: u16,
        down: bool,
    },
}

/// Le pointeur du système, relevé à chaque tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorState {
    /// Position en pixels physiques du bureau virtuel.
    pub x: f64,
    pub y: f64,
    pub visible: bool,
    /// Identifie la forme courante : sa poignée sous Windows. Change quand la forme change.
    pub shape: u64,
}

/// L'image d'un curseur, telle que l'app la dessine (CursorShapeBlob) : point chaud et taille en
/// points de l'hôte, image PNG en pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct CursorImage {
    pub hot_x: f32,
    pub hot_y: f32,
    pub width: f32,
    pub height: f32,
    pub png: Vec<u8>,
}

impl CursorImage {
    /// La charge du message 14 : hotX, hotY, largeur, hauteur en Float32 big-endian, puis le PNG.
    pub fn blob(&self) -> Vec<u8> {
        let mut d = Vec::with_capacity(16 + self.png.len());
        for v in [self.hot_x, self.hot_y, self.width, self.height] {
            d.extend_from_slice(&v.to_bits().to_be_bytes());
        }
        d.extend_from_slice(&self.png);
        d
    }
}

/// Encode une image RGBA 8 bits en PNG.
pub fn rgba_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, width, height);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let mut w = e.write_header().ok()?;
        w.write_image_data(rgba).ok()?;
    }
    Some(out)
}

#[derive(Clone, Copy, Debug)]
pub struct StreamConfig {
    pub fps: u32,
    pub bitrate: u32,
    /// Plus grand côté encodé, en pixels.
    pub max_side: u32,
    /// Le curseur dessiné dans la vidéo, plutôt que par l'app.
    pub cursor_in_video: bool,
}

pub enum StreamEvent {
    Frame(AccessUnit),
    Failed(String),
}

pub type FrameSink = Box<dyn Fn(StreamEvent) + Send + Sync + 'static>;

/// Une capture encodée en cours. La lâcher l'arrête.
pub trait VideoStream: Send {
    fn request_keyframe(&self);
    fn set_fps(&self, fps: u32);
}

pub trait Platform: Send + Sync + 'static {
    /// Le nom montré dans l'app.
    fn host_name(&self) -> String;
    /// Les fenêtres proposées, dans un ordre stable.
    fn windows(&self) -> Vec<WindowInfo>;
    fn source_rect(&self, source: StreamSource) -> Option<Rect>;
    /// Met la fenêtre au premier plan (et la restaure si elle est réduite).
    fn bring_forward(&self, source: StreamSource);
    fn start_stream(
        &self,
        source: StreamSource,
        cfg: StreamConfig,
        sink: FrameSink,
    ) -> Result<Box<dyn VideoStream>, String>;
    fn inject(&self, actions: &[Action]);
    /// "close", "minimize" ou "fullScreen" ; faux si refusé.
    fn window_command(&self, id: u32, action: &str) -> bool;
    /// Le pointeur du système, s'il y en a un.
    fn cursor(&self) -> Option<CursorState> {
        None
    }
    /// L'image du curseur courant.
    fn cursor_image(&self) -> Option<CursorImage> {
        None
    }
}

/// Annonce Bonjour `_sill._tcp` par le répondeur de Windows (`p=0` si `tls`, sinon porte en
/// clair). L'objet rendu retire l'annonce quand on le lâche.
pub fn advertise(name: &str, port: u16, tls: bool) -> Result<Box<dyn Send>, String> {
    #[cfg(windows)]
    {
        windows::mdns::advertise(name, port, tls)
    }
    #[cfg(not(windows))]
    {
        let _ = (name, port, tls);
        Err("annonce Bonjour disponible seulement sous Windows".into())
    }
}
