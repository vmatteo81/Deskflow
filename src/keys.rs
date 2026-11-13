//! Keycode canonici (Linux evdev), nomi leggibili per la hotkey e
//! conversione da/verso gli scancode Windows (set 1). Usando i tasti fisici
//! il layout (italiano, US, ...) lo decide il PC che riceve.

/// Insieme di tasti premuti (bitmap per keycode < 768).
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct KeySet([u64; 12]);

impl KeySet {
    pub fn insert(&mut self, code: u16) -> bool {
        let (i, b) = Self::pos(code);
        if i >= self.0.len() {
            return false;
        }
        let was = self.0[i] & b != 0;
        self.0[i] |= b;
        !was
    }
    pub fn remove(&mut self, code: u16) -> bool {
        let (i, b) = Self::pos(code);
        if i >= self.0.len() {
            return false;
        }
        let was = self.0[i] & b != 0;
        self.0[i] &= !b;
        was
    }
    pub fn contains(&self, code: u16) -> bool {
        let (i, b) = Self::pos(code);
        i < self.0.len() && self.0[i] & b != 0
    }
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|w| *w == 0)
    }
    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        (0..(self.0.len() * 64) as u16).filter(|c| self.contains(*c))
    }
    pub fn clear(&mut self) {
        self.0 = [0; 12];
    }
    fn pos(code: u16) -> (usize, u64) {
        ((code / 64) as usize, 1u64 << (code % 64))
    }
}

pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_RIGHTSHIFT: u16 = 54;
pub const KEY_LEFTALT: u16 = 56;
pub const KEY_NUMLOCK: u16 = 69;
#[cfg(test)]
pub const KEY_SCROLLLOCK: u16 = 70;
pub const KEY_RIGHTCTRL: u16 = 97;
pub const KEY_RIGHTALT: u16 = 100;
pub const KEY_PAUSE: u16 = 119;
pub const KEY_LEFTMETA: u16 = 125;
pub const KEY_RIGHTMETA: u16 = 126;

pub fn is_modifier(code: u16) -> bool {
    matches!(
        code,
        KEY_LEFTCTRL
            | KEY_LEFTSHIFT
            | KEY_RIGHTSHIFT
            | KEY_LEFTALT
            | KEY_RIGHTCTRL
            | KEY_RIGHTALT
            | KEY_LEFTMETA
            | KEY_RIGHTMETA
    )
}

/// Tasti del blocco principale che producono un carattere (cifre, lettere,
/// punteggiatura, il tasto `< >`). Spazio, tastierino e tasti di controllo
/// restano tasti fisici.
pub fn is_text_key(code: u16) -> bool {
    matches!(code, 2..=13 | 16..=27 | 30..=41 | 43..=53 | 86)
}

/// Livello (modificatori) con cui un tasto produce un carattere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Level {
    pub shift: bool,
    pub altgr: bool,
}

/// Tasto fisico + livello che sul PC di destinazione produce un carattere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharKey {
    pub code: u16,
    pub level: Level,
}

/// Nomi accettati nella configurazione della hotkey.
const NAMES: &[(&str, u16)] = &[
    ("Esc", 1),
    ("1", 2),
    ("2", 3),
    ("3", 4),
    ("4", 5),
    ("5", 6),
    ("6", 7),
    ("7", 8),
    ("8", 9),
    ("9", 10),
    ("0", 11),
    ("Minus", 12),
    ("Equal", 13),
    ("Backspace", 14),
    ("Tab", 15),
    ("Q", 16),
    ("W", 17),
    ("E", 18),
    ("R", 19),
    ("T", 20),
    ("Y", 21),
    ("U", 22),
    ("I", 23),
    ("O", 24),
    ("P", 25),
    ("LeftBrace", 26),
    ("RightBrace", 27),
    ("Enter", 28),
    ("LeftCtrl", 29),
    ("A", 30),
    ("S", 31),
    ("D", 32),
    ("F", 33),
    ("G", 34),
    ("H", 35),
    ("J", 36),
    ("K", 37),
    ("L", 38),
    ("Semicolon", 39),
    ("Apostrophe", 40),
    ("Grave", 41),
    ("LeftShift", 42),
    ("Backslash", 43),
    ("Z", 44),
    ("X", 45),
    ("C", 46),
    ("V", 47),
    ("B", 48),
    ("N", 49),
    ("M", 50),
    ("Comma", 51),
    ("Dot", 52),
    ("Slash", 53),
    ("RightShift", 54),
    ("KpAsterisk", 55),
    ("LeftAlt", 56),
    ("Space", 57),
    ("CapsLock", 58),
    ("F1", 59),
    ("F2", 60),
    ("F3", 61),
    ("F4", 62),
    ("F5", 63),
    ("F6", 64),
    ("F7", 65),
    ("F8", 66),
    ("F9", 67),
    ("F10", 68),
    ("NumLock", 69),
    ("ScrollLock", 70),
    ("F11", 87),
    ("F12", 88),
    ("RightCtrl", 97),
    ("PrintScreen", 99),
    ("RightAlt", 100),
    ("Home", 102),
    ("Up", 103),
    ("PageUp", 104),
    ("Left", 105),
    ("Right", 106),
    ("End", 107),
    ("Down", 108),
    ("PageDown", 109),
    ("Insert", 110),
    ("Delete", 111),
    ("Pause", 119),
    ("LeftMeta", 125),
    ("RightMeta", 126),
    ("Menu", 127),
    ("F13", 183),
    ("F14", 184),
    ("F15", 185),
];

pub fn code_from_name(name: &str) -> Option<u16> {
    let alias = match name.to_ascii_lowercase().as_str() {
        "ctrl" | "control" => "LeftCtrl",
        "shift" => "LeftShift",
        "alt" => "LeftAlt",
        "altgr" => "RightAlt",
        "win" | "super" | "meta" | "cmd" => "LeftMeta",
        "scroll" | "scrlk" => "ScrollLock",
        "escape" => "Esc",
        _ => name,
    }
    .to_string();
    NAMES
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(&alias))
        .map(|(_, c)| *c)
}

pub fn all_names() -> impl Iterator<Item = &'static str> {
    NAMES.iter().map(|(n, _)| *n)
}

/// Come iniettare un tasto su Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinKey {
    Scan {
        scan: u16,
        extended: bool,
    },
    /// Tasti senza uno scancode semplice (Pause): si usa il virtual-key.
    Vk(u16),
}

const VK_PAUSE: u32 = 0x13;
const VK_NUMLOCK: u32 = 0x90;

/// Scancode con prefisso E0 → keycode evdev.
const EXTENDED: &[(u16, u16)] = &[
    (0x1C, 96),  // KpEnter
    (0x1D, 97),  // RightCtrl
    (0x35, 98),  // KpSlash
    (0x37, 99),  // PrintScreen
    (0x38, 100), // RightAlt
    (0x47, 102), // Home
    (0x48, 103), // Up
    (0x49, 104), // PageUp
    (0x4B, 105), // Left
    (0x4D, 106), // Right
    (0x4F, 107), // End
    (0x50, 108), // Down
    (0x51, 109), // PageDown
    (0x52, 110), // Insert
    (0x53, 111), // Delete
    (0x5B, 125), // LeftMeta
    (0x5C, 126), // RightMeta
    (0x5D, 127), // Menu
    (0x20, 113), // Mute
    (0x2E, 114), // VolumeDown
    (0x30, 115), // VolumeUp
    (0x22, 164), // PlayPause
    (0x24, 166), // Stop
    (0x10, 165), // PreviousSong
    (0x19, 163), // NextSong
    (0x45, 69),  // NumLock (l'hook LL lo marca come esteso)
];

/// Scancode non estesi diversi dall'identità.
const PLAIN_SPECIAL: &[(u16, u16)] = &[
    (0x56, 86),  // 102nd (tasto < > italiano)
    (0x57, 87),  // F11
    (0x58, 88),  // F12
    (0x64, 183), // F13
    (0x65, 184),
    (0x66, 185),
    (0x70, 93),  // Katakana/Hiragana
    (0x73, 89),  // RO
    (0x79, 92),  // Henkan
    (0x7B, 94),  // Muhenkan
    (0x7D, 124), // Yen
];

/// Dall'hook tastiera Windows (vkCode, scanCode, flag esteso) al keycode evdev.
pub fn win_to_evdev(vk: u32, scan: u32, extended: bool) -> Option<u16> {
    if vk == VK_PAUSE {
        return Some(KEY_PAUSE);
    }
    if vk == VK_NUMLOCK {
        return Some(KEY_NUMLOCK);
    }
    let scan = scan as u16;
    if extended {
        return EXTENDED.iter().find(|(s, _)| *s == scan).map(|(_, c)| *c);
    }
    if let Some((_, c)) = PLAIN_SPECIAL.iter().find(|(s, _)| *s == scan) {
        return Some(*c);
    }
    if (1..=0x53).contains(&scan) {
        return Some(scan);
    }
    None
}

/// Dal keycode evdev al modo di iniettarlo su Windows.
pub fn evdev_to_win(code: u16) -> Option<WinKey> {
    if code == KEY_PAUSE {
        return Some(WinKey::Vk(VK_PAUSE as u16));
    }
    if let Some((s, _)) = EXTENDED.iter().find(|(_, c)| *c == code) {
        return Some(WinKey::Scan {
            scan: *s,
            extended: true,
        });
    }
    if let Some((s, _)) = PLAIN_SPECIAL.iter().find(|(_, c)| *c == code) {
        return Some(WinKey::Scan {
            scan: *s,
            extended: false,
        });
    }
    if (1..=0x53).contains(&code) {
        return Some(WinKey::Scan {
            scan: code,
            extended: false,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyset_basics() {
        let mut s = KeySet::default();
        assert!(s.is_empty());
        assert!(s.insert(30));
        assert!(!s.insert(30));
        assert!(s.insert(700));
        assert!(!s.insert(9999));
        assert_eq!(s.iter().collect::<Vec<_>>(), vec![30, 700]);
        assert!(s.remove(30));
        assert!(!s.remove(30));
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn text_keys() {
        assert!(is_text_key(30)); // A
        assert!(is_text_key(39)); // ò
        assert!(is_text_key(86)); // < >
        assert!(!is_text_key(57)); // spazio
        assert!(!is_text_key(28)); // Invio
        assert!(!is_text_key(KEY_LEFTSHIFT));
        assert!(!is_text_key(72)); // tastierino 8
    }

    #[test]
    fn names() {
        assert_eq!(code_from_name("scrolllock"), Some(KEY_SCROLLLOCK));
        assert_eq!(code_from_name("ctrl"), Some(KEY_LEFTCTRL));
        assert_eq!(code_from_name("RightCtrl"), Some(KEY_RIGHTCTRL));
        assert_eq!(code_from_name("Win"), Some(KEY_LEFTMETA));
        assert_eq!(code_from_name("boh"), None);
    }

    #[test]
    fn windows_roundtrip_for_every_mapped_key() {
        for code in 1u16..256 {
            let Some(w) = evdev_to_win(code) else {
                continue;
            };
            let back = match w {
                WinKey::Scan { scan, extended } => win_to_evdev(0, scan as u32, extended),
                WinKey::Vk(vk) => win_to_evdev(vk as u32, 0, false),
            };
            assert_eq!(back, Some(code), "keycode {code} -> {w:?}");
        }
    }

    #[test]
    fn windows_specific_keys() {
        // Freccia su: E0 48
        assert_eq!(win_to_evdev(0x26, 0x48, true), Some(103));
        // Tastierino 8: 48 senza E0
        assert_eq!(win_to_evdev(0x68, 0x48, false), Some(72));
        assert_eq!(win_to_evdev(0x13, 0x45, false), Some(KEY_PAUSE));
        assert_eq!(win_to_evdev(0x90, 0x45, true), Some(KEY_NUMLOCK));
        assert_eq!(win_to_evdev(0x41, 0x1E, false), Some(30)); // A
        assert_eq!(
            evdev_to_win(97),
            Some(WinKey::Scan {
                scan: 0x1D,
                extended: true
            })
        );
        // Shift "finto" che Windows genera col tastierino: va ignorato.
        assert_eq!(win_to_evdev(0xA0, 0x2A, true), None);
    }
}
