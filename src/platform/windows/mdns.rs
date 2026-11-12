//! Annonce Bonjour par l'API DNS-SD de Windows (dnsapi, Windows 10 et 11).
//!
//! Service `_sill._tcp`, sans clé TXT `p` : l'app y voit une porte en clair, qu'une build DEBUG
//! accepte (DiscoveryPolicy.rowWord). Pas de clé `r` non plus : c'est l'étiquette des Mac
//! appairés.

use crate::log;
use std::ffi::c_void;
use windows::Win32::NetworkManagement::Dns::*;
use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
use windows::core::{PCWSTR, PWSTR};

const DNS_REQUEST_PENDING: u32 = 9506;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn computer_name() -> String {
    unsafe {
        let mut len = 0u32;
        let _ = GetComputerNameExW(ComputerNameDnsHostname, None, &mut len);
        let mut buf = vec![0u16; len as usize + 1];
        let mut len2 = buf.len() as u32;
        if GetComputerNameExW(ComputerNameDnsHostname, Some(PWSTR(buf.as_mut_ptr())), &mut len2).is_ok() {
            return String::from_utf16_lossy(&buf[..len2 as usize]);
        }
    }
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into())
}

/// L'adresse IPv4 de la route par défaut (aucun paquet n'est envoyé).
fn primary_ipv4() -> Option<std::net::Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(a) if !a.is_unspecified() && !a.is_loopback() => Some(a),
        _ => None,
    }
}

unsafe extern "system" fn registered(status: u32, _ctx: *const c_void, instance: *const DNS_SERVICE_INSTANCE) {
    if status == 0 {
        log!("Bonjour : service enregistré");
    } else {
        log!("Bonjour : échec de l'enregistrement (code {status})");
    }
    if !instance.is_null() {
        unsafe { DnsServiceFreeInstance(instance) };
    }
}

struct Registration {
    request: Box<DNS_SERVICE_REGISTER_REQUEST>,
    instance: *mut DNS_SERVICE_INSTANCE,
    _strings: Vec<Vec<u16>>,
    _ip4: Box<u32>,
}

// Les pointeurs ne sont touchés que par l'enregistrement et son retrait.
unsafe impl Send for Registration {}

impl Drop for Registration {
    fn drop(&mut self) {
        unsafe {
            let _ = DnsServiceDeRegister(&*self.request, None);
            DnsServiceFreeInstance(self.instance);
        }
    }
}

pub fn advertise(name: &str, port: u16, tls: bool) -> Result<Box<dyn Send>, String> {
    // Un point dans le nom de l'instance la couperait en étiquettes DNS.
    let instance_name: String = name.chars().map(|c| if c == '.' { '-' } else { c }).take(63).collect();
    let host = computer_name();
    let service = wide(&format!("{instance_name}._sill._tcp.local"));
    let hostname = wide(&format!("{host}.local"));
    let ip = primary_ipv4();
    let (key_p, value_0) = (wide("p"), wide("0"));
    let keys = [PCWSTR(key_p.as_ptr())];
    let values = [PCWSTR(value_0.as_ptr())];
    // Ordre des octets de IP4_ADDRESS : celui du réseau, lu comme un u32 natif.
    let ip4 = Box::new(ip.map(|a| u32::from_ne_bytes(a.octets())).unwrap_or(0));
    unsafe {
        let instance = DnsServiceConstructInstance(
            PCWSTR(service.as_ptr()),
            PCWSTR(hostname.as_ptr()),
            if ip.is_some() { Some(&*ip4 as *const u32) } else { None },
            None,
            port,
            0,
            0,
            if tls { 1 } else { 0 },
            if tls { keys.as_ptr() } else { std::ptr::null() },
            if tls { values.as_ptr() } else { std::ptr::null() },
        );
        if instance.is_null() {
            return Err("DnsServiceConstructInstance a échoué".into());
        }
        let request = Box::new(DNS_SERVICE_REGISTER_REQUEST {
            Version: DNS_QUERY_REQUEST_VERSION1.0,
            InterfaceIndex: 0,
            pServiceInstance: instance,
            pRegisterCompletionCallback: Some(registered),
            pQueryContext: std::ptr::null_mut(),
            hCredentials: Default::default(),
            unicastEnabled: false.into(),
        });
        let status = DnsServiceRegister(&*request, None);
        if status != DNS_REQUEST_PENDING {
            DnsServiceFreeInstance(instance);
            return Err(format!("DnsServiceRegister a répondu {status}"));
        }
        log!(
            "Bonjour : « {instance_name} » sur {host}.local{} port {port}",
            ip.map(|a| format!(" ({a})")).unwrap_or_default()
        );
        Ok(Box::new(Registration { request, instance, _strings: vec![service, hostname, key_p, value_0], _ip4: ip4 }))
    }
}
