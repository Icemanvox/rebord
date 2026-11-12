//! Un répondeur mDNS / DNS-SD minimal (RFC 6762 et 6763), pour annoncer `_sill._tcp` sans
//! dépendre du répondeur de Windows (`DnsServiceRegister`), qui ne se laisse pas voir de tous.
//!
//! Il répond aux questions sur le type de service, l'instance et le nom d'hôte, s'annonce au
//! démarrage et quand les adresses changent, et dit au revoir en partant. Portable : testé sous
//! Linux comme sous Windows.

use crate::log;
use socket2::{Domain, Protocol, SockRef, Socket, Type};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
pub const PORT: u16 = 5353;
pub const SERVICE_TYPE: &str = "_sill._tcp.local";
const META: &str = "_services._dns-sd._udp.local";

const T_A: u16 = 1;
const T_PTR: u16 = 12;
const T_TXT: u16 = 16;
const T_AAAA: u16 = 28;
const T_SRV: u16 = 33;
const T_NSEC: u16 = 47;
const T_ANY: u16 = 255;
const CLASS_IN: u16 = 1;
const CACHE_FLUSH: u16 = 0x8000;

/// Durée de vie des enregistrements : deux minutes, pour qu'un hôte arrêté sans au revoir
/// (processus tué) disparaisse vite de l'app.
const TTL: u32 = 120;

// ---------------------------------------------------------------------------------------------
// Noms

fn eq_name(a: &str, b: &str) -> bool {
    a.trim_end_matches('.').eq_ignore_ascii_case(b.trim_end_matches('.'))
}

/// Une étiquette DNS d'au plus 63 octets, coupée sur une frontière de caractère.
fn label(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if out.len() + c.len_utf8() > 63 {
            break;
        }
        out.push(c);
    }
    out
}

/// Écrit un nom en étiquettes, sans compression. Le premier composant d'un nom d'instance peut
/// contenir des points : `labels` dit où couper.
fn put_labels(out: &mut Vec<u8>, labels: &[&str]) {
    for l in labels {
        let b = l.as_bytes();
        out.push(b.len().min(63) as u8);
        out.extend_from_slice(&b[..b.len().min(63)]);
    }
    out.push(0);
}

fn put_name(out: &mut Vec<u8>, name: &Name) {
    match name {
        Name::Plain(n) => {
            let parts: Vec<&str> = n.trim_end_matches('.').split('.').filter(|p| !p.is_empty()).collect();
            put_labels(out, &parts);
        }
        Name::Instance(inst) => {
            let mut parts = vec![inst.as_str()];
            parts.extend(SERVICE_TYPE.split('.'));
            put_labels(out, &parts);
        }
    }
}

/// Lit un nom (avec pointeurs de compression) ; rend le nom et la position après lui.
fn read_name(pkt: &[u8], mut off: usize) -> Option<(String, usize)> {
    let mut labels: Vec<String> = Vec::new();
    let mut end: Option<usize> = None;
    let mut jumps = 0;
    loop {
        let len = *pkt.get(off)? as usize;
        if len & 0xC0 == 0xC0 {
            let ptr = ((len & 0x3F) << 8) | *pkt.get(off + 1)? as usize;
            if end.is_none() {
                end = Some(off + 2);
            }
            jumps += 1;
            if jumps > 32 || ptr >= pkt.len() {
                return None;
            }
            off = ptr;
            continue;
        }
        if len & 0xC0 != 0 {
            return None;
        }
        if len == 0 {
            off += 1;
            break;
        }
        let bytes = pkt.get(off + 1..off + 1 + len)?;
        labels.push(String::from_utf8_lossy(bytes).into_owned());
        off += 1 + len;
        if labels.len() > 128 {
            return None;
        }
    }
    Some((labels.join("."), end.unwrap_or(off)))
}

// ---------------------------------------------------------------------------------------------
// Messages

#[derive(Clone, Debug, PartialEq, Eq)]
enum Name {
    Plain(String),
    /// `<instance>._sill._tcp.local`, l'instance pouvant contenir des points.
    Instance(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RData {
    Ptr(Name),
    Srv {
        port: u16,
        target: String,
    },
    Txt(Vec<String>),
    A(Ipv4Addr),
    /// Les types qui existent pour ce nom (les autres n'existent pas).
    Nsec(Vec<u16>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Record {
    name: Name,
    rtype: u16,
    unique: bool,
    ttl: u32,
    data: RData,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Question {
    pub name: String,
    pub qtype: u16,
    pub unicast: bool,
}

#[derive(Debug)]
pub struct Query {
    pub id: u16,
    pub questions: Vec<Question>,
    /// La section des questions telle que reçue (rendue telle quelle dans une réponse classique).
    question_bytes: Vec<u8>,
}

/// Une question mDNS, ou `None` pour une réponse ou un paquet illisible.
pub fn parse_query(pkt: &[u8]) -> Option<Query> {
    if pkt.len() < 12 {
        return None;
    }
    let id = u16::from_be_bytes([pkt[0], pkt[1]]);
    let flags = u16::from_be_bytes([pkt[2], pkt[3]]);
    if flags & 0x8000 != 0 || (flags >> 11) & 0xF != 0 {
        return None; // une réponse, ou pas une requête standard
    }
    let qd = u16::from_be_bytes([pkt[4], pkt[5]]) as usize;
    let mut off = 12;
    let mut questions = Vec::with_capacity(qd);
    for _ in 0..qd {
        let (name, next) = read_name(pkt, off)?;
        let qtype = u16::from_be_bytes(pkt.get(next..next + 2)?.try_into().ok()?);
        let qclass = u16::from_be_bytes(pkt.get(next + 2..next + 4)?.try_into().ok()?);
        off = next + 4;
        if qclass & 0x7FFF == CLASS_IN || qclass & 0x7FFF == 255 {
            questions.push(Question { name, qtype, unicast: qclass & 0x8000 != 0 });
        }
    }
    Some(Query { id, questions, question_bytes: pkt[12..off].to_vec() })
}

fn put_record(out: &mut Vec<u8>, r: &Record, legacy: bool) {
    put_name(out, &r.name);
    out.extend_from_slice(&r.rtype.to_be_bytes());
    let class = if r.unique && !legacy { CLASS_IN | CACHE_FLUSH } else { CLASS_IN };
    out.extend_from_slice(&class.to_be_bytes());
    let ttl = if legacy { r.ttl.min(10) } else { r.ttl };
    out.extend_from_slice(&ttl.to_be_bytes());
    let mut d = Vec::new();
    match &r.data {
        RData::Ptr(n) => put_name(&mut d, n),
        RData::Srv { port, target } => {
            d.extend_from_slice(&[0, 0, 0, 0]); // priorité, poids
            d.extend_from_slice(&port.to_be_bytes());
            put_name(&mut d, &Name::Plain(target.clone()));
        }
        // Sans entrée, une chaîne de longueur nulle (RFC 6763 §6.1) : pas de clé `p`, donc une
        // porte en clair pour l'app. `p=0` : TLS, porte ouverte.
        RData::Txt(entries) if entries.is_empty() => d.push(0),
        RData::Txt(entries) => {
            for e in entries {
                let b = &e.as_bytes()[..e.len().min(255)];
                d.push(b.len() as u8);
                d.extend_from_slice(b);
            }
        }
        RData::A(a) => d.extend_from_slice(&a.octets()),
        RData::Nsec(types) => {
            put_name(&mut d, &r.name);
            let mut bitmap = [0u8; 32];
            let mut len = 0;
            for &t in types {
                if t < 256 {
                    bitmap[(t / 8) as usize] |= 0x80 >> (t % 8);
                    len = len.max(t as usize / 8 + 1);
                }
            }
            d.push(0);
            d.push(len as u8);
            d.extend_from_slice(&bitmap[..len]);
        }
    }
    out.extend_from_slice(&(d.len() as u16).to_be_bytes());
    out.extend_from_slice(&d);
}

fn build(id: u16, questions: Option<&[u8]>, qdcount: u16, answers: &[Record], extra: &[Record]) -> Vec<u8> {
    let legacy = questions.is_some();
    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x8400u16.to_be_bytes()); // réponse, autorité
    out.extend_from_slice(&(if legacy { qdcount } else { 0 }).to_be_bytes());
    out.extend_from_slice(&(answers.len() as u16).to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&(extra.len() as u16).to_be_bytes());
    if let Some(q) = questions {
        out.extend_from_slice(q);
    }
    for r in answers {
        put_record(&mut out, r, legacy);
    }
    for r in extra {
        put_record(&mut out, r, legacy);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Le service

#[derive(Clone, Debug)]
pub struct Service {
    pub instance: String,
    /// Nom d'hôte complet, `….local`, propre à Rebord pour ne pas contredire celui de Windows.
    pub host: String,
    pub port: u16,
    /// Les entrées TXT (`p=0` pour une porte TLS ouverte, rien pour une porte en clair).
    pub txt: Vec<String>,
}

impl Service {
    pub fn new(instance: &str, computer: &str, port: u16) -> Service {
        let host_label: String = format!("Rebord-{computer}")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
            .take(63)
            .collect();
        Service { instance: label(instance), host: format!("{host_label}.local"), port, txt: Vec::new() }
    }

    fn instance_name(&self) -> String {
        format!("{}.{SERVICE_TYPE}", self.instance)
    }

    fn ptr(&self, ttl: u32) -> Record {
        Record {
            name: Name::Plain(SERVICE_TYPE.into()),
            rtype: T_PTR,
            unique: false,
            ttl,
            data: RData::Ptr(Name::Instance(self.instance.clone())),
        }
    }
    fn srv(&self, ttl: u32) -> Record {
        Record {
            name: Name::Instance(self.instance.clone()),
            rtype: T_SRV,
            unique: true,
            ttl,
            data: RData::Srv { port: self.port, target: self.host.clone() },
        }
    }
    fn txt(&self, ttl: u32) -> Record {
        Record {
            name: Name::Instance(self.instance.clone()),
            rtype: T_TXT,
            unique: true,
            ttl,
            data: RData::Txt(self.txt.clone()),
        }
    }
    fn a(&self, addrs: &[Ipv4Addr], ttl: u32) -> Vec<Record> {
        addrs
            .iter()
            .map(|a| Record { name: Name::Plain(self.host.clone()), rtype: T_A, unique: true, ttl, data: RData::A(*a) })
            .collect()
    }
    fn host_nsec(&self, ttl: u32) -> Record {
        Record { name: Name::Plain(self.host.clone()), rtype: T_NSEC, unique: true, ttl, data: RData::Nsec(vec![T_A]) }
    }
    fn meta(&self, ttl: u32) -> Record {
        Record {
            name: Name::Plain(META.into()),
            rtype: T_PTR,
            unique: false,
            ttl,
            data: RData::Ptr(Name::Plain(SERVICE_TYPE.into())),
        }
    }

    /// Les réponses et enregistrements additionnels pour ces questions ; vides si aucune ne nous
    /// concerne.
    fn answer(&self, questions: &[Question], addrs: &[Ipv4Addr]) -> (Vec<Record>, Vec<Record>) {
        let mut ans: Vec<Record> = Vec::new();
        let mut extra: Vec<Record> = Vec::new();
        let instance = self.instance_name();
        let wants = |q: &Question, t: u16| q.qtype == t || q.qtype == T_ANY;
        for q in questions {
            if eq_name(&q.name, SERVICE_TYPE) && wants(q, T_PTR) {
                ans.push(self.ptr(TTL));
                extra.push(self.srv(TTL));
                extra.push(self.txt(TTL));
                extra.extend(self.a(addrs, TTL));
                extra.push(self.host_nsec(TTL));
            } else if eq_name(&q.name, META) && wants(q, T_PTR) {
                ans.push(self.meta(TTL));
            } else if eq_name(&q.name, &instance) {
                if wants(q, T_SRV) {
                    ans.push(self.srv(TTL));
                    extra.extend(self.a(addrs, TTL));
                    extra.push(self.host_nsec(TTL));
                }
                if wants(q, T_TXT) {
                    ans.push(self.txt(TTL));
                }
            } else if eq_name(&q.name, &self.host) {
                if wants(q, T_A) {
                    ans.extend(self.a(addrs, TTL));
                }
                if q.qtype == T_AAAA {
                    ans.push(self.host_nsec(TTL)); // pas d'IPv6 : inutile d'attendre
                }
            }
        }
        dedup(&mut ans);
        extra.retain(|r| !ans.contains(r));
        dedup(&mut extra);
        (ans, extra)
    }

    /// Une annonce (ou, `ttl` nul, un au revoir) de tous nos enregistrements.
    fn announcement(&self, addrs: &[Ipv4Addr], ttl: u32) -> Vec<u8> {
        let mut ans = vec![self.ptr(ttl), self.srv(ttl), self.txt(ttl), self.meta(ttl)];
        ans.extend(self.a(addrs, ttl));
        build(0, None, 0, &ans, &[])
    }
}

fn dedup(v: &mut Vec<Record>) {
    let mut seen: Vec<Record> = Vec::new();
    v.retain(|r| {
        if seen.contains(r) {
            false
        } else {
            seen.push(r.clone());
            true
        }
    });
}

/// La réponse à un paquet reçu de `from` : à qui l'envoyer et quoi. `addrs` : nos adresses sur le
/// lien de `from`.
pub fn respond(
    service: &Service,
    pkt: &[u8],
    from: SocketAddrV4,
    addrs: &[Ipv4Addr],
) -> Option<(Vec<u8>, SocketAddrV4)> {
    let q = parse_query(pkt)?;
    let (ans, extra) = service.answer(&q.questions, addrs);
    if ans.is_empty() {
        return None;
    }
    if from.port() != PORT {
        // Résolveur classique (dig, nslookup) : réponse directe, avec l'identifiant et les questions.
        return Some((build(q.id, Some(&q.question_bytes), q.questions.len() as u16, &ans, &extra), from));
    }
    let to = if q.questions.iter().any(|x| x.unicast) { from } else { SocketAddrV4::new(GROUP, PORT) };
    Some((build(0, None, 0, &ans, &extra), to))
}

// ---------------------------------------------------------------------------------------------
// Les interfaces et le fil du répondeur

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Iface {
    pub addr: Ipv4Addr,
    pub mask: Ipv4Addr,
}

impl Iface {
    fn contains(&self, ip: Ipv4Addr) -> bool {
        let m = u32::from(self.mask);
        u32::from(self.addr) & m == u32::from(ip) & m
    }
}

/// Les adresses IPv4 utilisables : ni boucle locale, ni adresse automatique 169.254.
pub fn interfaces() -> Vec<Iface> {
    let mut v: Vec<Iface> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(a) if !a.ip.is_loopback() && !a.ip.is_link_local() => {
                Some(Iface { addr: a.ip, mask: a.netmask })
            }
            _ => None,
        })
        .collect();
    if v.is_empty() {
        // Énumération vide (vue sous Wine) : l'adresse de la route par défaut, en /24.
        if let Some(addr) = default_route_ipv4() {
            v.push(Iface { addr, mask: Ipv4Addr::new(255, 255, 255, 0) });
        }
    }
    v.sort_by_key(|i| u32::from(i.addr));
    v.dedup();
    v
}

/// L'adresse IPv4 par laquelle part la route par défaut (aucun paquet n'est envoyé).
fn default_route_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(a) if !a.is_unspecified() && !a.is_loopback() => Some(a),
        _ => None,
    }
}

struct Shared {
    socket: UdpSocket,
    service: Service,
    ifaces: Mutex<Vec<Iface>>,
    stop: AtomicBool,
}

impl Shared {
    fn send_multicast(&self, iface: &Iface, pkt: &[u8]) {
        let s = SockRef::from(&self.socket);
        if s.set_multicast_if_v4(&iface.addr).is_ok() {
            let _ = self.socket.send_to(pkt, SocketAddrV4::new(GROUP, PORT));
        }
    }

    fn announce(&self, ttl: u32) {
        let ifaces = self.ifaces.lock().unwrap().clone();
        for i in &ifaces {
            self.send_multicast(i, &self.service.announcement(&[i.addr], ttl));
        }
    }
}

static ACTIVE: OnceLock<Mutex<Vec<Arc<Shared>>>> = OnceLock::new();

/// L'annonce en cours ; la lâcher envoie l'au revoir et arrête le répondeur.
pub struct Responder {
    shared: Arc<Shared>,
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        self.shared.announce(0);
        if let Some(a) = ACTIVE.get() {
            a.lock().unwrap().retain(|s| !Arc::ptr_eq(s, &self.shared));
        }
    }
}

/// Au revoir de toutes les annonces en cours (fermeture de la console, Ctrl+C).
pub fn goodbye_all() {
    if let Some(a) = ACTIVE.get() {
        for s in a.lock().unwrap().iter() {
            s.announce(0);
        }
    }
}

fn open_socket() -> std::io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Le port 5353 est partagé avec le répondeur du système.
    s.set_reuse_address(true)?;
    #[cfg(all(unix, not(target_os = "solaris")))]
    s.set_reuse_port(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT)).into())?;
    s.set_multicast_ttl_v4(255)?;
    s.set_multicast_loop_v4(true)?;
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    Ok(s.into())
}

fn join(socket: &UdpSocket, ifaces: &[Iface]) {
    for i in ifaces {
        if let Err(e) = socket.join_multicast_v4(&GROUP, &i.addr) {
            log!("mDNS : impossible de rejoindre le groupe sur {} : {e}", i.addr);
        }
    }
}

pub fn start(service: Service) -> Result<Responder, String> {
    let socket = open_socket().map_err(|e| format!("port UDP 5353 : {e}"))?;
    let ifaces = interfaces();
    if ifaces.is_empty() {
        log!("mDNS : aucune adresse IPv4 pour l'instant ; l'annonce partira quand le réseau viendra");
    }
    join(&socket, &ifaces);
    log!(
        "mDNS : « {} » ({}), port {}, sur {}",
        service.instance,
        service.host,
        service.port,
        ifaces.iter().map(|i| i.addr.to_string()).collect::<Vec<_>>().join(", ")
    );
    let shared = Arc::new(Shared { socket, service, ifaces: Mutex::new(ifaces), stop: AtomicBool::new(false) });
    ACTIVE.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap().push(shared.clone());
    let s = shared.clone();
    thread::Builder::new().name("mdns".into()).spawn(move || run(s)).map_err(|e| e.to_string())?;
    Ok(Responder { shared })
}

fn run(s: Arc<Shared>) {
    let started = Instant::now();
    // Annonces à 0, 1 et 3 s (RFC 6762 §8.3 : au moins deux, la seconde un peu après la première).
    let mut announces: Vec<Duration> = vec![Duration::ZERO, Duration::from_secs(1), Duration::from_secs(3)];
    let mut next_scan = Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 9000];
    let mut answered = 0u64;
    while !s.stop.load(Ordering::SeqCst) {
        if let Some(&t) = announces.first() {
            if started.elapsed() >= t {
                announces.remove(0);
                s.announce(TTL);
            }
        }
        if Instant::now() >= next_scan {
            next_scan = Instant::now() + Duration::from_secs(5);
            let fresh = interfaces();
            let mut cur = s.ifaces.lock().unwrap();
            if fresh != *cur {
                let added: Vec<Iface> = fresh.iter().filter(|i| !cur.contains(i)).copied().collect();
                log!(
                    "mDNS : adresses maintenant {}",
                    fresh.iter().map(|i| i.addr.to_string()).collect::<Vec<_>>().join(", ")
                );
                join(&s.socket, &added);
                *cur = fresh;
                drop(cur);
                s.announce(TTL);
            }
        }
        let (n, from) = match s.socket.recv_from(&mut buf) {
            Ok(x) => x,
            Err(_) => continue, // délai écoulé
        };
        let SocketAddr::V4(from) = from else { continue };
        let ifaces = s.ifaces.lock().unwrap().clone();
        let link: Vec<Iface> = ifaces.iter().filter(|i| i.contains(*from.ip())).copied().collect();
        if link.iter().any(|i| i.addr == *from.ip()) && from.port() == PORT {
            continue; // nos propres paquets, revenus par la boucle multicast
        }
        // Une question venue d'ailleurs que nos réseaux (boucle locale, VPN) reçoit toutes nos adresses.
        let addrs: Vec<Ipv4Addr> = if link.is_empty() {
            ifaces.iter().map(|i| i.addr).collect()
        } else {
            link.iter().map(|i| i.addr).collect()
        };
        let Some((reply, to)) = respond(&s.service, &buf[..n], from, &addrs) else { continue };
        if *to.ip() == GROUP {
            match link.first().or(ifaces.first()) {
                Some(i) => s.send_multicast(i, &reply),
                None => continue,
            }
        } else {
            let _ = s.socket.send_to(&reply, to);
        }
        answered += 1;
        if answered <= 3 {
            log!("mDNS : question de {} ({} adresse(s) données)", from.ip(), addrs.len());
        }
    }
}

/// Le nom de cette machine, pour le nom d'hôte de l'annonce.
pub fn computer_name() -> String {
    #[cfg(windows)]
    {
        crate::platform::windows::mdns::computer_name()
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "pc".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(name: &str, qtype: u16, qu: bool) -> Vec<u8> {
        let mut p = vec![0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        put_name(&mut p, &Name::Plain(name.into()));
        p.extend_from_slice(&qtype.to_be_bytes());
        p.extend_from_slice(&(if qu { 0x8001u16 } else { 1u16 }).to_be_bytes());
        p
    }

    /// Les enregistrements d'une réponse : (nom, type, ttl, données brutes).
    fn records(pkt: &[u8]) -> Vec<(String, u16, u32, Vec<u8>)> {
        let n = |o: usize| u16::from_be_bytes([pkt[o], pkt[o + 1]]) as usize;
        let (qd, an, ns, ar) = (n(4), n(6), n(8), n(10));
        let mut off = 12;
        for _ in 0..qd {
            off = read_name(pkt, off).unwrap().1 + 4;
        }
        let mut out = Vec::new();
        for _ in 0..an + ns + ar {
            let (name, o) = read_name(pkt, off).unwrap();
            let t = n(o) as u16;
            let ttl = u32::from_be_bytes(pkt[o + 4..o + 8].try_into().unwrap());
            let len = n(o + 8);
            out.push((name, t, ttl, pkt[o + 10..o + 10 + len].to_vec()));
            off = o + 10 + len;
        }
        out
    }

    fn svc() -> Service {
        Service::new("David-PC", "David-PC", 48100)
    }

    const MDNS_PEER: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 20), PORT);

    #[test]
    fn browse_gets_ptr_with_srv_txt_and_address() {
        let ip = Ipv4Addr::new(192, 168, 1, 170);
        let (pkt, to) = respond(&svc(), &query("_sill._tcp.local", T_PTR, false), MDNS_PEER, &[ip]).unwrap();
        assert_eq!(to, SocketAddrV4::new(GROUP, PORT));
        let r = records(&pkt);
        assert_eq!(r[0].0, "_sill._tcp.local");
        assert_eq!(r[0].1, T_PTR);
        assert_eq!(read_name(&r[0].3, 0).unwrap().0, "David-PC._sill._tcp.local");
        let srv = r.iter().find(|x| x.1 == T_SRV).unwrap();
        assert_eq!(u16::from_be_bytes([srv.3[4], srv.3[5]]), 48100);
        assert_eq!(read_name(&srv.3, 6).unwrap().0, "Rebord-David-PC.local");
        let txt = r.iter().find(|x| x.1 == T_TXT).unwrap();
        assert_eq!(txt.3, vec![0]);
        let a = r.iter().find(|x| x.1 == T_A).unwrap();
        assert_eq!(a.3, ip.octets().to_vec());
        assert!(r.iter().any(|x| x.1 == T_NSEC));
    }

    #[test]
    fn open_door_txt() {
        let mut s = svc();
        s.txt = vec!["p=0".into()];
        let (pkt, _) = respond(&s, &query("David-PC._sill._tcp.local", T_TXT, false), MDNS_PEER, &[]).unwrap();
        assert_eq!(records(&pkt)[0].3, b"\x03p=0".to_vec());
    }

    #[test]
    fn case_insensitive_and_qu() {
        let (_, to) = respond(&svc(), &query("_SILL._TCP.LOCAL.", T_PTR, true), MDNS_PEER, &[]).unwrap();
        assert_eq!(to, MDNS_PEER);
    }

    #[test]
    fn instance_with_dots_and_spaces() {
        let s = Service::new("Bureau de David v2.1", "PC", 1);
        let q = query("Bureau de David v2.1._sill._tcp.local", T_SRV, false);
        // Le nom est lu en étiquettes : la requête l'a coupé aux points, la comparaison suit.
        let (pkt, _) = respond(&s, &q, MDNS_PEER, &[Ipv4Addr::new(10, 0, 0, 2)]).unwrap();
        assert!(records(&pkt).iter().any(|x| x.1 == T_SRV));
    }

    #[test]
    fn host_address_and_no_ipv6() {
        let s = svc();
        let (pkt, _) =
            respond(&s, &query("rebord-david-pc.local", T_A, false), MDNS_PEER, &[Ipv4Addr::new(10, 0, 0, 2)]).unwrap();
        assert_eq!(records(&pkt)[0].3, vec![10, 0, 0, 2]);
        let (pkt, _) = respond(&s, &query("Rebord-David-PC.local", T_AAAA, false), MDNS_PEER, &[]).unwrap();
        assert_eq!(records(&pkt)[0].1, T_NSEC);
    }

    #[test]
    fn legacy_unicast_echoes_the_question() {
        let mut q = query("_sill._tcp.local", T_PTR, false);
        q[0] = 0x12;
        q[1] = 0x34;
        let from = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 20), 53000);
        let (pkt, to) = respond(&svc(), &q, from, &[Ipv4Addr::new(192, 168, 1, 170)]).unwrap();
        assert_eq!(to, from);
        assert_eq!(&pkt[0..2], &[0x12, 0x34]);
        assert_eq!(u16::from_be_bytes([pkt[4], pkt[5]]), 1);
        assert!(records(&pkt).iter().all(|r| r.2 <= 10));
    }

    #[test]
    fn other_questions_and_responses_are_ignored() {
        assert!(respond(&svc(), &query("_airplay._tcp.local", T_PTR, false), MDNS_PEER, &[]).is_none());
        let mut resp = query("_sill._tcp.local", T_PTR, false);
        resp[2] = 0x84;
        assert!(respond(&svc(), &resp, MDNS_PEER, &[]).is_none());
        assert!(respond(&svc(), &[1, 2, 3], MDNS_PEER, &[]).is_none());
    }

    #[test]
    fn services_meta_query() {
        let (pkt, _) = respond(&svc(), &query("_services._dns-sd._udp.local", T_PTR, false), MDNS_PEER, &[]).unwrap();
        assert_eq!(read_name(&records(&pkt)[0].3, 0).unwrap().0, "_sill._tcp.local");
    }

    #[test]
    fn compressed_names_are_read() {
        // "_sill._tcp.local" en 12, puis un pointeur vers lui.
        let mut p = vec![0u8; 12];
        put_name(&mut p, &Name::Plain("_sill._tcp.local".into()));
        let ptr_at = p.len();
        p.extend_from_slice(&[0xC0, 12]);
        assert_eq!(read_name(&p, ptr_at).unwrap(), ("_sill._tcp.local".into(), ptr_at + 2));
        // Une boucle de pointeurs ne fait pas tourner le lecteur sans fin.
        let looped = [0u8, 0, 0xC0, 2];
        assert!(read_name(&looped, 2).is_none());
    }

    #[test]
    fn goodbye_has_zero_ttl() {
        let r = records(&svc().announcement(&[Ipv4Addr::new(10, 0, 0, 2)], 0));
        assert!(r.len() >= 5 && r.iter().all(|x| x.2 == 0));
    }
}
