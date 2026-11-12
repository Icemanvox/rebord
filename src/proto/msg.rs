//! Les charges JSON, au format exact de `JSONEncoder` de Swift.
//!
//! Un enum Swift à valeur associée s'écrit `{"cas":{"_0":valeur}}` (ou avec les noms des
//! paramètres quand ils en ont), un cas sans valeur `{"cas":{}}`. Les champs facultatifs absents
//! ne sont pas écrits.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Ce que l'hôte diffuse : une fenêtre, l'écran principal, ou rien.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StreamSource {
    None,
    Window(u32),
    Desktop,
}

impl StreamSource {
    pub fn to_value(self) -> Value {
        match self {
            StreamSource::None => json!({"none": {}}),
            StreamSource::Window(id) => json!({"window": {"_0": id}}),
            StreamSource::Desktop => json!({"desktop": {}}),
        }
    }

    pub fn from_value(v: &Value) -> Option<StreamSource> {
        let (case, body) = single_case(v)?;
        match case {
            "none" => Some(StreamSource::None),
            "desktop" => Some(StreamSource::Desktop),
            "window" => {
                let id = body.get("_0")?.as_u64()?;
                u32::try_from(id).ok().map(StreamSource::Window)
            }
            _ => None,
        }
    }
}

impl Serialize for StreamSource {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

impl<'de> Deserialize<'de> for StreamSource {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        StreamSource::from_value(&v).ok_or_else(|| serde::de::Error::custom("StreamSource inconnue"))
    }
}

/// `{"cas":{…}}` : le seul couple clé/objet.
fn single_case(v: &Value) -> Option<(&str, &Map<String, Value>)> {
    let obj = v.as_object()?;
    if obj.len() != 1 {
        return None;
    }
    let (k, body) = obj.iter().next()?;
    Some((k.as_str(), body.as_object()?))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    #[serde(rename = "bundleID")]
    pub bundle_id: String,
    /// En points (pixels logiques), comme sur Mac.
    pub width: i64,
    pub height: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowList {
    pub mac_name: String,
    pub windows: Vec<WindowInfo>,
    pub active: StreamSource,
    #[serde(rename = "launchID", skip_serializing_if = "Option::is_none")]
    pub launch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_version: Option<String>,
    #[serde(rename = "protocol", skip_serializing_if = "Option::is_none")]
    pub protocol: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gestures: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clipboard: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_repeat: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub off_console: Option<bool>,
}

/// Premier message de l'appareil sur une session. Tous les champs sont facultatifs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(rename = "protocol", skip_serializing_if = "Option::is_none")]
    pub protocol: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio: Option<Vec<String>>,
}

/// Le panneau de l'appareil, en points, l'échelle de texte voulue et la cadence voulue.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Viewport {
    pub width: f64,
    pub height: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_cursor: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<i64>,
}

impl Viewport {
    /// Les bornes de l'hôte Mac (ViewportBounds) : un côté de 1 à 16384 points, une échelle de
    /// 0,25 à 4. Hors bornes, le message est refusé en entier.
    pub fn refusal(&self) -> Option<String> {
        let side = |x: f64| x.is_finite() && (1.0..=16384.0).contains(&x);
        if !side(self.width) || !side(self.height) {
            return Some(format!("taille {}×{} hors de 1…16384", self.width, self.height));
        }
        if let Some(s) = self.scale {
            if !(s.is_finite() && (0.25..=4.0).contains(&s)) {
                return Some(format!("échelle {} hors de 0,25…4", s));
            }
        }
        None
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStats {
    pub fps: i64,
    pub frame_age_ms: i64,
    pub rtt_ms: i64,
    pub device: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_age_max_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt_max_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowCommand {
    pub id: u32,
    /// "close", "minimize" ou "fullScreen".
    pub action: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackpadGesture {
    /// swipeUp, swipeDown, swipeLeft, swipeRight, pinch, spread.
    pub gesture: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingers: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerAction {
    Move,
    LeftDown,
    LeftUp,
    RightDown,
    RightUp,
}

impl PointerAction {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "move" => PointerAction::Move,
            "leftDown" => PointerAction::LeftDown,
            "leftUp" => PointerAction::LeftUp,
            "rightDown" => PointerAction::RightDown,
            "rightUp" => PointerAction::RightUp,
            _ => return None,
        })
    }
    fn word(self) -> &'static str {
        match self {
            PointerAction::Move => "move",
            PointerAction::LeftDown => "leftDown",
            PointerAction::LeftUp => "leftUp",
            PointerAction::RightDown => "rightDown",
            PointerAction::RightUp => "rightUp",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollPhase {
    Began,
    Ended,
    MomentumBegan,
    MomentumEnded,
}

/// Une entrée de l'appareil. Positions et déplacements en fractions de l'image (0 à 1).
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Pointer {
        action: PointerAction,
        x: f64,
        y: f64,
    },
    /// dy positif : le contenu descend (défilement naturel).
    Scroll {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    /// Texte du clavier logiciel ; "\n", "\t" et "\u{8}" valent Entrée, Tab et Retour arrière.
    Text(String),
    ScrollGesture {
        phase: ScrollPhase,
        x: f64,
        y: f64,
    },
    /// Usage HID USB ; modificateurs en bits UIKeyModifierFlags.
    Key {
        hid_usage: u16,
        down: bool,
        modifiers: u64,
    },
}

impl InputEvent {
    pub fn from_json(bytes: &[u8]) -> Option<InputEvent> {
        let v: Value = serde_json::from_slice(bytes).ok()?;
        let (case, b) = single_case(&v)?;
        let f = |k: &str| b.get(k).and_then(Value::as_f64).filter(|x| x.is_finite());
        Some(match case {
            "pointer" => {
                InputEvent::Pointer { action: PointerAction::parse(b.get("_0")?.as_str()?)?, x: f("x")?, y: f("y")? }
            }
            "scroll" => InputEvent::Scroll { x: f("x")?, y: f("y")?, dx: f("dx")?, dy: f("dy")? },
            "text" => InputEvent::Text(b.get("_0")?.as_str()?.to_string()),
            "scrollGesture" => InputEvent::ScrollGesture {
                phase: match b.get("_0")?.as_str()? {
                    "began" => ScrollPhase::Began,
                    "ended" => ScrollPhase::Ended,
                    "momentumBegan" => ScrollPhase::MomentumBegan,
                    "momentumEnded" => ScrollPhase::MomentumEnded,
                    _ => return None,
                },
                x: f("x")?,
                y: f("y")?,
            },
            "key" => InputEvent::Key {
                hid_usage: u16::try_from(b.get("hidUsage")?.as_u64()?).ok()?,
                down: b.get("down")?.as_bool()?,
                modifiers: b.get("modifiers")?.as_u64()?,
            },
            _ => return None,
        })
    }

    /// L'encodage côté appareil, pour le client de test.
    pub fn to_json(&self) -> Vec<u8> {
        let v = match self {
            InputEvent::Pointer { action, x, y } => json!({"pointer": {"_0": action.word(), "x": x, "y": y}}),
            InputEvent::Scroll { x, y, dx, dy } => json!({"scroll": {"x": x, "y": y, "dx": dx, "dy": dy}}),
            InputEvent::Text(t) => json!({"text": {"_0": t}}),
            InputEvent::ScrollGesture { phase, x, y } => {
                let p = match phase {
                    ScrollPhase::Began => "began",
                    ScrollPhase::Ended => "ended",
                    ScrollPhase::MomentumBegan => "momentumBegan",
                    ScrollPhase::MomentumEnded => "momentumEnded",
                };
                json!({"scrollGesture": {"_0": p, "x": x, "y": y}})
            }
            InputEvent::Key { hid_usage, down, modifiers } => {
                json!({"key": {"hidUsage": hid_usage, "down": down, "modifiers": modifiers}})
            }
        };
        serde_json::to_vec(&v).unwrap()
    }
}

pub fn encode<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

pub fn decode<'a, T: Deserialize<'a>>(b: &'a [u8]) -> Option<T> {
    serde_json::from_slice(b).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Les vecteurs viennent des tests de Sill (Tests/checks/…) ou de ses commentaires.
    #[test]
    fn swift_pointer_and_key_vectors() {
        assert_eq!(
            InputEvent::from_json(br#"{"pointer":{"_0":"move","x":0.25,"y":0.5}}"#),
            Some(InputEvent::Pointer { action: PointerAction::Move, x: 0.25, y: 0.5 })
        );
        assert_eq!(
            InputEvent::from_json(br#"{"key":{"down":true,"hidUsage":4,"modifiers":0}}"#),
            Some(InputEvent::Key { hid_usage: 4, down: true, modifiers: 0 })
        );
        assert_eq!(
            InputEvent::from_json(br#"{"key":{"hidUsage":44,"down":false,"modifiers":1048576}}"#),
            Some(InputEvent::Key { hid_usage: 44, down: false, modifiers: 1 << 20 })
        );
        // Un entier là où un Double est attendu reste un nombre valide.
        assert_eq!(
            InputEvent::from_json(br#"{"scroll":{"x":0,"y":1,"dx":0,"dy":-0.5}}"#),
            Some(InputEvent::Scroll { x: 0.0, y: 1.0, dx: 0.0, dy: -0.5 })
        );
        assert_eq!(InputEvent::from_json(r#"{"text":{"_0":"é\n"}}"#.as_bytes()), Some(InputEvent::Text("é\n".into())));
        assert_eq!(InputEvent::from_json(br#"{"teleport":{}}"#), None);
    }

    #[test]
    fn input_round_trip() {
        for e in [
            InputEvent::Pointer { action: PointerAction::RightUp, x: 0.1, y: 0.9 },
            InputEvent::ScrollGesture { phase: ScrollPhase::MomentumBegan, x: 0.5, y: 0.5 },
            InputEvent::Key { hid_usage: 0xE3, down: true, modifiers: 1 << 20 },
        ] {
            assert_eq!(InputEvent::from_json(&e.to_json()), Some(e));
        }
    }

    #[test]
    fn stream_source_shapes() {
        assert_eq!(serde_json::to_string(&StreamSource::Desktop).unwrap(), r#"{"desktop":{}}"#);
        assert_eq!(serde_json::to_string(&StreamSource::Window(7)).unwrap(), r#"{"window":{"_0":7}}"#);
        assert_eq!(decode::<StreamSource>(br#"{"none":{}}"#), Some(StreamSource::None));
        assert_eq!(decode::<StreamSource>(br#"{"window":{"_0":4294967295}}"#), Some(StreamSource::Window(u32::MAX)));
        assert_eq!(decode::<StreamSource>(br#"{"window":{"_0":4294967296}}"#), None);
    }

    #[test]
    fn window_list_matches_the_mac() {
        let l = WindowList {
            mac_name: "PC".into(),
            windows: vec![WindowInfo {
                id: 1,
                title: "T".into(),
                app_name: "A".into(),
                bundle_id: "b".into(),
                width: 800,
                height: 600,
            }],
            active: StreamSource::Desktop,
            launch_id: Some("L".into()),
            host_version: Some("0.6.1".into()),
            protocol: Some(1),
            gestures: None,
            clipboard: None,
            key_repeat: None,
            locked: None,
            off_console: None,
        };
        assert_eq!(
            String::from_utf8(encode(&l)).unwrap(),
            r#"{"macName":"PC","windows":[{"id":1,"title":"T","appName":"A","bundleID":"b","width":800,"height":600}],"active":{"desktop":{}},"launchID":"L","hostVersion":"0.6.1","protocol":1}"#
        );
    }

    #[test]
    fn hello_and_viewport() {
        let h: Hello = decode(
            br#"{"appVersion":"0.6.1","build":"42","protocol":1,"device":"iPad (iPad14,1)","audio":["aac-eld"]}"#,
        )
        .unwrap();
        assert_eq!(h.device.as_deref(), Some("iPad (iPad14,1)"));
        assert_eq!(decode::<Hello>(b"{}"), Some(Hello::default()));
        let v: Viewport = decode(br#"{"width":1180,"height":820,"scale":1,"fps":120}"#).unwrap();
        assert_eq!(v.fps, Some(120));
        assert!(v.refusal().is_none());
        assert!(Viewport { width: 1e300, height: 1.0, scale: None, local_cursor: None, fps: None }.refusal().is_some());
    }
}
