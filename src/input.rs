//! Traduction des entrées de l'app en actions Windows. Portable, donc testée sous Linux.
//!
//! - Positions : fractions de l'image → pixels du rectangle de la source.
//! - Molette : fractions de l'image → unités WHEEL_DELTA, avec report des restes.
//! - Clavier : usage HID USB → code de balayage, ⌘ traduit en Ctrl (ou en touche Windows).
//! - ⌘Espace, que l'app envoie pour Spotlight, ouvre le menu Démarrer.

use crate::platform::{Action, Rect};
use crate::proto::msg::{InputEvent, PointerAction};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdMode {
    /// ⌘C devient Ctrl+C : les raccourcis du Mac marchent sous Windows.
    Ctrl,
    /// ⌘ devient la touche Windows.
    Win,
}

const HID_SPACE: u16 = 0x2C;
const HID_LCTRL: u16 = 0xE0;
const HID_LSHIFT: u16 = 0xE1;
const HID_LALT: u16 = 0xE2;
const HID_LGUI: u16 = 0xE3;
const HID_RCTRL: u16 = 0xE4;
const HID_RSHIFT: u16 = 0xE5;
const HID_RALT: u16 = 0xE6;
const HID_RGUI: u16 = 0xE7;

const SCAN_LWIN: (u16, bool) = (0x5B, true);

/// Usage HID (page clavier 0x07) → code de balayage jeu 1 et préfixe E0.
pub fn hid_to_scan(hid: u16) -> Option<(u16, bool)> {
    let n = |s: u16| Some((s, false));
    let e = |s: u16| Some((s, true));
    match hid {
        0x04 => n(0x1E),
        0x05 => n(0x30),
        0x06 => n(0x2E),
        0x07 => n(0x20),
        0x08 => n(0x12),
        0x09 => n(0x21),
        0x0A => n(0x22),
        0x0B => n(0x23),
        0x0C => n(0x17),
        0x0D => n(0x24),
        0x0E => n(0x25),
        0x0F => n(0x26),
        0x10 => n(0x32),
        0x11 => n(0x31),
        0x12 => n(0x18),
        0x13 => n(0x19),
        0x14 => n(0x10),
        0x15 => n(0x13),
        0x16 => n(0x1F),
        0x17 => n(0x14),
        0x18 => n(0x16),
        0x19 => n(0x2F),
        0x1A => n(0x11),
        0x1B => n(0x2D),
        0x1C => n(0x15),
        0x1D => n(0x2C),
        // 1 à 9 puis 0
        0x1E..=0x26 => n(hid - 0x1E + 0x02),
        0x27 => n(0x0B),
        0x28 => n(0x1C),
        0x29 => n(0x01),
        0x2A => n(0x0E),
        0x2B => n(0x0F),
        0x2C => n(0x39),
        0x2D => n(0x0C),
        0x2E => n(0x0D),
        0x2F => n(0x1A),
        0x30 => n(0x1B),
        0x31 => n(0x2B),
        0x32 => n(0x2B),
        0x33 => n(0x27),
        0x34 => n(0x28),
        0x35 => n(0x29),
        0x36 => n(0x33),
        0x37 => n(0x34),
        0x38 => n(0x35),
        0x39 => n(0x3A),
        // F1 à F10, F11, F12
        0x3A..=0x43 => n(hid - 0x3A + 0x3B),
        0x44 => n(0x57),
        0x45 => n(0x58),
        0x46 => e(0x37),
        0x47 => n(0x46),
        0x49 => e(0x52),
        0x4A => e(0x47),
        0x4B => e(0x49),
        0x4C => e(0x53),
        0x4D => e(0x4F),
        0x4E => e(0x51),
        0x4F => e(0x4D),
        0x50 => e(0x4B),
        0x51 => e(0x50),
        0x52 => e(0x48),
        // pavé numérique
        0x53 => n(0x45),
        0x54 => e(0x35),
        0x55 => n(0x37),
        0x56 => n(0x4A),
        0x57 => n(0x4E),
        0x58 => e(0x1C),
        0x59 => n(0x4F),
        0x5A => n(0x50),
        0x5B => n(0x51),
        0x5C => n(0x4B),
        0x5D => n(0x4C),
        0x5E => n(0x4D),
        0x5F => n(0x47),
        0x60 => n(0x48),
        0x61 => n(0x49),
        0x62 => n(0x52),
        0x63 => n(0x53),
        0x64 => n(0x56),
        0x65 => e(0x5D),
        0x67 => n(0x59),
        // F13 à F24
        0x68..=0x72 => n(hid - 0x68 + 0x64),
        0x73 => n(0x76),
        0x7F => e(0x20),
        0x80 => e(0x30),
        0x81 => e(0x2E),
        0x87 => n(0x73),
        0x88 => n(0x70),
        0x89 => n(0x7D),
        0x8A => n(0x79),
        0x8B => n(0x7B),
        HID_LCTRL => n(0x1D),
        HID_LSHIFT => n(0x2A),
        HID_LALT => n(0x38),
        HID_LGUI => e(0x5B),
        HID_RCTRL => e(0x1D),
        HID_RSHIFT => n(0x36),
        HID_RALT => e(0x38),
        HID_RGUI => e(0x5C),
        _ => None,
    }
}

fn is_cmd(hid: u16) -> bool {
    hid == HID_LGUI || hid == HID_RGUI
}

fn is_other_modifier(hid: u16) -> bool {
    matches!(hid, HID_LCTRL | HID_RCTRL | HID_LSHIFT | HID_RSHIFT | HID_LALT | HID_RALT)
}

/// L'état d'entrée d'un appareil : ce qu'il tient enfoncé, les restes de molette.
pub struct InputTranslator {
    cmd_mode: CmdMode,
    held_keys: BTreeSet<u16>,
    left_down: bool,
    right_down: bool,
    wheel_rest_v: f64,
    wheel_rest_h: f64,
    /// ⌘Espace a ouvert Démarrer : l'Espace et les ⌘ déjà relâchés ne repartent pas à leur relâchement.
    swallow_space_up: bool,
    swallow_cmd_up: BTreeSet<u16>,
}

/// Unités de molette par point logique : un cran (120) fait environ 57 points sous Windows.
const WHEEL_UNITS_PER_POINT: f64 = 120.0 / 57.0;

impl InputTranslator {
    pub fn new(cmd_mode: CmdMode) -> Self {
        InputTranslator {
            cmd_mode,
            held_keys: BTreeSet::new(),
            left_down: false,
            right_down: false,
            wheel_rest_v: 0.0,
            wheel_rest_h: 0.0,
            swallow_space_up: false,
            swallow_cmd_up: BTreeSet::new(),
        }
    }

    fn point(rect: &Rect, x: f64, y: f64) -> Action {
        let fx = x.clamp(0.0, 1.0);
        let fy = y.clamp(0.0, 1.0);
        let px = rect.x + (fx * rect.w).min(rect.w - 1.0).max(0.0);
        let py = rect.y + (fy * rect.h).min(rect.h - 1.0).max(0.0);
        Action::MoveTo { x: px.round() as i32, y: py.round() as i32 }
    }

    fn key_action(&self, hid: u16, down: bool) -> Option<Action> {
        let (scan, extended) = if is_cmd(hid) {
            match self.cmd_mode {
                CmdMode::Ctrl => hid_to_scan(if hid == HID_LGUI { HID_LCTRL } else { HID_RCTRL })?,
                CmdMode::Win => hid_to_scan(hid)?,
            }
        } else {
            hid_to_scan(hid)?
        };
        Some(Action::Key { scan, extended, down })
    }

    pub fn translate(&mut self, event: &InputEvent, rect: &Rect) -> Vec<Action> {
        match *event {
            InputEvent::Pointer { action, x, y } => {
                let mut out = vec![Self::point(rect, x, y)];
                match action {
                    PointerAction::Move => {}
                    PointerAction::LeftDown => {
                        self.left_down = true;
                        out.push(Action::Button { right: false, down: true });
                    }
                    PointerAction::LeftUp => {
                        self.left_down = false;
                        out.push(Action::Button { right: false, down: false });
                    }
                    PointerAction::RightDown => {
                        self.right_down = true;
                        out.push(Action::Button { right: true, down: true });
                    }
                    PointerAction::RightUp => {
                        self.right_down = false;
                        out.push(Action::Button { right: true, down: false });
                    }
                }
                out
            }
            InputEvent::Scroll { x, y, dx, dy } => {
                let points_v = dy * rect.h / rect.scale.max(0.5);
                let points_h = dx * rect.w / rect.scale.max(0.5);
                self.wheel_rest_v += points_v * WHEEL_UNITS_PER_POINT;
                // dx positif : le contenu va à droite, donc on défile vers la gauche.
                self.wheel_rest_h -= points_h * WHEEL_UNITS_PER_POINT;
                let v = self.wheel_rest_v.trunc();
                let h = self.wheel_rest_h.trunc();
                self.wheel_rest_v -= v;
                self.wheel_rest_h -= h;
                let mut out = vec![Self::point(rect, x, y)];
                if v != 0.0 || h != 0.0 {
                    out.push(Action::Wheel { vertical: v as i32, horizontal: h as i32 });
                }
                out
            }
            InputEvent::ScrollGesture { .. } => {
                // Windows n'a pas de phases de défilement : l'inertie arrive déjà en deltas.
                self.wheel_rest_v = 0.0;
                self.wheel_rest_h = 0.0;
                Vec::new()
            }
            InputEvent::Text(ref text) => text_actions(text),
            InputEvent::Key { hid_usage, down, .. } => self.key(hid_usage, down),
        }
    }

    fn key(&mut self, hid: u16, down: bool) -> Vec<Action> {
        let mut out = Vec::new();
        if down {
            let cmds: Vec<u16> = self.held_keys.iter().copied().filter(|&k| is_cmd(k)).collect();
            let others = self.held_keys.iter().any(|&k| is_other_modifier(k));
            if hid == HID_SPACE && self.cmd_mode == CmdMode::Ctrl && !cmds.is_empty() && !others {
                // ⌘Espace : relâcher les Ctrl qui tiennent lieu de ⌘, puis Windows seul.
                for k in cmds {
                    out.extend(self.key_action(k, false));
                    self.held_keys.remove(&k);
                    self.swallow_cmd_up.insert(k);
                }
                out.push(Action::Key { scan: SCAN_LWIN.0, extended: SCAN_LWIN.1, down: true });
                out.push(Action::Key { scan: SCAN_LWIN.0, extended: SCAN_LWIN.1, down: false });
                self.swallow_space_up = true;
                return out;
            }
            if let Some(a) = self.key_action(hid, true) {
                self.held_keys.insert(hid);
                out.push(a);
            }
        } else {
            if hid == HID_SPACE && self.swallow_space_up {
                self.swallow_space_up = false;
                return out;
            }
            if self.swallow_cmd_up.remove(&hid) {
                return out;
            }
            if self.held_keys.remove(&hid) {
                out.extend(self.key_action(hid, false));
            } else if let Some(a) = self.key_action(hid, false) {
                // Un relâchement sans appui connu part quand même : mieux vaut une touche relâchée
                // deux fois qu'une touche bloquée.
                out.push(a);
            }
        }
        out
    }

    /// Tout ce que cet appareil tient enfoncé, relâché : à la déconnexion ou au changement de source.
    pub fn release_all(&mut self) -> Vec<Action> {
        let mut out = Vec::new();
        if self.left_down {
            out.push(Action::Button { right: false, down: false });
        }
        if self.right_down {
            out.push(Action::Button { right: true, down: false });
        }
        let keys: Vec<u16> = self.held_keys.iter().rev().copied().collect();
        for k in keys {
            out.extend(self.key_action(k, false));
        }
        self.held_keys.clear();
        self.left_down = false;
        self.right_down = false;
        self.swallow_space_up = false;
        self.swallow_cmd_up.clear();
        out
    }
}

fn tap(scan: u16) -> [Action; 2] {
    [Action::Key { scan, extended: false, down: true }, Action::Key { scan, extended: false, down: false }]
}

/// Le texte du clavier logiciel : touches réelles pour Entrée, Tab et Retour arrière, Unicode sinon.
pub fn text_actions(text: &str) -> Vec<Action> {
    let mut out = Vec::new();
    for c in text.chars() {
        match c {
            '\n' | '\r' => out.extend(tap(0x1C)),
            '\t' => out.extend(tap(0x0F)),
            '\u{8}' => out.extend(tap(0x0E)),
            _ => {
                let mut buf = [0u16; 2];
                for &unit in c.encode_utf16(&mut buf).iter() {
                    out.push(Action::Unicode { unit, down: true });
                    out.push(Action::Unicode { unit, down: false });
                }
            }
        }
    }
    out
}

/// Les gestes à trois ou quatre doigts, en raccourcis Windows.
pub fn gesture_actions(name: &str) -> Vec<Action> {
    let k = |scan: u16, extended: bool, down: bool| Action::Key { scan, extended, down };
    let (lwin, lctrl, tab, d, left, right) =
        ((0x5B, true), (0x1D, false), (0x0F, false), (0x20, false), (0x4B, true), (0x4D, true));
    let chord = |keys: &[(u16, bool)]| {
        let mut v: Vec<Action> = keys.iter().map(|&(s, e)| k(s, e, true)).collect();
        v.extend(keys.iter().rev().map(|&(s, e)| k(s, e, false)));
        v
    };
    match name {
        // Mission Control et App Exposé : la vue des tâches.
        "swipeUp" | "swipeDown" => chord(&[lwin, tab]),
        // Le Space de droite / de gauche : le bureau virtuel suivant / précédent.
        "swipeLeft" => chord(&[lctrl, lwin, right]),
        "swipeRight" => chord(&[lctrl, lwin, left]),
        // Apps : le menu Démarrer.
        "pinch" => chord(&[lwin]),
        // Afficher le bureau.
        "spread" => chord(&[lwin, d]),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::msg::ScrollPhase;

    const R: Rect = Rect { x: 100.0, y: 50.0, w: 1000.0, h: 500.0, scale: 1.0 };

    fn key(hid: u16, down: bool) -> InputEvent {
        InputEvent::Key { hid_usage: hid, down, modifiers: 0 }
    }

    #[test]
    fn pointer_maps_into_the_rect() {
        let mut t = InputTranslator::new(CmdMode::Ctrl);
        let a = t.translate(&InputEvent::Pointer { action: PointerAction::LeftDown, x: 0.5, y: 0.5 }, &R);
        assert_eq!(a, vec![Action::MoveTo { x: 600, y: 300 }, Action::Button { right: false, down: true }]);
        // Bornée au rectangle, dernier pixel compris.
        let a = t.translate(&InputEvent::Pointer { action: PointerAction::Move, x: 1.5, y: -1.0 }, &R);
        assert_eq!(a, vec![Action::MoveTo { x: 1099, y: 50 }]);
        assert_eq!(t.release_all(), vec![Action::Button { right: false, down: false }]);
        assert!(t.release_all().is_empty());
    }

    #[test]
    fn command_becomes_control() {
        let mut t = InputTranslator::new(CmdMode::Ctrl);
        let mut all = Vec::new();
        for e in [key(HID_LGUI, true), key(0x19, true), key(0x19, false), key(HID_LGUI, false)] {
            all.extend(t.translate(&e, &R));
        }
        assert_eq!(
            all,
            vec![
                Action::Key { scan: 0x1D, extended: false, down: true },
                Action::Key { scan: 0x2F, extended: false, down: true },
                Action::Key { scan: 0x2F, extended: false, down: false },
                Action::Key { scan: 0x1D, extended: false, down: false },
            ]
        );
    }

    #[test]
    fn command_space_opens_start_once() {
        let mut t = InputTranslator::new(CmdMode::Ctrl);
        let mut all = Vec::new();
        for e in [key(HID_LGUI, true), key(HID_SPACE, true), key(HID_SPACE, false), key(HID_LGUI, false)] {
            all.extend(t.translate(&e, &R));
        }
        assert_eq!(
            all,
            vec![
                Action::Key { scan: 0x1D, extended: false, down: true },
                Action::Key { scan: 0x1D, extended: false, down: false },
                Action::Key { scan: 0x5B, extended: true, down: true },
                Action::Key { scan: 0x5B, extended: true, down: false },
            ]
        );
        // Ensuite, tout redevient normal.
        assert_eq!(
            t.translate(&key(HID_SPACE, true), &R),
            vec![Action::Key { scan: 0x39, extended: false, down: true }]
        );
    }

    #[test]
    fn shift_command_space_is_not_spotlight() {
        let mut t = InputTranslator::new(CmdMode::Ctrl);
        t.translate(&key(HID_LSHIFT, true), &R);
        t.translate(&key(HID_LGUI, true), &R);
        assert_eq!(
            t.translate(&key(HID_SPACE, true), &R),
            vec![Action::Key { scan: 0x39, extended: false, down: true }]
        );
        assert_eq!(t.release_all().len(), 3);
    }

    #[test]
    fn wheel_accumulates_and_signs() {
        let mut t = InputTranslator::new(CmdMode::Ctrl);
        // 0,01 de 500 px = 5 points, soit 10,5 unités : 10 maintenant, le reste plus tard.
        let a = t.translate(&InputEvent::Scroll { x: 0.5, y: 0.5, dx: 0.0, dy: 0.01 }, &R);
        assert_eq!(a[1], Action::Wheel { vertical: 10, horizontal: 0 });
        let a = t.translate(&InputEvent::Scroll { x: 0.5, y: 0.5, dx: 0.0, dy: 0.01 }, &R);
        assert_eq!(a[1], Action::Wheel { vertical: 11, horizontal: 0 });
        // Contenu poussé vers la droite : molette horizontale négative.
        let a = t.translate(&InputEvent::Scroll { x: 0.5, y: 0.5, dx: 0.01, dy: 0.0 }, &R);
        assert!(matches!(a[1], Action::Wheel { vertical: 0, horizontal: h } if h < 0));
        assert!(t.translate(&InputEvent::ScrollGesture { phase: ScrollPhase::Ended, x: 0.0, y: 0.0 }, &R).is_empty());
    }

    #[test]
    fn text_uses_real_keys_for_controls() {
        let a = text_actions("a\n😀");
        assert_eq!(a.len(), 2 + 2 + 4);
        assert_eq!(a[2], Action::Key { scan: 0x1C, extended: false, down: true });
        assert!(matches!(a[4], Action::Unicode { unit: 0xD83D, down: true }));
    }

    #[test]
    fn scan_table_covers_letters_digits_and_modifiers() {
        for hid in (0x04..=0x39).chain(0xE0..=0xE7) {
            assert!(hid_to_scan(hid).is_some(), "HID {hid:#x}");
        }
        assert_eq!(hid_to_scan(0x27), Some((0x0B, false))); // 0
        assert_eq!(hid_to_scan(0x45), Some((0x58, false))); // F12
        assert_eq!(hid_to_scan(0x50), Some((0x4B, true))); // flèche gauche
    }

    #[test]
    fn gestures_are_balanced_chords() {
        for g in ["swipeUp", "swipeDown", "swipeLeft", "swipeRight", "pinch", "spread"] {
            let a = gesture_actions(g);
            let downs = a.iter().filter(|x| matches!(x, Action::Key { down: true, .. })).count();
            assert!(downs > 0 && downs * 2 == a.len(), "{g}");
        }
        assert!(gesture_actions("teleport").is_empty());
    }
}
