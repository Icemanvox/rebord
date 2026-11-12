//! Une session complète, comme l'app la mène, contre la plateforme synthétique.

use rebord::platform::synthetic::Synthetic;
use rebord::platform::{Action, Platform};
use rebord::probe::{Probe, frame_to_annexb};
use rebord::proto::hevc;
use rebord::proto::msg::{InputEvent, PointerAction, StreamSource};
use rebord::proto::{ParameterSets, kind};
use rebord::server::{self, Config, Door};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

fn start() -> (Arc<Synthetic>, String) {
    start_with(Door::Plain)
}

fn open_door() -> Door {
    let id = rebord::tls::Identity::ephemeral("Banc d'essai").unwrap();
    Door::Open(rebord::tls::server_config(&id).unwrap())
}

fn start_with(door: Door) -> (Arc<Synthetic>, String) {
    let platform = Arc::new(Synthetic::new());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let p: Arc<dyn Platform> = platform.clone();
    let config = Config { name: "Banc d'essai".into(), door, ..Config::default() };
    thread::spawn(move || server::run(p, config, vec![listener]));
    (platform, addr)
}

fn wait_for(platform: &Synthetic, pred: impl Fn(&[Action]) -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        if pred(&platform.injected.lock().unwrap()) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn full_session() {
    let (platform, addr) = start();
    session(platform, Probe::connect(&addr).unwrap());
}

#[test]
fn full_session_over_tls() {
    let (platform, addr) = start_with(open_door());
    session(platform, Probe::connect_tls(&addr).unwrap());
}

#[test]
fn tls_door_refuses_plain_and_certificateless_clients() {
    let (_platform, addr) = start_with(open_door());
    // Un client en clair : l'hôte n'y voit pas de TLS et ferme, sans rien envoyer de Sill.
    let mut s = TcpStream::connect(&addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let hello = rebord::proto::Message::new(kind::HELLO, b"{}".to_vec()).serialize();
    s.write_all(&hello).unwrap();
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap_or(0);
    assert!(n == 0 || buf[0] == 0x15, "pas de réponse Sill à un client en clair");
    // Un client TLS sans certificat : refusé pendant le handshake.
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoCheck))
        .with_no_client_auth();
    let mut config = config;
    config.alpn_protocols = vec![b"sill/1".to_vec()];
    let tcp = TcpStream::connect(&addr).unwrap();
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let r = rebord::tls::connect(tcp, Arc::new(config)).and_then(|mut t| {
        t.write_all(&hello)?;
        t.flush()?;
        let mut b = [0u8; 16];
        let n = t.read(&mut b)?;
        if n == 0 { Err(std::io::ErrorKind::UnexpectedEof.into()) } else { Ok(()) }
    });
    assert!(r.is_err(), "un appareil sans certificat ne doit pas entrer");
}

#[derive(Debug)]
struct NoCheck;
impl rustls::client::danger::ServerCertVerifier for NoCheck {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

fn session(platform: Arc<Synthetic>, mut p: Probe) {
    p.hello().unwrap();

    // 1. Le catalogue arrive sans rien demander.
    let list = p.window_list(Duration::from_secs(2)).unwrap();
    assert_eq!(list.mac_name, "Banc d'essai");
    assert_eq!(list.windows.len(), 2);
    assert_eq!(list.active, StreamSource::None);
    assert_eq!(list.host_version.as_deref(), Some("0.6.1"));
    assert_eq!(list.protocol, Some(1));
    assert_eq!(list.gestures, Some(1));

    // 2. Le bureau demandé : liste à jour, puis paramètres et image clé, dans cet ordre.
    p.viewport(1180.0, 820.0, 60).unwrap();
    p.select(StreamSource::Desktop).unwrap();
    let list = p.window_list(Duration::from_secs(2)).unwrap();
    assert_eq!(list.active, StreamSource::Desktop);
    let ps = p.until(Duration::from_secs(2), |m| m.kind == kind::PARAMETER_SETS || m.kind == kind::FRAME).unwrap();
    assert_eq!(ps.kind, kind::PARAMETER_SETS, "les paramètres précèdent la première image");
    assert!(ps.is_keyframe);
    let sets = ParameterSets::decode(&ps.payload).unwrap();
    assert_eq!(sets.nal_length_size, 4);
    let types: Vec<u8> = sets.sets.iter().map(|s| hevc::nal_type(s).unwrap()).collect();
    assert_eq!(types, vec![32, 33, 34]);
    let key = p.read().unwrap();
    assert_eq!(key.kind, kind::FRAME);
    assert!(key.is_keyframe);
    let mut annexb = Vec::new();
    assert!(frame_to_annexb(&key.payload, &mut annexb));
    let first = hevc::split_annexb(&annexb)[0];
    assert!(hevc::is_irap(hevc::nal_type(first).unwrap()));

    // 3. Des images delta suivent, et le tick toutes les 30 ms.
    let delta = p.until(Duration::from_secs(2), |m| m.kind == kind::FRAME).unwrap();
    assert!(!delta.is_keyframe);
    p.until(Duration::from_secs(1), |m| m.kind == kind::TICK).unwrap();

    // 4. Ping : la même charge revient.
    let stamp = 1234.5f64.to_bits().to_be_bytes().to_vec();
    p.send(kind::PING, stamp.clone()).unwrap();
    let pong = p.until(Duration::from_secs(2), |m| m.kind == kind::PONG).unwrap();
    assert_eq!(pong.payload, stamp);

    // 5. Un clic au centre de l'image tombe au centre de l'écran (1920×1080).
    p.input(&InputEvent::Pointer { action: PointerAction::LeftDown, x: 0.5, y: 0.5 }).unwrap();
    assert!(wait_for(&platform, |a| a
        .ends_with(&[Action::MoveTo { x: 960, y: 540 }, Action::Button { right: false, down: true }])));

    // 6. Une image clé demandée arrive, précédée des paramètres.
    p.send(kind::KEYFRAME_REQUEST, Vec::new()).unwrap();
    let again = p.until(Duration::from_secs(3), |m| m.kind == kind::PARAMETER_SETS).unwrap();
    assert!(again.is_keyframe);
    let next = p.read().unwrap();
    assert!(next.kind == kind::FRAME && next.is_keyframe);

    // 7. Changer de source relâche le bouton tenu et passe à la fenêtre.
    p.select(StreamSource::Window(101)).unwrap();
    let list = p.until(Duration::from_secs(2), |m| m.kind == kind::WINDOW_LIST).unwrap();
    let list: rebord::proto::msg::WindowList = rebord::proto::msg::decode(&list.payload).unwrap();
    assert_eq!(list.active, StreamSource::Window(101));
    assert!(wait_for(&platform, |a| a.last() == Some(&Action::Button { right: false, down: false })));

    // 8. Une fenêtre inconnue n'arrête pas ce qui est diffusé.
    p.select(StreamSource::Window(999)).unwrap();
    let list = p.window_list(Duration::from_secs(2)).unwrap();
    assert_eq!(list.active, StreamSource::Window(101));

    // 9. Une touche tenue à la déconnexion est relâchée.
    p.input(&InputEvent::Key { hid_usage: 0x04, down: true, modifiers: 0 }).unwrap();
    assert!(wait_for(&platform, |a| a.last() == Some(&Action::Key { scan: 0x1E, extended: false, down: true })));
    drop(p);
    assert!(wait_for(&platform, |a| a.last() == Some(&Action::Key { scan: 0x1E, extended: false, down: false })));
}

#[test]
fn a_second_device_gets_its_own_keyframe() {
    let (_platform, addr) = start();
    let mut a = Probe::connect(&addr).unwrap();
    a.hello().unwrap();
    a.window_list(Duration::from_secs(2)).unwrap();
    a.select(StreamSource::Desktop).unwrap();
    a.until(Duration::from_secs(2), |m| m.kind == kind::FRAME && m.is_keyframe).unwrap();
    // Laisser passer des deltas, puis brancher un second appareil sur la même source.
    for _ in 0..5 {
        a.until(Duration::from_secs(2), |m| m.kind == kind::FRAME && !m.is_keyframe).unwrap();
    }
    let mut b = Probe::connect(&addr).unwrap();
    b.hello().unwrap();
    b.window_list(Duration::from_secs(2)).unwrap();
    b.select(StreamSource::Desktop).unwrap();
    // Sa première image est une clé, jamais une delta qu'il ne pourrait pas décoder.
    let first = b.until(Duration::from_secs(3), |m| m.kind == kind::FRAME).unwrap();
    assert!(first.is_keyframe);
}

#[test]
fn malformed_and_oversized_messages() {
    let (_platform, addr) = start();
    let mut p = Probe::connect(&addr).unwrap();
    p.window_list(Duration::from_secs(2)).unwrap();
    // JSON illisible : ignoré, la session continue.
    p.send(kind::INPUT, b"{pas du json".to_vec()).unwrap();
    p.send(kind::VIEWPORT, br#"{"width":1e300,"height":1}"#.to_vec()).unwrap();
    p.send(77, vec![1, 2, 3]).unwrap();
    let stamp = vec![9u8; 8];
    p.send(kind::PING, stamp.clone()).unwrap();
    assert_eq!(p.until(Duration::from_secs(2), |m| m.kind == kind::PONG).unwrap().payload, stamp);
    // Une longueur annoncée de 2 Mio : l'hôte ferme.
    use std::io::Write;
    let mut h = vec![kind::INPUT];
    h.extend_from_slice(&0f64.to_bits().to_be_bytes());
    h.push(0);
    h.extend_from_slice(&(2u32 << 20).to_be_bytes());
    p.stream.write_all(&h).unwrap();
    p.stream.flush().unwrap();
    let end = Instant::now() + Duration::from_secs(3);
    let closed = loop {
        match p.read() {
            Err(_) => break true,
            Ok(_) if Instant::now() > end => break false,
            Ok(_) => {}
        }
    };
    assert!(closed);
}

#[test]
fn pointer_shape_and_host_moves() {
    let (platform, addr) = start();
    let mut p = Probe::connect(&addr).unwrap();
    p.hello().unwrap();
    p.window_list(Duration::from_secs(2)).unwrap();
    p.select(StreamSource::Desktop).unwrap();
    // La forme arrive : point chaud, taille (Float32 big-endian), puis un PNG.
    let shape = p.until(Duration::from_secs(2), |m| m.kind == kind::CURSOR_SHAPE).unwrap();
    let f = |i: usize| f32::from_bits(u32::from_be_bytes(shape.payload[i * 4..i * 4 + 4].try_into().unwrap()));
    assert_eq!((f(0), f(1), f(2), f(3)), (0.0, 0.0, 2.0, 2.0));
    assert_eq!(&shape.payload[16..24], b"\x89PNG\r\n\x1a\n");

    // La souris du PC bouge : l'appareil apprend où, en fraction de l'image (1920×1080).
    {
        let mut c = platform.cursor.lock().unwrap();
        c.x = 960.0;
        c.y = 270.0;
    }
    // (Un premier message a pu donner sa position de départ, 5,5.)
    let m = p
        .until(Duration::from_secs(2), |m| {
            m.kind == kind::MAC_POINTER && serde_json::from_slice::<serde_json::Value>(&m.payload).unwrap()["x"] == 0.5
        })
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&m.payload).unwrap();
    assert_eq!(v["inside"], true);
    assert_eq!(v["x"], 0.5);
    assert_eq!(v["y"], 0.25);
    assert_eq!(v["seen"], 0);

    // L'appareil le déplace lui-même : pas d'écho de sa propre position.
    p.input(&InputEvent::Pointer { action: PointerAction::Move, x: 0.1, y: 0.1 }).unwrap();
    let end = Instant::now() + Duration::from_millis(400);
    while Instant::now() < end {
        let m = p.read().unwrap();
        assert_ne!(m.kind, kind::MAC_POINTER, "position renvoyée alors que l'appareil menait");
    }

    // Hors de l'image : `inside` faux, sans coordonnées ; `seen` compte l'entrée lue.
    {
        let mut c = platform.cursor.lock().unwrap();
        c.x = 5000.0;
    }
    let m = p.until(Duration::from_secs(2), |m| m.kind == kind::MAC_POINTER).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&m.payload).unwrap();
    assert_eq!(v["inside"], false);
    assert!(v.get("x").is_none());
    assert_eq!(v["seen"], 1);
}
