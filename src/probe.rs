//! Un client minimal qui parle comme l'app iOS : pour les tests, et pour vérifier un hôte à
//! distance (`rebord --probe hôte:port`), en enregistrant la vidéo reçue en Annex B lisible par
//! ffmpeg.

use crate::proto::msg::{self, Hello, InputEvent, StreamSource, Viewport, WindowList};
use crate::proto::{Message, ParameterSets, kind};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

pub trait Io: Read + Write + Send {}
impl<T: Read + Write + Send> Io for T {}

pub struct Probe {
    pub stream: Box<dyn Io>,
}

impl Probe {
    /// En clair, comme une build Debug de l'app devant une porte sans `p`.
    pub fn connect(addr: &str) -> std::io::Result<Probe> {
        let stream = Self::tcp(addr)?;
        Ok(Probe { stream: Box::new(stream) })
    }

    /// En TLS `sill/1`, comme l'app devant une porte ouverte (`p=0`) : clé de l'hôte acceptée telle
    /// quelle, certificat éphémère présenté.
    pub fn connect_tls(addr: &str) -> std::io::Result<Probe> {
        let stream = Self::tcp(addr)?;
        let id = crate::tls::Identity::ephemeral("Sonde Rebord").map_err(std::io::Error::other)?;
        let config = crate::tls::client_config(&id, crate::tls::SESSION_ALPN).map_err(std::io::Error::other)?;
        Ok(Probe { stream: Box::new(crate::tls::connect(stream, config)?) })
    }

    fn tcp(addr: &str) -> std::io::Result<TcpStream> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        Ok(stream)
    }

    pub fn send(&mut self, k: u8, payload: Vec<u8>) -> std::io::Result<()> {
        self.stream.write_all(&Message::new(k, payload).serialize())?;
        self.stream.flush()
    }

    pub fn hello(&mut self) -> std::io::Result<()> {
        let h = Hello {
            app_version: Some("0.6.1".into()),
            build: Some("probe".into()),
            protocol: Some(1),
            device: Some("Sonde Rebord".into()),
            audio: None,
        };
        self.send(kind::HELLO, msg::encode(&h))
    }

    pub fn viewport(&mut self, w: f64, h: f64, fps: i64) -> std::io::Result<()> {
        let v = Viewport { width: w, height: h, scale: None, local_cursor: None, fps: Some(fps) };
        self.send(kind::VIEWPORT, msg::encode(&v))
    }

    pub fn select(&mut self, s: StreamSource) -> std::io::Result<()> {
        self.send(kind::SELECT_SOURCE, msg::encode(&s))
    }

    pub fn input(&mut self, e: &InputEvent) -> std::io::Result<()> {
        self.send(kind::INPUT, e.to_json())
    }

    pub fn read(&mut self) -> std::io::Result<Message> {
        Message::read_from(&mut self.stream, 64 << 20)?
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connexion fermée"))
    }

    /// Lit jusqu'au premier message qui satisfait `pred`, dans la limite de `timeout`.
    pub fn until(&mut self, timeout: Duration, mut pred: impl FnMut(&Message) -> bool) -> std::io::Result<Message> {
        let end = Instant::now() + timeout;
        loop {
            if Instant::now() > end {
                return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "message attendu non reçu"));
            }
            let m = self.read()?;
            if pred(&m) {
                return Ok(m);
            }
        }
    }

    pub fn window_list(&mut self, timeout: Duration) -> std::io::Result<WindowList> {
        let m = self.until(timeout, |m| m.kind == kind::WINDOW_LIST)?;
        msg::decode(&m.payload)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "liste de fenêtres illisible"))
    }
}

/// Repasse une image (NAL préfixées de 4 octets) en Annex B.
pub fn frame_to_annexb(data: &[u8], out: &mut Vec<u8>) -> bool {
    let mut off = 0;
    while off + 4 <= data.len() {
        let len = u32::from_be_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        if off + len > data.len() {
            return false;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[off..off + len]);
        off += len;
    }
    off == data.len()
}

/// `rebord --probe hôte:port [secondes] [fichier]` : se connecte comme l'app, demande le bureau,
/// enregistre la vidéo reçue et résume ce qui est arrivé.
pub fn run_cli(addr: &str, plain: bool, seconds: f64, out_path: &str) -> Result<(), String> {
    let e = |x: std::io::Error| x.to_string();
    let mut p = if plain { Probe::connect(addr) } else { Probe::connect_tls(addr) }.map_err(e)?;
    println!("Connecté {}", if plain { "en clair" } else { "en TLS 1.3 (sill/1)" });
    p.hello().map_err(e)?;
    let list = p.window_list(Duration::from_secs(5)).map_err(e)?;
    println!("Hôte « {} », version {:?}, {} fenêtre(s) :", list.mac_name, list.host_version, list.windows.len());
    for w in &list.windows {
        println!("  {:>10}  {} — {} ({}×{})", w.id, w.app_name, w.title, w.width, w.height);
    }
    p.viewport(1180.0, 820.0, 60).map_err(e)?;
    p.select(StreamSource::Desktop).map_err(e)?;
    let mut annexb = Vec::new();
    let (mut frames, mut keys, mut bytes, mut ticks) = (0u32, 0u32, 0usize, 0u32);
    let mut first_frame: Option<Instant> = None;
    let start = Instant::now();
    let mut ping_sent: Option<Instant> = None;
    while start.elapsed().as_secs_f64() < seconds {
        let m = p.read().map_err(e)?;
        match m.kind {
            kind::PARAMETER_SETS => {
                let ps = ParameterSets::decode(&m.payload).ok_or("ensembles de paramètres illisibles")?;
                for s in &ps.sets {
                    annexb.extend_from_slice(&[0, 0, 0, 1]);
                    annexb.extend_from_slice(s);
                }
            }
            kind::FRAME => {
                if first_frame.is_none() {
                    first_frame = Some(Instant::now());
                    println!("Première image après {} ms", start.elapsed().as_millis());
                }
                frames += 1;
                keys += m.is_keyframe as u32;
                bytes += m.payload.len();
                if !frame_to_annexb(&m.payload, &mut annexb) {
                    return Err("image mal découpée (préfixes de longueur incohérents)".into());
                }
                if frames == 10 {
                    ping_sent = Some(Instant::now());
                    p.send(kind::PING, 0f64.to_bits().to_be_bytes().to_vec()).map_err(e)?;
                }
            }
            kind::PONG => {
                if let Some(t) = ping_sent.take() {
                    println!("Aller-retour : {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
                }
            }
            kind::TICK => ticks += 1,
            kind::WINDOW_LIST => {
                if let Some(l) = msg::decode::<WindowList>(&m.payload) {
                    println!("Source active : {:?}", l.active);
                }
            }
            _ => {}
        }
    }
    let span = first_frame.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0).max(0.001);
    println!(
        "{frames} images ({keys} clés) en {:.1} s : {:.0} i/s, {:.1} Mbit/s ; {ticks} ticks",
        span,
        frames as f64 / span,
        bytes as f64 * 8.0 / span / 1e6
    );
    std::fs::write(out_path, &annexb).map_err(|x| x.to_string())?;
    println!("Vidéo enregistrée dans {out_path} (lisible par ffplay ou VLC).");
    Ok(())
}
