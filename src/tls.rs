//! La porte TLS de Sill (RemoteTLS.swift) : TLS 1.3 seul, certificat P-256 auto-signé de chaque
//! côté, ALPN `sill/1` (session) et `sill-pair/1` (appairage), ni tickets ni reprise de session.
//!
//! Porte ouverte (`p=0` dans le TXT) : l'hôte accepte tout appareil qui présente un certificat, et
//! l'appareil accepte la clé de l'hôte sans appairage. Une build Release de l'app s'y connecte.

use crate::log;
use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256, PublicKeyData};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms, ring as provider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::{NoServerSessionStorage, ServerConnection};
use rustls::{DigitallySignedStruct, DistinguishedName as TlsDn, SignatureScheme};
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const SESSION_ALPN: &[u8] = b"sill/1";
pub const PAIRING_ALPN: &[u8] = b"sill-pair/1";
pub const SERVER_NAME: &str = "sill";
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

fn provider() -> Arc<CryptoProvider> {
    Arc::new(provider::default_provider())
}

fn algorithms() -> WebPkiSupportedAlgorithms {
    provider::default_provider().signature_verification_algorithms
}

// ---------------------------------------------------------------------------------------------
// Identité

pub struct Identity {
    pub key: KeyPair,
    pub cert: CertificateDer<'static>,
    /// SHA-256 de la SubjectPublicKeyInfo : ce que l'app épingle.
    pub fingerprint: [u8; 32],
}

impl Identity {
    /// Une identité éphémère (tests, sonde).
    pub fn ephemeral(name: &str) -> Result<Identity, String> {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
        Self::from_key(key, name)
    }

    fn from_key(key: KeyPair, name: &str) -> Result<Identity, String> {
        let mut params = CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, name);
        params.distinguished_name = dn;
        let cert = params.self_signed(&key).map_err(|e| e.to_string())?;
        let digest = ring::digest::digest(&ring::digest::SHA256, &key.subject_public_key_info());
        let mut fingerprint = [0u8; 32];
        fingerprint.copy_from_slice(digest.as_ref());
        Ok(Identity { cert: cert.der().clone(), key, fingerprint })
    }

    /// La clé de cet hôte, gardée d'un lancement à l'autre : une session rouverte retrouve la même.
    pub fn load_or_create(dir: &Path, name: &str) -> Result<Identity, String> {
        let path = dir.join("identity-key.pem");
        let key = match std::fs::read_to_string(&path) {
            Ok(pem) => KeyPair::from_pem(&pem).map_err(|e| format!("{} illisible : {e}", path.display()))?,
            Err(_) => {
                let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
                std::fs::create_dir_all(dir).map_err(|e| format!("{} : {e}", dir.display()))?;
                std::fs::write(&path, key.serialize_pem()).map_err(|e| format!("{} : {e}", path.display()))?;
                log!("TLS : nouvelle clé d'hôte enregistrée dans {}", path.display());
                key
            }
        };
        if key.algorithm() != &PKCS_ECDSA_P256_SHA256 {
            return Err(format!("{} n'est pas une clé P-256", path.display()));
        }
        Self::from_key(key, name)
    }

    fn private_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.serialize_der()))
    }

    /// Les 16 premiers chiffres hexadécimaux de l'empreinte, pour le journal.
    pub fn short(&self) -> String {
        self.fingerprint[..8].iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Le dossier des données de Rebord.
pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(d).join("Rebord");
    }
    if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(d).join("rebord");
    }
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h).join(".local/share/rebord");
    }
    PathBuf::from(".")
}

// ---------------------------------------------------------------------------------------------
// Vérificateurs : l'épingle est toute la confiance, jamais les noms, dates ou chaînes.

/// Côté hôte, porte ouverte : tout certificat est admis, pourvu que l'appareil prouve qu'il en
/// détient la clé (signature du handshake vérifiée).
#[derive(Debug)]
struct AnyDevice;

impl ClientCertVerifier for AnyDevice {
    fn offer_client_auth(&self) -> bool {
        true
    }
    fn client_auth_mandatory(&self) -> bool {
        true
    }
    fn root_hint_subjects(&self) -> &[TlsDn] {
        &[]
    }
    fn verify_client_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 refusé".into()))
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &algorithms())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        algorithms().supported_schemes()
    }
}

/// Côté sonde : la clé de l'hôte, quelle qu'elle soit, comme l'app sur une porte ouverte.
#[derive(Debug)]
struct AnyHost;

impl ServerCertVerifier for AnyHost {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 refusé".into()))
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &algorithms())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        algorithms().supported_schemes()
    }
}

pub fn server_config(id: &Identity) -> Result<Arc<rustls::ServerConfig>, String> {
    let mut c = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .with_client_cert_verifier(Arc::new(AnyDevice))
        .with_single_cert(vec![id.cert.clone()], id.private_key())
        .map_err(|e| e.to_string())?;
    c.alpn_protocols = vec![SESSION_ALPN.to_vec(), PAIRING_ALPN.to_vec()];
    c.send_tls13_tickets = 0;
    c.session_storage = Arc::new(NoServerSessionStorage {});
    Ok(Arc::new(c))
}

pub fn client_config(id: &Identity, alpn: &[u8]) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut c = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyHost))
        .with_client_auth_cert(vec![id.cert.clone()], id.private_key())
        .map_err(|e| e.to_string())?;
    c.alpn_protocols = vec![alpn.to_vec()];
    c.resumption = rustls::client::Resumption::disabled();
    Ok(Arc::new(c))
}

pub fn connect(
    stream: TcpStream,
    config: Arc<rustls::ClientConfig>,
) -> io::Result<rustls::StreamOwned<rustls::ClientConnection, TcpStream>> {
    let name = ServerName::try_from(SERVER_NAME).unwrap();
    let conn = rustls::ClientConnection::new(config, name).map_err(io::Error::other)?;
    let mut s = rustls::StreamOwned::new(conn, stream);
    while s.conn.is_handshaking() {
        s.conn.complete_io(&mut s.sock)?;
    }
    Ok(s)
}

// ---------------------------------------------------------------------------------------------
// Une connexion côté hôte, lue et écrite depuis deux fils.
//
// Le fil de lecture lit la socket sans verrou puis passe les octets à rustls ; le fil d'envoi
// chiffre et écrit. Tout chiffrement se fait sous le verrou d'écriture de la socket, pris avant
// celui de la connexion : les enregistrements partent dans l'ordre où ils ont été chiffrés.

struct Shared {
    conn: Mutex<ServerConnection>,
    sock: Mutex<TcpStream>,
}

impl Shared {
    fn flush(&self, plaintext: Option<&[u8]>) -> io::Result<()> {
        let mut sock = self.sock.lock().unwrap();
        let mut out = Vec::new();
        {
            let mut c = self.conn.lock().unwrap();
            if let Some(p) = plaintext {
                c.writer().write_all(p)?;
            }
            while c.wants_write() {
                c.write_tls(&mut out)?;
            }
        }
        if !out.is_empty() {
            sock.write_all(&out)?;
        }
        Ok(())
    }
}

pub struct TlsReader {
    sock: TcpStream,
    shared: Arc<Shared>,
    raw: Box<[u8]>,
}

pub struct TlsWriter {
    shared: Arc<Shared>,
}

impl TlsReader {
    /// Lit la socket une fois et passe ce qui est venu à rustls. Faux en fin de flux.
    fn pump(&mut self) -> io::Result<bool> {
        let n = self.sock.read(&mut self.raw)?;
        if n == 0 {
            return Ok(false);
        }
        let (failed, wants_write) = {
            let mut c = self.shared.conn.lock().unwrap();
            let mut slice = &self.raw[..n];
            let mut failed = None;
            while !slice.is_empty() {
                if let Err(e) = c.read_tls(&mut slice) {
                    failed = Some(io::Error::new(io::ErrorKind::InvalidData, e));
                    break;
                }
                if let Err(e) = c.process_new_packets() {
                    failed = Some(io::Error::new(io::ErrorKind::InvalidData, e));
                    break;
                }
            }
            (failed, c.wants_write())
        };
        // Réponses du handshake, ou l'alerte qui explique un échec. Seulement s'il y a à écrire :
        // le verrou d'écriture peut être tenu par l'envoi d'une grosse image, et la lecture des
        // entrées n'a pas à l'attendre.
        let flushed = if wants_write { self.shared.flush(None) } else { Ok(()) };
        if let Some(e) = failed {
            return Err(e);
        }
        flushed?;
        Ok(true)
    }
}

impl Read for TlsReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        loop {
            {
                let mut c = self.shared.conn.lock().unwrap();
                match c.reader().read(out) {
                    Ok(n) => return Ok(n), // 0 : l'appareil a fermé proprement
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
            if !self.pump()? {
                return Ok(0);
            }
        }
    }
}

impl Write for TlsWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.shared.flush(Some(buf))?;
        Ok(buf.len())
    }
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.shared.flush(Some(buf))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub struct Accepted {
    pub reader: TlsReader,
    pub writer: TlsWriter,
    pub alpn: Vec<u8>,
}

/// Le handshake côté hôte, en 10 s au plus.
pub fn accept(stream: TcpStream, config: Arc<rustls::ServerConfig>) -> io::Result<Accepted> {
    let mut conn = ServerConnection::new(config).map_err(io::Error::other)?;
    // Sans plafond : une image clé en 2560×1440 dépasse de loin les 64 Kio par défaut, et
    // `write_all` échouait alors (WriteZero). Le fil d'envoi chiffre et vide tout aussitôt.
    conn.set_buffer_limit(None);
    let shared = Arc::new(Shared { conn: Mutex::new(conn), sock: Mutex::new(stream.try_clone()?) });
    let mut reader = TlsReader { sock: stream, shared: shared.clone(), raw: vec![0u8; 18 * 1024].into_boxed_slice() };
    let started = Instant::now();
    reader.sock.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    while shared.conn.lock().unwrap().is_handshaking() {
        if started.elapsed() > HANDSHAKE_TIMEOUT {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "handshake TLS trop long"));
        }
        if !reader.pump()? {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "fermée pendant le handshake TLS"));
        }
    }
    reader.sock.set_read_timeout(None)?;
    let alpn = shared.conn.lock().unwrap().alpn_protocol().map(|a| a.to_vec()).unwrap_or_default();
    Ok(Accepted { reader, writer: TlsWriter { shared }, alpn })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Une image clé de plusieurs Mio passe en un seul `write_all` (bug de la 0.2.0 : WriteZero
    /// au-delà de 64 Kio, la connexion fermée, l'app qui se reconnectait en boucle).
    #[test]
    fn large_writes_go_through() {
        let host = Identity::ephemeral("hôte").unwrap();
        let server = server_config(&host).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let payload: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let expected = payload.clone();
        let t = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut a = accept(s, server).unwrap();
            assert_eq!(a.alpn, SESSION_ALPN);
            a.writer.write_all(&payload).unwrap();
            a.writer.write_all(b"fin").unwrap();
            let mut b = [0u8; 5];
            a.reader.read_exact(&mut b).unwrap();
            assert_eq!(&b, b"merci");
        });
        let dev = Identity::ephemeral("appareil").unwrap();
        let tcp = TcpStream::connect(addr).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut c = connect(tcp, client_config(&dev, SESSION_ALPN).unwrap()).unwrap();
        let mut got = vec![0u8; expected.len() + 3];
        c.read_exact(&mut got).unwrap();
        assert!(got[..expected.len()] == expected[..]);
        assert_eq!(&got[expected.len()..], b"fin");
        c.write_all(b"merci").unwrap();
        c.flush().unwrap();
        t.join().unwrap();
    }
}
