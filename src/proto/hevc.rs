//! HEVC : de la sortie Annex B de Media Foundation au format de l'app.
//!
//! L'app attend les VPS/SPS/PPS à part (message 0) et chaque image en NAL précédées de leur
//! longueur sur 4 octets (message 1), comme VideoToolbox les livre. Media Foundation produit des
//! codes de départ 00 00 01 / 00 00 00 01 avec les ensembles de paramètres en ligne.

use super::ParameterSets;

pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;
pub const NAL_AUD: u8 = 35;
pub const NAL_EOS: u8 = 36;
pub const NAL_EOB: u8 = 37;
pub const NAL_FD: u8 = 38;

/// Type d'une NAL HEVC (6 bits après le bit interdit).
pub fn nal_type(nal: &[u8]) -> Option<u8> {
    nal.first().map(|b| (b >> 1) & 0x3f)
}

/// IRAP (types 16 à 23) : BLA, IDR, CRA.
pub fn is_irap(t: u8) -> bool {
    (16..=23).contains(&t)
}

/// Découpe un flux Annex B en NAL, sans les codes de départ ni les zéros de fin.
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new(); // (début du code, début de la NAL)
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let code_start = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((code_start, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (n, &(_, nal_start)) in starts.iter().enumerate() {
        let mut end = if n + 1 < starts.len() { starts[n + 1].0 } else { data.len() };
        while end > nal_start && data[end - 1] == 0 {
            end -= 1;
        }
        if end > nal_start {
            out.push(&data[nal_start..end]);
        }
    }
    out
}

/// Une unité d'accès prête à partir.
#[derive(Debug, PartialEq, Eq)]
pub struct AccessUnit {
    /// Présents quand l'unité porte ses VPS, SPS et PPS (en pratique chaque image clé).
    pub parameter_sets: Option<ParameterSets>,
    /// Les NAL d'image, chacune précédée de sa longueur sur 4 octets big-endian.
    pub data: Vec<u8>,
    pub keyframe: bool,
}

/// Convertit la sortie de l'encodeur. Les ensembles de paramètres, délimiteurs et bourrages sont
/// retirés de l'image ; `None` si l'unité ne contient aucune NAL d'image.
pub fn from_annexb(annexb: &[u8]) -> Option<AccessUnit> {
    let (mut vps, mut sps, mut pps) = (None, None, None);
    let mut data = Vec::with_capacity(annexb.len() + 16);
    let mut keyframe = false;
    let mut pictures = 0;
    for nal in split_annexb(annexb) {
        let Some(t) = nal_type(nal) else { continue };
        match t {
            NAL_VPS => vps = Some(nal.to_vec()),
            NAL_SPS => sps = Some(nal.to_vec()),
            NAL_PPS => pps = Some(nal.to_vec()),
            NAL_AUD | NAL_EOS | NAL_EOB | NAL_FD => {}
            _ => {
                if t < 32 {
                    pictures += 1;
                    keyframe |= is_irap(t);
                }
                data.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                data.extend_from_slice(nal);
            }
        }
    }
    if pictures == 0 {
        return None;
    }
    let parameter_sets = match (vps, sps, pps) {
        (Some(v), Some(s), Some(p)) => Some(ParameterSets { nal_length_size: 4, sets: vec![v, s, p] }),
        _ => None,
    };
    Some(AccessUnit { parameter_sets, data, keyframe })
}

/// Les VPS, SPS et PPS d'un bloc Annex B (l'en-tête de séquence d'un type Media Foundation).
pub fn parameter_sets_from_annexb(data: &[u8]) -> Option<ParameterSets> {
    let (mut vps, mut sps, mut pps) = (None, None, None);
    for nal in split_annexb(data) {
        match nal_type(nal) {
            Some(NAL_VPS) => vps = Some(nal.to_vec()),
            Some(NAL_SPS) => sps = Some(nal.to_vec()),
            Some(NAL_PPS) => pps = Some(nal.to_vec()),
            _ => {}
        }
    }
    Some(ParameterSets { nal_length_size: 4, sets: vec![vps?, sps?, pps?] })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(t: u8, body: &[u8]) -> Vec<u8> {
        let mut v = vec![t << 1, 1];
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn splits_three_and_four_byte_codes() {
        let mut s = vec![0, 0, 0, 1];
        s.extend(nal(NAL_VPS, &[7]));
        s.extend([0, 0, 1]);
        s.extend(nal(1, &[8, 0])); // un zéro final appartient à la NAL suivante
        s.extend([0, 0, 0, 1]);
        s.extend(nal(1, &[9]));
        let parts = split_annexb(&s);
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], &nal(NAL_VPS, &[7])[..]);
        assert_eq!(parts[1], &nal(1, &[8])[..]);
        assert_eq!(parts[2], &nal(1, &[9])[..]);
    }

    #[test]
    fn keyframe_with_parameter_sets() {
        let mut s = Vec::new();
        for n in [nal(NAL_AUD, &[0x50]), nal(NAL_VPS, &[1]), nal(NAL_SPS, &[2]), nal(NAL_PPS, &[3]), nal(19, &[4, 5])] {
            s.extend([0, 0, 0, 1]);
            s.extend(n);
        }
        let au = from_annexb(&s).unwrap();
        assert!(au.keyframe);
        let ps = au.parameter_sets.unwrap();
        assert_eq!(ps.sets, vec![nal(NAL_VPS, &[1]), nal(NAL_SPS, &[2]), nal(NAL_PPS, &[3])]);
        let idr = nal(19, &[4, 5]);
        let mut expect = (idr.len() as u32).to_be_bytes().to_vec();
        expect.extend(idr);
        assert_eq!(au.data, expect);
    }

    #[test]
    fn sequence_header() {
        let mut s = Vec::new();
        for n in [nal(NAL_VPS, &[1]), nal(NAL_SPS, &[2]), nal(NAL_PPS, &[3])] {
            s.extend([0, 0, 1]);
            s.extend(n);
        }
        assert_eq!(parameter_sets_from_annexb(&s).unwrap().sets.len(), 3);
        assert!(parameter_sets_from_annexb(&s[..s.len() - 6]).is_none());
    }

    #[test]
    fn delta_frame_and_empty_unit() {
        let mut s = vec![0, 0, 1];
        s.extend(nal(1, &[1, 2, 3]));
        let au = from_annexb(&s).unwrap();
        assert!(!au.keyframe);
        assert!(au.parameter_sets.is_none());
        let mut only_ps = vec![0, 0, 1];
        only_ps.extend(nal(NAL_SPS, &[1]));
        assert!(from_annexb(&only_ps).is_none());
    }
}
