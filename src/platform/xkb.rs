//! Layout di tastiera su Linux tramite libxkbcommon (caricata a runtime).
//! Dal layout attivo (GNOME, KDE o di sistema) si costruiscono due tabelle:
//! tasto+livello → carattere (server) e carattere → tasto+livello (client).

use std::collections::HashMap;
use std::ffi::CString;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use xkbcommon_dl::{self as xkb, xkb_context_flags, xkb_keymap_compile_flags};

use crate::keys::{is_text_key, CharKey, Level, KEY_RIGHTALT};

const ISO_LEVEL3_SHIFT: u32 = 0xfe03;

const LEVELS: [Level; 4] = [
    Level {
        shift: false,
        altgr: false,
    },
    Level {
        shift: true,
        altgr: false,
    },
    Level {
        shift: false,
        altgr: true,
    },
    Level {
        shift: true,
        altgr: true,
    },
];

/// Layout e variante XKB, es. ("it", "") o ("us", "intl").
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Names {
    pub layout: String,
    pub variant: String,
}

#[derive(Debug, Default)]
pub struct Keymap {
    pub names: Names,
    forward: HashMap<(u16, Level), char>,
    reverse: HashMap<char, CharKey>,
}

impl Keymap {
    pub fn char_for(&self, code: u16, level: Level) -> Option<char> {
        self.forward.get(&(code, level)).copied()
    }

    pub fn char_key(&self, ch: char) -> Option<CharKey> {
        self.reverse.get(&ch).copied()
    }

    /// Compila il layout con libxkbcommon e lo esplora livello per livello.
    pub fn compile(names: &Names) -> Option<Keymap> {
        let x = xkb::xkbcommon_option()?;
        let layout = CString::new(names.layout.as_str()).ok()?;
        let variant = CString::new(names.variant.as_str()).ok()?;
        let rules = CString::new("evdev").ok()?;
        let rn = xkb::xkb_rule_names {
            rules: rules.as_ptr(),
            model: std::ptr::null(),
            layout: layout.as_ptr(),
            variant: variant.as_ptr(),
            options: std::ptr::null(),
        };
        let mut km = Keymap {
            names: names.clone(),
            ..Default::default()
        };
        // SAFETY: chiamate C su oggetti creati qui e distrutti prima di uscire;
        // i puntatori delle stringhe restano validi per tutta la funzione.
        unsafe {
            let ctx = (x.xkb_context_new)(xkb_context_flags::XKB_CONTEXT_NO_FLAGS);
            if ctx.is_null() {
                return None;
            }
            let keymap = (x.xkb_keymap_new_from_names)(
                ctx,
                &rn,
                xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            );
            if keymap.is_null() {
                (x.xkb_context_unref)(ctx);
                return None;
            }
            let state = (x.xkb_state_new)(keymap);
            if !state.is_null() {
                let idx = |n: &[u8]| (x.xkb_keymap_mod_get_index)(keymap, n.as_ptr() as *const _);
                let shift = idx(b"Shift\0");
                let mod5 = idx(b"Mod5\0");
                let bit = |i: u32| if i < 32 { 1u32 << i } else { 0 };
                (x.xkb_state_update_mask)(state, 0, 0, 0, 0, 0, 0);
                let has_l3 = mod5 < 32
                    && (x.xkb_state_key_get_one_sym)(state, KEY_RIGHTALT as u32 + 8)
                        == ISO_LEVEL3_SHIFT;
                for level in LEVELS {
                    if level.altgr && !has_l3 {
                        continue;
                    }
                    let mask = if level.shift { bit(shift) } else { 0 }
                        | if level.altgr { bit(mod5) } else { 0 };
                    (x.xkb_state_update_mask)(state, mask, 0, 0, 0, 0, 0);
                    for code in 1u16..=255 {
                        let u = (x.xkb_state_key_get_utf32)(state, code as u32 + 8);
                        let Some(ch) = char::from_u32(u).filter(|c| *c != '\0' && !c.is_control())
                        else {
                            continue;
                        };
                        km.insert(code, level, ch);
                    }
                }
                (x.xkb_state_unref)(state);
            }
            (x.xkb_keymap_unref)(keymap);
            (x.xkb_context_unref)(ctx);
        }
        (!km.forward.is_empty()).then_some(km)
    }

    fn insert(&mut self, code: u16, level: Level, ch: char) {
        if !is_text_key(code) && code != 57 {
            return; // tastierino & co. dipendono dal Bloc Num
        }
        self.forward.insert((code, level), ch);
        // I livelli si visitano dal più semplice: vince il primo tasto trovato.
        self.reverse.entry(ch).or_insert(CharKey { code, level });
    }
}

/// Keymap condivisa e ricaricata quando l'utente cambia layout.
#[derive(Clone)]
pub struct Shared(Arc<Mutex<Option<Keymap>>>);

impl Shared {
    pub fn start() -> Shared {
        let names = detect();
        let km = Keymap::compile(&names);
        match &km {
            Some(_) => log::info!("layout di tastiera: {}", describe(&names)),
            None => log::warn!(
                "libxkbcommon non disponibile o layout «{}» sconosciuto: uso i tasti fisici",
                describe(&names)
            ),
        }
        let shared = Shared(Arc::new(Mutex::new(km)));
        let s = shared.clone();
        let _ = std::thread::Builder::new()
            .name("ponte-layout".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_secs(5));
                let names = detect();
                let changed = s.lock().as_ref().is_none_or(|k| k.names != names);
                if changed {
                    if let Some(km) = Keymap::compile(&names) {
                        log::info!("layout di tastiera cambiato: {}", describe(&names));
                        *s.lock() = Some(km);
                    }
                }
            });
        shared
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Keymap>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn char_for(&self, code: u16, level: Level) -> Option<char> {
        self.lock().as_ref()?.char_for(code, level)
    }

    pub fn char_key(&self, ch: char) -> Option<CharKey> {
        self.lock().as_ref()?.char_key(ch)
    }
}

fn describe(n: &Names) -> String {
    if n.variant.is_empty() {
        n.layout.clone()
    } else {
        format!("{} ({})", n.layout, n.variant)
    }
}

// ------------------------------------------------------- rilevamento layout

/// Layout attivo: GNOME (sorgente in uso), KDE, poi configurazione di sistema.
pub fn detect() -> Names {
    if let Ok(l) = std::env::var("PONTE_XKB_LAYOUT") {
        return split_layout(&l);
    }
    for key in ["mru-sources", "sources"] {
        if let Some(out) = run(
            "gsettings",
            &["get", "org.gnome.desktop.input-sources", key],
        ) {
            if let Some(n) = parse_gsettings(&out) {
                return n;
            }
        }
    }
    if let Some(home) = dirs::config_dir() {
        if let Ok(t) = std::fs::read_to_string(home.join("kxkbrc")) {
            if let Some(n) = parse_kxkbrc(&t) {
                return n;
            }
        }
    }
    if let Ok(t) = std::fs::read_to_string("/etc/default/keyboard") {
        if let Some(n) = parse_shell_vars(&t, "XKBLAYOUT", "XKBVARIANT") {
            return n;
        }
    }
    if let Ok(t) = std::fs::read_to_string("/etc/vconsole.conf") {
        if let Some(n) = parse_shell_vars(&t, "XKBLAYOUT", "XKBVARIANT") {
            return n;
        }
    }
    if let Some(out) = run("localectl", &["status"]) {
        let get = |k: &str| {
            out.lines()
                .find_map(|l| l.trim().strip_prefix(k).map(|v| v.trim().to_string()))
        };
        if let Some(layout) = get("X11 Layout:") {
            return Names {
                layout: first(&layout),
                variant: get("X11 Variant:").map(|v| first(&v)).unwrap_or_default(),
            };
        }
    }
    Names {
        layout: "us".into(),
        variant: String::new(),
    }
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn first(list: &str) -> String {
    list.split(',').next().unwrap_or("").trim().to_string()
}

/// "it", "us+intl" → layout e variante.
fn split_layout(s: &str) -> Names {
    let (l, v) = s.split_once('+').unwrap_or((s, ""));
    Names {
        layout: l.trim().to_string(),
        variant: v.trim().to_string(),
    }
}

/// `[('xkb', 'us+intl'), ('ibus', 'mozc')]` → prima sorgente xkb.
fn parse_gsettings(out: &str) -> Option<Names> {
    let mut rest = out;
    while let Some(i) = rest.find("('xkb', '") {
        rest = &rest[i + 9..];
        let end = rest.find('\'')?;
        let n = split_layout(&rest[..end]);
        if !n.layout.is_empty() {
            return Some(n);
        }
    }
    None
}

fn parse_kxkbrc(t: &str) -> Option<Names> {
    let get = |k: &str| t.lines().find_map(|l| l.trim().strip_prefix(k).map(first));
    let layout = get("LayoutList=")?;
    (!layout.is_empty()).then(|| Names {
        layout,
        variant: get("VariantList=").unwrap_or_default(),
    })
}

fn parse_shell_vars(t: &str, lk: &str, vk: &str) -> Option<Names> {
    let get = |k: &str| {
        t.lines().find_map(|l| {
            let v = l.trim().strip_prefix(k)?.strip_prefix('=')?;
            Some(first(v.trim().trim_matches('"')))
        })
    };
    let layout = get(lk)?;
    (!layout.is_empty()).then(|| Names {
        layout,
        variant: get(vk).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Level = LEVELS[0];
    const SHIFT: Level = LEVELS[1];
    const ALTGR: Level = LEVELS[2];

    #[test]
    fn parses_layout_sources() {
        let n = |l: &str, v: &str| {
            Some(Names {
                layout: l.into(),
                variant: v.into(),
            })
        };
        assert_eq!(parse_gsettings("[('xkb', 'it')]\n"), n("it", ""));
        assert_eq!(
            parse_gsettings("[('ibus', 'mozc'), ('xkb', 'us+intl')]"),
            n("us", "intl")
        );
        assert_eq!(parse_gsettings("@a(ss) []"), None);
        assert_eq!(
            parse_kxkbrc("[Layout]\nLayoutList=de,us\nVariantList=nodeadkeys,\n"),
            n("de", "nodeadkeys")
        );
        assert_eq!(
            parse_shell_vars(
                "XKBMODEL=\"pc105\"\nXKBLAYOUT=\"it\"\nXKBVARIANT=\"\"\n",
                "XKBLAYOUT",
                "XKBVARIANT"
            ),
            n("it", "")
        );
    }

    #[test]
    fn italian_layout_tables() {
        let Some(km) = Keymap::compile(&split_layout("it")) else {
            eprintln!("libxkbcommon assente: test saltato");
            return;
        };
        assert_eq!(km.char_for(39, PLAIN), Some('ò'));
        assert_eq!(km.char_for(39, ALTGR), Some('@'));
        assert_eq!(km.char_for(3, SHIFT), Some('"'));
        assert_eq!(km.char_for(30, SHIFT), Some('A'));
        // '@' esiste con AltGr (su Linux sia AltGr+ò che AltGr+Q).
        let at = km.char_key('@').unwrap();
        assert_eq!(at.level, ALTGR);
        assert_eq!(km.char_for(at.code, at.level), Some('@'));
        assert_eq!(km.char_key('a').map(|k| k.code), Some(30));
        assert_eq!(km.char_key(' ').map(|k| k.code), Some(57));
        // '@' su un layout US è Shift+2.
        let us = Keymap::compile(&split_layout("us")).unwrap();
        assert_eq!(
            us.char_key('@'),
            Some(CharKey {
                code: 3,
                level: SHIFT
            })
        );
        assert_eq!(us.char_key('ò'), None);
        assert_eq!(us.char_for(39, ALTGR), None, "niente AltGr sul layout US");
    }
}
