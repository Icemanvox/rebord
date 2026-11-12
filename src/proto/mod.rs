//! Le protocole de Sill (ALPN `sill/1`), tel que le client iOS le parle.
//!
//! Référence : `Sources/StreamProtocol/StreamMessage.swift` du dépôt Saffsanity/sill.
//! En-tête de 14 octets, big-endian : type(1) horodatage(8, Double) image clé(1) longueur(4).

pub mod hevc;
pub mod msg;

use std::io::{self, Read};

/// Les types de messages. Un octet inconnu est lu puis ignoré, jamais une erreur.
pub mod kind {
    pub const PARAMETER_SETS: u8 = 0;
    pub const FRAME: u8 = 1;
    pub const WINDOW_LIST: u8 = 2;
    pub const THUMBNAIL: u8 = 3;
    pub const APP_ICON: u8 = 4;
    pub const APP_LIST: u8 = 5;
    pub const SELECT_SOURCE: u8 = 6;
    pub const LAUNCH_APP: u8 = 7;
    pub const INPUT: u8 = 8;
    pub const VIEWPORT: u8 = 9;
    pub const PING: u8 = 10;
    pub const PONG: u8 = 11;
    pub const CLIENT_STATS: u8 = 12;
    pub const TICK: u8 = 13;
    pub const CURSOR_SHAPE: u8 = 14;
    pub const WINDOW_COMMAND: u8 = 15;
    pub const HOST_SETTINGS: u8 = 16;
    pub const CHANGE_SETTINGS: u8 = 17;
    pub const GOODBYE: u8 = 22;
    pub const HELLO: u8 = 23;
    pub const MAC_POINTER: u8 = 26;
    pub const FETCH_MENU: u8 = 27;
    pub const GESTURE: u8 = 28;
    pub const CLIPBOARD: u8 = 32;
    pub const KEYFRAME_REQUEST: u8 = 44;

    /// Nom lisible pour le journal.
    pub fn name(k: u8) -> &'static str {
        match k {
            PARAMETER_SETS => "parameterSets",
            FRAME => "frame",
            WINDOW_LIST => "windowList",
            THUMBNAIL => "thumbnail",
            APP_ICON => "appIcon",
            APP_LIST => "appList",
            SELECT_SOURCE => "selectSource",
            LAUNCH_APP => "launchApp",
            INPUT => "input",
            VIEWPORT => "viewport",
            PING => "ping",
            PONG => "pong",
            CLIENT_STATS => "clientStats",
            TICK => "tick",
            CURSOR_SHAPE => "cursorShape",
            WINDOW_COMMAND => "windowCommand",
            HOST_SETTINGS => "hostSettings",
            CHANGE_SETTINGS => "changeSettings",
            GOODBYE => "goodbye",
            HELLO => "hello",
            MAC_POINTER => "macPointer",
            FETCH_MENU => "fetchMenu",
            GESTURE => "gesture",
            CLIPBOARD => "clipboard",
            KEYFRAME_REQUEST => "keyframeRequest",
            _ => "unknown",
        }
    }
}

pub const HEADER_LEN: usize = 14;
/// Plafond d'un message de l'appareil vers l'hôte (le plus gros réel fait quelques centaines d'octets).
pub const MAX_CLIENT_PAYLOAD: usize = 1 << 20;

/// Version du protocole annoncée dans la liste des fenêtres.
pub const PROTOCOL: i64 = 1;
/// Version de Sill pour Mac dont cet hôte suit le comportement : l'app compare des versions,
/// un hôte qui en annonce une trop ancienne lui paraîtrait à mettre à jour.
pub const COMPAT_HOST_VERSION: &str = "0.6.1";

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub kind: u8,
    pub timestamp: f64,
    pub is_keyframe: bool,
    pub payload: Vec<u8>,
}

impl Message {
    pub fn new(kind: u8, payload: Vec<u8>) -> Self {
        Message { kind, timestamp: now(), is_keyframe: false, payload }
    }

    pub fn serialize(&self) -> Vec<u8> {
        let mut d = Vec::with_capacity(HEADER_LEN + self.payload.len());
        d.push(self.kind);
        d.extend_from_slice(&self.timestamp.to_bits().to_be_bytes());
        d.push(self.is_keyframe as u8);
        d.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        d.extend_from_slice(&self.payload);
        d
    }

    /// Lit un message complet. `Ok(None)` à la fin propre du flux ; une erreur si la longueur
    /// annoncée dépasse `max_payload` (on ferme plutôt que d'attendre 4 Gio).
    pub fn read_from<R: Read>(r: &mut R, max_payload: usize) -> io::Result<Option<Message>> {
        let mut h = [0u8; HEADER_LEN];
        let mut got = 0;
        while got < HEADER_LEN {
            match r.read(&mut h[got..]) {
                Ok(0) if got == 0 => return Ok(None),
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        let kind = h[0];
        let timestamp = f64::from_bits(u64::from_be_bytes(h[1..9].try_into().unwrap()));
        let is_keyframe = h[9] == 1;
        let len = u32::from_be_bytes(h[10..14].try_into().unwrap()) as usize;
        if len > max_payload {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("message {} de {} octets annoncé, plafond {}", kind::name(kind), len, max_payload),
            ));
        }
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload)?;
        Ok(Some(Message { kind, timestamp, is_keyframe, payload }))
    }
}

/// Horloge murale en secondes depuis 1970, comme l'hôte Mac (l'app en déduit l'âge des images).
pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// VPS/SPS/PPS HEVC : longueur du préfixe NAL(1) nombre(1) puis, pour chacun, longueur(2) octets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParameterSets {
    pub nal_length_size: u8,
    pub sets: Vec<Vec<u8>>,
}

impl ParameterSets {
    pub fn encode(&self) -> Vec<u8> {
        let mut d = vec![self.nal_length_size, self.sets.len() as u8];
        for s in &self.sets {
            d.extend_from_slice(&(s.len() as u16).to_be_bytes());
            d.extend_from_slice(s);
        }
        d
    }

    pub fn decode(d: &[u8]) -> Option<ParameterSets> {
        if d.len() < 2 {
            return None;
        }
        let count = d[1] as usize;
        let mut off = 2;
        let mut sets = Vec::with_capacity(count);
        for _ in 0..count {
            let len = u16::from_be_bytes(d.get(off..off + 2)?.try_into().ok()?) as usize;
            off += 2;
            sets.push(d.get(off..off + len)?.to_vec());
            off += len;
        }
        Some(ParameterSets { nal_length_size: d[0], sets })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_round_trip() {
        let m = Message { kind: kind::FRAME, timestamp: 1_762_812_345.25, is_keyframe: true, payload: vec![1, 2, 3] };
        let bytes = m.serialize();
        assert_eq!(bytes.len(), 17);
        assert_eq!(bytes[0], 1);
        assert_eq!(&bytes[1..9], &1_762_812_345.25f64.to_bits().to_be_bytes());
        assert_eq!(bytes[9], 1);
        assert_eq!(&bytes[10..14], &[0, 0, 0, 3]);
        let back = Message::read_from(&mut &bytes[..], 1 << 20).unwrap().unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn oversized_payload_is_refused() {
        let mut h = vec![kind::INPUT];
        h.extend_from_slice(&0f64.to_bits().to_be_bytes());
        h.push(0);
        h.extend_from_slice(&(2_000_000u32).to_be_bytes());
        assert!(Message::read_from(&mut &h[..], MAX_CLIENT_PAYLOAD).is_err());
    }

    #[test]
    fn clean_end_of_stream() {
        assert!(Message::read_from(&mut &[][..], 10).unwrap().is_none());
    }

    #[test]
    fn parameter_sets_layout() {
        let ps = ParameterSets { nal_length_size: 4, sets: vec![vec![0x40, 1], vec![0x42], vec![0x44, 9, 9]] };
        let e = ps.encode();
        assert_eq!(e, vec![4, 3, 0, 2, 0x40, 1, 0, 1, 0x42, 0, 3, 0x44, 9, 9]);
        assert_eq!(ParameterSets::decode(&e).unwrap(), ps);
    }
}
