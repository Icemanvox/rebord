//! La session : écoute TCP, un lecteur et un écrivain par appareil, un coordinateur unique.
//!
//! Le coordinateur reproduit les règles de l'hôte Mac (StreamCoordinator, StreamServer) :
//! catalogue à la connexion, une seule source diffusée, image clé à chaque nouvel appareil,
//! `tick` toutes les 30 ms, images en retard abandonnées plutôt qu'empilées.

use crate::input::{CmdMode, InputTranslator, gesture_actions};
use crate::log;
use crate::platform::{Action, FrameSink, Platform, StreamConfig, StreamEvent, VideoStream};
use crate::proto::msg::{
    self, ClientStats, Hello, InputEvent, StreamSource, TrackpadGesture, Viewport, WindowCommand, WindowList,
};
use crate::proto::{MAX_CLIENT_PAYLOAD, Message, kind};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_PORT: u16 = 48100;
const TICK: Duration = Duration::from_millis(30);
const CATALOG_EVERY: Duration = Duration::from_secs(1);
/// Au-delà de deux images en attente d'envoi, une image delta est abandonnée (règle de l'hôte Mac).
const MAX_INFLIGHT: usize = 2;
const KEYFRAME_REQUEST_SPACING: Duration = Duration::from_secs(1);
/// Après un échec de capture, la même source n'est pas relancée avant ce délai.
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(3);

#[derive(Clone, Debug)]
pub struct Config {
    pub name: String,
    pub bitrate: u32,
    pub max_fps: u32,
    pub max_side: u32,
    pub cmd_mode: CmdMode,
    pub door: Door,
    /// Le curseur dessiné dans la vidéo plutôt que par l'app (forme et position non envoyées).
    pub cursor_in_video: bool,
}

/// Comment l'hôte parle sur le réseau local.
#[derive(Clone)]
pub enum Door {
    /// TCP en clair : seule une build Debug de l'app s'y connecte.
    Plain,
    /// TLS, porte ouverte (`p=0`) : tout appareil, sans appairage.
    Open(Arc<rustls::ServerConfig>),
}

impl std::fmt::Debug for Door {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Door::Plain => "Plain",
            Door::Open(_) => "Open",
        })
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            name: "PC".into(),
            bitrate: 20_000_000,
            max_fps: 120,
            max_side: 3840,
            cmd_mode: CmdMode::Ctrl,
            door: Door::Plain,
            cursor_in_video: false,
        }
    }
}

enum Out {
    Control(Vec<u8>),
    Frame(Vec<u8>),
}

#[derive(Clone)]
struct Writer {
    tx: Sender<Out>,
    inflight: Arc<AtomicUsize>,
}

impl Writer {
    fn send(&self, m: &Message) {
        let _ = self.tx.send(Out::Control(m.serialize()));
    }
    fn send_frame(&self, bytes: Vec<u8>) {
        self.inflight.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(Out::Frame(bytes)).is_err() {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

enum Event {
    Connected { id: u64, writer: Writer, peer: SocketAddr },
    Received { id: u64, msg: Message },
    Closed { id: u64, why: String },
    Stream { generation: u64, event: StreamEvent },
}

struct Client {
    writer: Writer,
    peer: SocketAddr,
    name: String,
    fps: u32,
    needs_keyframe: bool,
    translator: InputTranslator,
    last_keyframe_request: Option<Instant>,
    last_stats_log: Option<Instant>,
    /// Messages d'entrée lus sur cette connexion : le `seen` des positions du pointeur.
    inputs: u64,
}

struct Active {
    source: StreamSource,
    stream: Box<dyn VideoStream>,
    generation: u64,
    fps: u32,
}

pub struct Server {
    platform: Arc<dyn Platform>,
    config: Config,
    events: Sender<Event>,
    inbox: Receiver<Event>,
    clients: BTreeMap<u64, Client>,
    source: StreamSource,
    active: Option<Active>,
    generation: u64,
    last_parameter_sets: Option<Vec<u8>>,
    windows: Vec<msg::WindowInfo>,
    launch_id: String,
    failed: Option<(StreamSource, Instant)>,
    /// Le curseur : la forme envoyée (et son message, pour les appareils qui arrivent), la
    /// dernière position annoncée, le dernier point où une entrée l'a posé.
    cursor_shape: Option<u64>,
    cursor_blob: Option<Vec<u8>>,
    pointer_sent: Option<(i64, i64, bool)>,
    last_injected: Option<(i32, i32)>,
    ignored: std::collections::BTreeSet<u8>,
}

/// Lance l'écoute sur `listeners` et rend la main seulement si le coordinateur s'arrête.
pub fn run(platform: Arc<dyn Platform>, config: Config, listeners: Vec<TcpListener>) {
    let (tx, rx) = mpsc::channel();
    let next_id = Arc::new(AtomicUsize::new(1));
    for l in listeners {
        let tx = tx.clone();
        let next_id = next_id.clone();
        let door = config.door.clone();
        thread::Builder::new()
            .name("écoute".into())
            .spawn(move || accept_loop(l, door, tx, next_id))
            .expect("thread d'écoute");
    }
    let mut server = Server::new(platform, config, tx, rx);
    server.run();
}

fn accept_loop(listener: TcpListener, door: Door, events: Sender<Event>, next_id: Arc<AtomicUsize>) {
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let id = next_id.fetch_add(1, Ordering::SeqCst) as u64;
                let door = door.clone();
                let events = events.clone();
                // Le handshake TLS se fait hors du fil d'écoute : un appareil lent n'en bloque pas d'autre.
                let spawned = thread::Builder::new().name(format!("accueil {id}")).spawn(move || {
                    if let Err(e) = admit(id, s, door, events) {
                        log!("connexion {id} refusée : {e}");
                    }
                });
                if let Err(e) = spawned {
                    log!("connexion {id} refusée : {e}");
                }
            }
            Err(e) => {
                log!("écoute : {e}");
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn admit(id: u64, stream: TcpStream, door: Door, events: Sender<Event>) -> std::io::Result<()> {
    let peer = stream.peer_addr()?;
    stream.set_nodelay(true)?;
    // Un appareil disparu (Wi-Fi coupé, veille) se remarque en une dizaine de secondes.
    let ka = socket2::TcpKeepalive::new().with_time(Duration::from_secs(5)).with_interval(Duration::from_secs(2));
    let _ = socket2::SockRef::from(&stream).set_tcp_keepalive(&ka);
    let closer = stream.try_clone()?;
    match door {
        Door::Plain => {
            let reader = stream.try_clone()?;
            start_session(id, peer, Box::new(reader), Box::new(stream), closer, events)
        }
        Door::Open(config) => {
            let a = crate::tls::accept(stream, config)?;
            if a.alpn == crate::tls::PAIRING_ALPN {
                log!(
                    "{peer} demande un appairage : pas encore géré par Rebord (la porte est ouverte, inutile d'appairer)"
                );
                let _ = closer.shutdown(Shutdown::Both);
                return Ok(());
            }
            if a.alpn != crate::tls::SESSION_ALPN {
                let _ = closer.shutdown(Shutdown::Both);
                return Err(std::io::Error::other("protocole d'application inconnu"));
            }
            start_session(id, peer, Box::new(a.reader), Box::new(a.writer), closer, events)
        }
    }
}

fn start_session(
    id: u64,
    peer: SocketAddr,
    mut reader: Box<dyn Read + Send>,
    mut write_half: Box<dyn Write + Send>,
    closer: TcpStream,
    events: Sender<Event>,
) -> std::io::Result<()> {
    let (tx, rx) = mpsc::channel::<Out>();
    let inflight = Arc::new(AtomicUsize::new(0));
    let writer = Writer { tx, inflight: inflight.clone() };
    events.send(Event::Connected { id, writer, peer }).ok();

    let closer2 = closer.try_clone()?;
    let events2 = events.clone();
    thread::Builder::new().name(format!("envoi {id}")).spawn(move || {
        for out in rx {
            let (bytes, frame) = match out {
                Out::Control(b) => (b, false),
                Out::Frame(b) => (b, true),
            };
            let result = write_half.write_all(&bytes);
            if frame {
                inflight.fetch_sub(1, Ordering::SeqCst);
            }
            if let Err(e) = result {
                // Dit tout de suite : sous Windows, la lecture bloquée ne se réveille pas toujours
                // quand la socket est fermée, et l'appareil resterait inscrit sans recevoir rien.
                events2.send(Event::Closed { id, why: format!("envoi impossible : {e}") }).ok();
                break;
            }
        }
        let _ = closer2.shutdown(Shutdown::Both);
    })?;

    thread::Builder::new().name(format!("lecture {id}")).spawn(move || {
        let why = loop {
            match Message::read_from(&mut reader, MAX_CLIENT_PAYLOAD) {
                Ok(Some(msg)) => {
                    if events.send(Event::Received { id, msg }).is_err() {
                        break "arrêt".to_string();
                    }
                }
                Ok(None) => break "fermée par l'appareil".to_string(),
                Err(e) => break e.to_string(),
            }
        };
        let _ = closer.shutdown(Shutdown::Both);
        events.send(Event::Closed { id, why }).ok();
    })?;
    Ok(())
}

impl Server {
    fn new(platform: Arc<dyn Platform>, config: Config, events: Sender<Event>, inbox: Receiver<Event>) -> Self {
        let launch_id = format!(
            "{:08X}{:08X}",
            (crate::proto::now() * 1000.0) as u64 as u32,
            std::process::id().wrapping_mul(2_654_435_761)
        );
        Server {
            platform,
            config,
            events,
            inbox,
            clients: BTreeMap::new(),
            source: StreamSource::None,
            active: None,
            generation: 0,
            last_parameter_sets: None,
            windows: Vec::new(),
            launch_id,
            failed: None,
            cursor_shape: None,
            cursor_blob: None,
            pointer_sent: None,
            last_injected: None,
            ignored: Default::default(),
        }
    }

    fn run(&mut self) {
        self.windows = self.platform.windows();
        let mut next_tick = Instant::now() + TICK;
        let mut next_catalog = Instant::now() + CATALOG_EVERY;
        loop {
            let wait = next_tick.saturating_duration_since(Instant::now());
            match self.inbox.recv_timeout(wait) {
                Ok(ev) => self.handle(ev),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            let now = Instant::now();
            if now >= next_tick {
                next_tick = now + TICK;
                if !self.clients.is_empty() {
                    let tick = Message::new(kind::TICK, Vec::new());
                    for c in self.clients.values() {
                        c.writer.send(&tick);
                    }
                    self.cursor_tick();
                }
            }
            if now >= next_catalog {
                next_catalog = now + CATALOG_EVERY;
                if !self.clients.is_empty() {
                    self.refresh_catalog();
                }
            }
        }
    }

    fn window_list(&self) -> WindowList {
        WindowList {
            mac_name: self.config.name.clone(),
            windows: self.windows.clone(),
            active: self.source,
            launch_id: Some(self.launch_id.clone()),
            host_version: Some(crate::proto::COMPAT_HOST_VERSION.into()),
            protocol: Some(crate::proto::PROTOCOL),
            gestures: Some(1),
            clipboard: None,
            key_repeat: None,
            locked: None,
            off_console: None,
        }
    }

    fn broadcast_list(&self) {
        let m = Message::new(kind::WINDOW_LIST, msg::encode(&self.window_list()));
        for c in self.clients.values() {
            c.writer.send(&m);
        }
    }

    fn refresh_catalog(&mut self) {
        let fresh = self.platform.windows();
        let changed = fresh != self.windows;
        self.windows = fresh;
        if let StreamSource::Window(id) = self.source {
            if !self.windows.iter().any(|w| w.id == id) {
                log!("la fenêtre {id} a disparu : diffusion arrêtée");
                self.stop_stream();
                self.broadcast_list();
                return;
            }
        }
        if changed {
            self.broadcast_list();
        }
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Connected { id, writer, peer } => {
                log!("appareil {id} connecté depuis {peer}");
                self.clients.insert(
                    id,
                    Client {
                        writer,
                        peer,
                        name: format!("appareil {id}"),
                        fps: 60,
                        needs_keyframe: true,
                        translator: InputTranslator::new(self.config.cmd_mode),
                        last_keyframe_request: None,
                        last_stats_log: None,
                        inputs: 0,
                    },
                );
                // Le catalogue part tout de suite : un appareil d'avant le `hello` n'en envoie pas.
                self.windows = self.platform.windows();
                let m = Message::new(kind::WINDOW_LIST, msg::encode(&self.window_list()));
                self.clients[&id].writer.send(&m);
                if let Some(blob) = &self.cursor_blob {
                    self.clients[&id].writer.send(&Message::new(kind::CURSOR_SHAPE, blob.clone()));
                }
                // Que le nouvel appareil apprenne où est le pointeur.
                self.pointer_sent = None;
            }
            Event::Closed { id, why } => {
                if let Some(mut c) = self.clients.remove(&id) {
                    let release = c.translator.release_all();
                    if !release.is_empty() {
                        self.platform.inject(&release);
                    }
                    log!("{} ({}) déconnecté : {why}", c.name, c.peer);
                }
                if self.clients.is_empty() {
                    // Rien ne se capture sans appareil connecté.
                    self.stop_stream();
                } else {
                    self.apply_fps();
                }
            }
            Event::Received { id, msg } => self.received(id, msg),
            Event::Stream { generation, event } => self.stream_event(generation, event),
        }
    }

    fn received(&mut self, id: u64, m: Message) {
        if !self.clients.contains_key(&id) {
            return;
        }
        match m.kind {
            kind::HELLO => {
                let h: Hello = msg::decode(&m.payload).unwrap_or_default();
                let c = self.clients.get_mut(&id).unwrap();
                if let Some(d) = &h.device {
                    c.name = clean_label(d);
                }
                log!(
                    "{} : app {} (build {}), protocole {}",
                    c.name,
                    h.app_version.as_deref().unwrap_or("?"),
                    h.build.as_deref().unwrap_or("?"),
                    h.protocol.map(|p| p.to_string()).unwrap_or_else(|| "?".into())
                );
            }
            kind::SELECT_SOURCE => match msg::decode::<StreamSource>(&m.payload) {
                Some(s) => self.select(id, s),
                None => log!("selectSource illisible : {}", String::from_utf8_lossy(&m.payload)),
            },
            kind::VIEWPORT => match msg::decode::<Viewport>(&m.payload) {
                Some(v) => {
                    if let Some(why) = v.refusal() {
                        log!("viewport refusé : {why}");
                        return;
                    }
                    let c = self.clients.get_mut(&id).unwrap();
                    c.fps = v.fps.unwrap_or(60).clamp(1, 240) as u32;
                    self.apply_fps();
                }
                None => log!("viewport illisible"),
            },
            kind::INPUT => match InputEvent::from_json(&m.payload) {
                Some(e) => {
                    self.clients.get_mut(&id).unwrap().inputs += 1;
                    self.input(id, &e)
                }
                None => log!("entrée illisible : {}", String::from_utf8_lossy(&m.payload)),
            },
            kind::GESTURE => {
                if let Some(g) = msg::decode::<TrackpadGesture>(&m.payload) {
                    let a = gesture_actions(&g.gesture);
                    if a.is_empty() {
                        log!("geste inconnu : {}", g.gesture);
                    } else {
                        self.platform.inject(&a);
                    }
                }
            }
            kind::PING => {
                let pong = Message::new(kind::PONG, m.payload);
                self.clients[&id].writer.send(&pong);
            }
            kind::KEYFRAME_REQUEST => {
                let c = self.clients.get_mut(&id).unwrap();
                let now = Instant::now();
                if c.last_keyframe_request.is_some_and(|t| now.duration_since(t) < KEYFRAME_REQUEST_SPACING) {
                    return;
                }
                c.last_keyframe_request = Some(now);
                if let Some(a) = &self.active {
                    log!("image clé pour {} : son image a été interrompue", c.name);
                    a.stream.request_keyframe();
                }
            }
            kind::CLIENT_STATS => {
                let c = self.clients.get_mut(&id).unwrap();
                let now = Instant::now();
                if c.last_stats_log.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(10)) {
                    c.last_stats_log = Some(now);
                    if let Some(s) = msg::decode::<ClientStats>(&m.payload) {
                        log!(
                            "{} : {} i/s, âge des images {} ms (max {}), aller-retour {} ms",
                            c.name,
                            s.fps,
                            s.frame_age_ms,
                            s.frame_age_max_ms.unwrap_or(s.frame_age_ms),
                            s.rtt_ms
                        );
                    }
                }
            }
            kind::WINDOW_COMMAND => {
                if let Some(cmd) = msg::decode::<WindowCommand>(&m.payload) {
                    let ok = self.platform.window_command(cmd.id, &cmd.action);
                    log!("fenêtre {} {} : {}", cmd.id, cmd.action, if ok { "fait" } else { "refusé" });
                    self.refresh_catalog();
                }
            }
            k => {
                // Le reste (menus, réglages, presse-papiers…) viendra aux jalons suivants ; dit une
                // fois par type.
                if self.ignored.insert(k) {
                    log!("{} ignoré : pas encore géré (dit une seule fois)", kind::name(k));
                }
            }
        }
    }

    fn select(&mut self, id: u64, source: StreamSource) {
        let who = self.clients[&id].name.clone();
        log!("{who} choisit {:?}", source);
        // Un changement de source relâche ce que cet appareil tenait.
        let release = self.clients.get_mut(&id).unwrap().translator.release_all();
        if !release.is_empty() {
            self.platform.inject(&release);
        }
        if source == StreamSource::None {
            self.stop_stream();
            self.broadcast_list();
            return;
        }
        if let StreamSource::Window(wid) = source {
            if !self.windows.iter().any(|w| w.id == wid) {
                self.windows = self.platform.windows();
                if !self.windows.iter().any(|w| w.id == wid) {
                    log!("fenêtre {wid} inconnue");
                    self.broadcast_list();
                    return;
                }
            }
            self.platform.bring_forward(source);
        }
        if self.active.as_ref().is_some_and(|a| a.source == source) {
            // Même source : cet appareil attend juste une image clé.
            self.clients.get_mut(&id).unwrap().needs_keyframe = true;
            self.active.as_ref().unwrap().stream.request_keyframe();
            self.broadcast_list();
            return;
        }
        if let Some((s, t)) = self.failed {
            if s == source && t.elapsed() < RETRY_AFTER_FAILURE {
                log!("{:?} a échoué il y a moins de {} s : pas de nouvel essai", source, RETRY_AFTER_FAILURE.as_secs());
                self.broadcast_list();
                return;
            }
        }
        self.start_stream(source);
        self.broadcast_list();
    }

    fn wanted_fps(&self) -> u32 {
        let want = self.clients.values().map(|c| c.fps).max().unwrap_or(60);
        want.clamp(24, self.config.max_fps.max(24))
    }

    fn apply_fps(&mut self) {
        let fps = self.wanted_fps();
        if let Some(a) = &mut self.active {
            if a.fps != fps {
                log!("cadence : {} → {} i/s", a.fps, fps);
                a.fps = fps;
                a.stream.set_fps(fps);
            }
        }
    }

    fn start_stream(&mut self, source: StreamSource) {
        self.stop_stream();
        self.generation += 1;
        let generation = self.generation;
        let fps = self.wanted_fps();
        let cfg = StreamConfig {
            fps,
            // Le débit de l'hôte Mac est donné pour 60 i/s.
            bitrate: (self.config.bitrate as u64 * fps.max(60) as u64 / 60).min(200_000_000) as u32,
            max_side: self.config.max_side,
            cursor_in_video: self.config.cursor_in_video,
        };
        let events = self.events.clone();
        let sink: FrameSink = Box::new(move |event| {
            let _ = events.send(Event::Stream { generation, event });
        });
        match self.platform.start_stream(source, cfg, sink) {
            Ok(stream) => {
                log!("diffusion de {:?} à {} i/s, {} Mbit/s", source, fps, cfg.bitrate / 1_000_000);
                for c in self.clients.values_mut() {
                    c.needs_keyframe = true;
                }
                self.source = source;
                self.pointer_sent = None;
                self.active = Some(Active { source, stream, generation, fps });
                self.failed = None;
            }
            Err(e) => {
                log!("impossible de diffuser {:?} : {e}", source);
                self.failed = Some((source, Instant::now()));
                self.source = StreamSource::None;
            }
        }
    }

    fn stop_stream(&mut self) {
        if let Some(a) = self.active.take() {
            log!("diffusion de {:?} arrêtée", a.source);
        }
        self.source = StreamSource::None;
        self.last_parameter_sets = None;
    }

    fn stream_event(&mut self, generation: u64, event: StreamEvent) {
        if self.active.as_ref().map(|a| a.generation) != Some(generation) {
            return; // une diffusion déjà remplacée
        }
        match event {
            StreamEvent::Failed(e) => {
                let source = self.source;
                log!("la diffusion de {:?} a échoué : {e}", source);
                self.stop_stream();
                self.failed = Some((source, Instant::now()));
                self.broadcast_list();
            }
            StreamEvent::Frame(au) => {
                let now = crate::proto::now();
                if let Some(ps) = &au.parameter_sets {
                    let m =
                        Message { kind: kind::PARAMETER_SETS, timestamp: now, is_keyframe: true, payload: ps.encode() };
                    self.last_parameter_sets = Some(m.serialize());
                }
                let frame = Message { kind: kind::FRAME, timestamp: now, is_keyframe: au.keyframe, payload: au.data }
                    .serialize();
                let mut want_keyframe = false;
                for c in self.clients.values_mut() {
                    if au.keyframe {
                        // Chaque image clé répète les ensembles de paramètres, comme sur Mac.
                        if let Some(ps) = &self.last_parameter_sets {
                            let _ = c.writer.tx.send(Out::Control(ps.clone()));
                        } else if c.needs_keyframe {
                            continue; // sans eux, l'appareil ne saurait pas décoder
                        }
                        c.writer.send_frame(frame.clone());
                        c.needs_keyframe = false;
                    } else if c.needs_keyframe {
                        // Une delta sans sa clé ne se décode pas : on attend la prochaine clé.
                        want_keyframe = true;
                    } else if c.writer.inflight.load(Ordering::SeqCst) > MAX_INFLIGHT {
                        // Le lien ne suit pas : la delta est abandonnée, et toutes les suivantes
                        // jusqu'à une clé (envoyer quand même faisait scintiller l'iPad).
                        c.needs_keyframe = true;
                        want_keyframe = true;
                    } else {
                        c.writer.send_frame(frame.clone());
                    }
                }
                if want_keyframe {
                    if let Some(a) = &self.active {
                        a.stream.request_keyframe();
                    }
                }
            }
        }
    }

    fn input(&mut self, id: u64, e: &InputEvent) {
        let Some(rect) = self.platform.source_rect(self.source) else {
            // Rien n'est diffusé : seuls les relâchements partent, pour ne rien laisser enfoncé.
            let c = self.clients.get_mut(&id).unwrap();
            if matches!(e, InputEvent::Key { down: false, .. })
                || matches!(
                    e,
                    InputEvent::Pointer {
                        action: crate::proto::msg::PointerAction::LeftUp | crate::proto::msg::PointerAction::RightUp,
                        ..
                    }
                )
            {
                let a = c.translator.release_all();
                self.platform.inject(&a);
            }
            return;
        };
        let interacting = matches!(
            e,
            InputEvent::Key { down: true, .. }
                | InputEvent::Text(_)
                | InputEvent::Pointer {
                    action: crate::proto::msg::PointerAction::LeftDown | crate::proto::msg::PointerAction::RightDown,
                    ..
                }
        );
        if interacting {
            if let StreamSource::Window(_) = self.source {
                self.platform.bring_forward(self.source);
            }
        }
        let actions = self.clients.get_mut(&id).unwrap().translator.translate(e, &rect);
        if let Some(&Action::MoveTo { x, y }) = actions.iter().rev().find(|a| matches!(a, Action::MoveTo { .. })) {
            self.last_injected = Some((x, y));
        }
        if !actions.is_empty() {
            self.platform.inject(&actions);
        }
    }

    /// À chaque tick : la forme du curseur quand elle change (message 14), et sa position quand la
    /// souris du PC l'a bougée (message 26). Un mouvement venu d'un appareil n'est pas renvoyé :
    /// l'app dessine déjà son propre pointeur, ou aucun sous un doigt.
    fn cursor_tick(&mut self) {
        if self.config.cursor_in_video {
            return;
        }
        let Some(c) = self.platform.cursor() else { return };
        if c.visible && self.cursor_shape != Some(c.shape) {
            self.cursor_shape = Some(c.shape);
            if let Some(img) = self.platform.cursor_image() {
                let blob = img.blob();
                let m = Message::new(kind::CURSOR_SHAPE, blob.clone());
                for cl in self.clients.values() {
                    cl.writer.send(&m);
                }
                self.cursor_blob = Some(blob);
            }
        }
        let ours =
            self.last_injected.is_some_and(|(x, y)| (x as f64 - c.x).abs() <= 1.0 && (y as f64 - c.y).abs() <= 1.0);
        if ours {
            // L'appareil mène : ce qu'on a annoncé avant ne vaut plus.
            self.pointer_sent = None;
            return;
        }
        self.last_injected = None;
        let Some(rect) = self.platform.source_rect(self.source) else { return };
        let fx = (c.x - rect.x) / rect.w;
        let fy = (c.y - rect.y) / rect.h;
        let inside = c.visible && (0.0..=1.0).contains(&fx) && (0.0..=1.0).contains(&fy);
        let key =
            if inside { ((fx * 10_000.0).round() as i64, (fy * 10_000.0).round() as i64, true) } else { (0, 0, false) };
        if self.pointer_sent == Some(key) {
            return;
        }
        self.pointer_sent = Some(key);
        for cl in self.clients.values() {
            let mut v = serde_json::json!({"inside": inside, "seen": cl.inputs});
            if inside {
                v["x"] = serde_json::json!(key.0 as f64 / 10_000.0);
                v["y"] = serde_json::json!(key.1 as f64 / 10_000.0);
            }
            cl.writer.send(&Message::new(kind::MAC_POINTER, serde_json::to_vec(&v).unwrap()));
        }
    }
}

/// Un nom d'appareil pour le journal : sans caractères de contrôle, 64 caractères au plus.
fn clean_label(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect()
}
