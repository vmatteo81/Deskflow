//! Motore del server: decide evento per evento se l'input resta locale o va
//! al client, gestisce bordi, hotkey e tasti "a metà" durante i passaggi.
//! È indipendente dalla piattaforma (testato con una piattaforma finta).

use std::net::TcpStream;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use crate::keys::KeySet;
use crate::platform::{Rect, ServerPlatform};
use crate::protocol::{along_to_frac, frac_to_along, Button, Msg, Side};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Input {
    /// Movimento relativo grezzo (Linux evdev).
    #[cfg_attr(windows, allow(dead_code))]
    Motion {
        dx: f64,
        dy: f64,
    },
    /// Posizione assoluta proposta dal sistema (hook Windows).
    #[cfg_attr(not(windows), allow(dead_code))]
    MotionAbs {
        x: i32,
        y: i32,
    },
    Button {
        button: Button,
        down: bool,
    },
    Wheel {
        dx: i16,
        dy: i16,
    },
    Key {
        code: u16,
        down: bool,
        repeat: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Lascia passare l'evento al sistema locale.
    Pass,
    /// Blocca l'evento (è stato inoltrato al client o consumato).
    Swallow,
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub side: Side,
    pub edge_switch: bool,
    pub hotkeys: Vec<Vec<u16>>,
    pub speed: f64,
    pub accel: f64,
    pub lock_while_dragging: bool,
}

pub struct ClientLink {
    pub id: u64,
    pub tx: Sender<Msg>,
    pub name: String,
    /// IP del client.
    pub addr: String,
    pub width: u32,
    pub height: u32,
    /// Copia del socket per chiuderlo se arriva un altro client.
    pub stream: Option<TcpStream>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Entry {
    Edge(f64),
    Center,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Exit {
    Hotkey,
    ClientLeft(f64),
    Disconnected,
}

const SWITCH_COOLDOWN: Duration = Duration::from_millis(200);

#[derive(Default)]
struct Inner {
    remote: bool,
    client: Option<ClientLink>,
    pressed: KeySet,
    local_keys: KeySet,
    remote_keys: KeySet,
    swallow_up: KeySet,
    local_buttons: u8,
    remote_buttons: u8,
    kbd_captured: bool,
    abs_source: bool,
    last_abs: Option<(i32, i32)>,
    anchor: Option<(i32, i32)>,
    return_pos: Option<(i32, i32)>,
    rem: (f64, f64),
    cooldown_until: Option<Instant>,
}

pub struct Engine {
    inner: Mutex<Inner>,
    platform: Arc<dyn ServerPlatform>,
    cfg: Settings,
}

impl Engine {
    pub fn new(platform: Arc<dyn ServerPlatform>, cfg: Settings) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            platform,
            cfg,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        // Un panic altrove non deve bloccare per sempre l'input.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(test)]
    pub fn is_remote(&self) -> bool {
        self.lock().remote
    }

    pub fn input(&self, ev: Input) -> Verdict {
        let mut g = self.lock();
        let s = &mut *g;
        match ev {
            Input::Key { code, down, repeat } => self.on_key(s, code, down, repeat),
            Input::Button { button, down } => self.on_button(s, button, down),
            Input::Wheel { dx, dy } => {
                if s.remote {
                    send(s, Msg::Wheel { dx, dy });
                    Verdict::Swallow
                } else {
                    Verdict::Pass
                }
            }
            Input::Motion { dx, dy } => self.on_motion(s, dx, dy),
            Input::MotionAbs { x, y } => self.on_motion_abs(s, x, y),
        }
    }

    // ---------------------------------------------------------------- tastiera

    fn on_key(&self, s: &mut Inner, code: u16, down: bool, repeat: bool) -> Verdict {
        if down && !repeat {
            s.pressed.insert(code);
        } else if !down {
            s.pressed.remove(code);
        }

        if down && !repeat && self.hotkey_hit(&s.pressed, code) {
            if s.remote {
                self.go_local(s, Exit::Hotkey);
            } else if s.client.is_some() {
                self.go_remote(s, Entry::Center);
            }
            s.swallow_up.insert(code);
            return Verdict::Swallow;
        }
        if s.swallow_up.contains(code) {
            if !down {
                s.swallow_up.remove(code);
            }
            return Verdict::Swallow;
        }

        if !s.remote {
            if down {
                s.local_keys.insert(code);
            } else {
                s.local_keys.remove(code);
            }
            return Verdict::Pass;
        }

        // Remoto: un tasto premuto prima del passaggio si rilascia in locale.
        if s.local_keys.contains(code) {
            if !down {
                s.local_keys.remove(code);
                self.maybe_capture_keyboards(s);
            }
            return Verdict::Pass;
        }
        if down {
            s.remote_keys.insert(code);
            // Comanda il layout di questo PC: si invia il carattere, non il tasto.
            if let Some(ch) = self.text_for(&s.pressed, code) {
                send(s, Msg::Text { code, ch });
                return Verdict::Swallow;
            }
        } else if !s.remote_keys.remove(code) {
            return Verdict::Swallow;
        }
        send(s, Msg::Key { code, down });
        Verdict::Swallow
    }

    /// Carattere da scrivere sul client per `code`, se è digitazione e non
    /// una scorciatoia (Ctrl+C, Alt+F, Win+E… restano tasti fisici).
    fn text_for(&self, pressed: &KeySet, code: u16) -> Option<char> {
        use crate::keys::*;
        if !is_text_key(code) {
            return None;
        }
        let any = |a: u16, b: u16| pressed.contains(a) || pressed.contains(b);
        let shift = any(KEY_LEFTSHIFT, KEY_RIGHTSHIFT);
        let ctrl = any(KEY_LEFTCTRL, KEY_RIGHTCTRL);
        let lalt = pressed.contains(KEY_LEFTALT);
        let altgr = pressed.contains(KEY_RIGHTALT) || (ctrl && lalt);
        if any(KEY_LEFTMETA, KEY_RIGHTMETA) || (!altgr && (ctrl || lalt)) {
            return None;
        }
        self.platform
            .char_for(code, Level { shift, altgr })
            .filter(|c| !c.is_control())
    }

    fn hotkey_hit(&self, pressed: &KeySet, code: u16) -> bool {
        // Tutti i tasti della combo premuti e nessun modificatore in più
        // (così Ctrl+ScrollLock non scatta, ma A tenuto premuto + ScrollLock sì).
        self.cfg.hotkeys.iter().any(|combo| {
            combo.contains(&code)
                && combo.iter().all(|k| pressed.contains(*k))
                && pressed
                    .iter()
                    .all(|k| combo.contains(&k) || !crate::keys::is_modifier(k))
        })
    }

    fn maybe_capture_keyboards(&self, s: &mut Inner) {
        if s.remote && !s.kbd_captured && s.local_keys.is_empty() {
            s.kbd_captured = true;
            self.platform.capture(true, true);
        }
    }

    // ------------------------------------------------------------------- mouse

    fn on_button(&self, s: &mut Inner, button: Button, down: bool) -> Verdict {
        let bit = button.bit();
        if !s.remote {
            if down {
                s.local_buttons |= bit;
            } else {
                s.local_buttons &= !bit;
            }
            return Verdict::Pass;
        }
        if !down && s.local_buttons & bit != 0 {
            s.local_buttons &= !bit;
            return Verdict::Pass;
        }
        if down {
            s.remote_buttons |= bit;
        } else if s.remote_buttons & bit == 0 {
            return Verdict::Swallow;
        } else {
            s.remote_buttons &= !bit;
        }
        send(s, Msg::Button { button, down });
        Verdict::Swallow
    }

    fn can_switch_by_edge(&self, s: &Inner) -> bool {
        self.cfg.edge_switch
            && s.client.is_some()
            && !(self.cfg.lock_while_dragging && s.local_buttons != 0)
            && s.cooldown_until.is_none_or(|t| Instant::now() >= t)
    }

    fn on_motion(&self, s: &mut Inner, dx: f64, dy: f64) -> Verdict {
        if s.remote {
            self.forward_motion(s, dx, dy, true);
            return Verdict::Swallow;
        }
        if self.can_switch_by_edge(s) && toward(self.cfg.side, dx, dy) {
            if let Some(p) = self.platform.cursor_pos() {
                let b = self.platform.bounds();
                if at_edge(self.cfg.side, b, p) {
                    self.go_remote(s, Entry::Edge(along_edge(self.cfg.side, b, p)));
                    return Verdict::Swallow;
                }
            }
        }
        Verdict::Pass
    }

    fn on_motion_abs(&self, s: &mut Inner, x: i32, y: i32) -> Verdict {
        s.abs_source = true;
        if s.remote {
            let (ax, ay) = s.anchor.unwrap_or((x, y));
            let (dx, dy) = (x - ax, y - ay);
            if dx != 0 || dy != 0 {
                self.forward_motion(s, dx as f64, dy as f64, false);
            }
            return Verdict::Swallow;
        }
        s.last_abs = Some((x, y));
        if self.can_switch_by_edge(s) {
            let b = self.platform.bounds();
            if at_edge(self.cfg.side, b, (x, y)) {
                self.go_remote(s, Entry::Edge(along_edge(self.cfg.side, b, (x, y))));
                return Verdict::Swallow;
            }
        }
        Verdict::Pass
    }

    fn forward_motion(&self, s: &mut Inner, dx: f64, dy: f64, accel: bool) {
        let mut f = self.cfg.speed;
        if accel && self.cfg.accel > 0.0 {
            let v = (dx * dx + dy * dy).sqrt();
            f *= 1.0 + self.cfg.accel * ((v - 2.0).max(0.0) / 8.0).min(2.5);
        }
        let x = dx * f + s.rem.0;
        let y = dy * f + s.rem.1;
        let (ix, iy) = (x.trunc(), y.trunc());
        s.rem = (x - ix, y - iy);
        if ix != 0.0 || iy != 0.0 {
            let c = |v: f64| v.clamp(i16::MIN as f64, i16::MAX as f64) as i16;
            send(
                s,
                Msg::Move {
                    dx: c(ix),
                    dy: c(iy),
                },
            );
        }
    }

    // --------------------------------------------------------------- passaggi

    fn go_remote(&self, s: &mut Inner, entry: Entry) {
        let Some(client) = &s.client else { return };
        log::info!("→ controllo a «{}»", client.name);
        crate::status::phase(crate::status::Phase::Controlling);
        s.return_pos = s.last_abs.or_else(|| self.platform.cursor_pos());
        s.remote = true;
        s.rem = (0.0, 0.0);
        if s.abs_source {
            // Windows: il cursore resta fermo al centro e misuriamo gli spostamenti.
            let c = self.platform.bounds().center();
            self.platform.warp(c.0, c.1);
            s.anchor = Some(c);
        }
        s.kbd_captured = s.local_keys.is_empty();
        self.platform.capture(true, s.kbd_captured);
        let msg = match entry {
            Entry::Edge(f) => Msg::Enter {
                along: frac_to_along(f),
                center: false,
            },
            Entry::Center => Msg::Enter {
                along: frac_to_along(0.5),
                center: true,
            },
        };
        send(s, msg);
    }

    fn go_local(&self, s: &mut Inner, exit: Exit) {
        if !s.remote {
            return;
        }
        log::info!("← controllo al server");
        s.remote = false;
        crate::status::phase(crate::status::Phase::Connected);
        if exit != Exit::Disconnected {
            let keys: Vec<u16> = s.remote_keys.iter().collect();
            for code in keys {
                send(s, Msg::Key { code, down: false });
            }
            for b in Button::ALL {
                if s.remote_buttons & b.bit() != 0 {
                    send(
                        s,
                        Msg::Button {
                            button: b,
                            down: false,
                        },
                    );
                }
            }
            if exit == Exit::Hotkey {
                send(s, Msg::Leave { along: 0 });
            }
        }
        s.remote_keys.clear();
        s.remote_buttons = 0;
        s.kbd_captured = false;
        self.platform.capture(false, false);
        let target = match exit {
            Exit::ClientLeft(f) => Some(return_point(self.cfg.side, self.platform.bounds(), f)),
            _ => s.return_pos,
        };
        if let Some((x, y)) = target {
            if s.abs_source || matches!(exit, Exit::ClientLeft(_)) {
                self.platform.warp(x, y);
            }
            if s.abs_source {
                s.last_abs = Some((x, y));
            }
        }
        s.anchor = None;
        s.cooldown_until = Some(Instant::now() + SWITCH_COOLDOWN);
    }

    // ----------------------------------------------------------- connessioni

    pub fn attach(&self, link: ClientLink) {
        let mut g = self.lock();
        let s = &mut *g;
        if let Some(old) = s.client.take() {
            log::info!("nuovo client: chiudo la connessione con «{}»", old.name);
            if let Some(st) = old.stream {
                let _ = st.shutdown(std::net::Shutdown::Both);
            }
            s.client = None;
            self.go_local(s, Exit::Disconnected);
        }
        log::info!(
            "client «{}» connesso ({}x{})",
            link.name,
            link.width,
            link.height
        );
        let (peer, peer_addr) = (link.name.clone(), link.addr.clone());
        crate::status::update(|st| {
            st.phase = crate::status::Phase::Connected;
            st.peer = peer;
            st.peer_addr = peer_addr;
        });
        s.client = Some(link);
    }

    pub fn detach(&self, id: u64) {
        let mut g = self.lock();
        let s = &mut *g;
        if s.client.as_ref().map(|c| c.id) == Some(id) {
            let name = s.client.take().map(|c| c.name).unwrap_or_default();
            log::info!("client «{name}» disconnesso");
            self.go_local(s, Exit::Disconnected);
            crate::status::update(|st| {
                st.phase = crate::status::Phase::Listening;
                st.peer.clear();
                st.peer_addr.clear();
                st.rtt_us = None;
            });
        }
    }

    pub fn client_left(&self, id: u64, along: u16) {
        let mut g = self.lock();
        let s = &mut *g;
        if s.client.as_ref().map(|c| c.id) == Some(id) {
            self.go_local(s, Exit::ClientLeft(along_to_frac(along)));
        }
    }

    pub fn send(&self, msg: Msg) {
        let g = self.lock();
        send(&g, msg);
    }

    pub fn send_to(&self, id: u64, msg: Msg) {
        let g = self.lock();
        if g.client.as_ref().map(|c| c.id) == Some(id) {
            send(&g, msg);
        }
    }
}

fn send(s: &Inner, msg: Msg) {
    if let Some(c) = &s.client {
        let _ = c.tx.send(msg);
    }
}

fn toward(side: Side, dx: f64, dy: f64) -> bool {
    match side {
        Side::Right => dx > 0.0,
        Side::Left => dx < 0.0,
        Side::Top => dy < 0.0,
        Side::Bottom => dy > 0.0,
    }
}

fn at_edge(side: Side, b: Rect, (x, y): (i32, i32)) -> bool {
    match side {
        Side::Right => x >= b.right(),
        Side::Left => x <= b.x,
        Side::Top => y <= b.y,
        Side::Bottom => y >= b.bottom(),
    }
}

fn along_edge(side: Side, b: Rect, (x, y): (i32, i32)) -> f64 {
    match side {
        Side::Left | Side::Right => (y - b.y) as f64 / (b.h - 1).max(1) as f64,
        Side::Top | Side::Bottom => (x - b.x) as f64 / (b.w - 1).max(1) as f64,
    }
}

/// Dove riappare il cursore del server quando torna dal client.
fn return_point(side: Side, b: Rect, f: f64) -> (i32, i32) {
    let ax = b.x + (f * (b.w - 1) as f64).round() as i32;
    let ay = b.y + (f * (b.h - 1) as f64).round() as i32;
    match side {
        Side::Right => (b.right() - 2, ay),
        Side::Left => (b.x + 2, ay),
        Side::Top => (ax, b.y + 2),
        Side::Bottom => (ax, b.bottom() - 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::Level;
    use crossbeam_channel::{unbounded, Receiver};

    #[derive(Default)]
    struct Fake {
        pos: Mutex<Option<(i32, i32)>>,
        warps: Mutex<Vec<(i32, i32)>>,
        captures: Mutex<Vec<(bool, bool)>>,
        /// Layout finto: (tasto, livello) → carattere.
        layout: Vec<(u16, Level, char)>,
    }

    impl ServerPlatform for Fake {
        fn bounds(&self) -> Rect {
            Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            }
        }
        fn cursor_pos(&self) -> Option<(i32, i32)> {
            *self.pos.lock().unwrap()
        }
        fn warp(&self, x: i32, y: i32) {
            self.warps.lock().unwrap().push((x, y));
        }
        fn capture(&self, mice: bool, keyboards: bool) {
            self.captures.lock().unwrap().push((mice, keyboards));
        }
        fn char_for(&self, code: u16, level: Level) -> Option<char> {
            self.layout
                .iter()
                .find(|(c, l, _)| *c == code && *l == level)
                .map(|(_, _, ch)| *ch)
        }
    }

    fn setup() -> (Arc<Fake>, Engine, Receiver<Msg>) {
        setup_with(Fake::default())
    }

    fn setup_with(fake: Fake) -> (Arc<Fake>, Engine, Receiver<Msg>) {
        let fake = Arc::new(fake);
        let e = Engine::new(
            fake.clone(),
            Settings {
                side: Side::Right,
                edge_switch: true,
                hotkeys: vec![vec![70], vec![97, 54]],
                speed: 1.0,
                accel: 0.0,
                lock_while_dragging: true,
            },
        );
        let (tx, rx) = unbounded();
        e.attach(ClientLink {
            id: 1,
            tx,
            name: "c".into(),
            addr: "10.0.0.2".into(),
            width: 1280,
            height: 720,
            stream: None,
        });
        (fake, e, rx)
    }

    fn drain(rx: &Receiver<Msg>) -> Vec<Msg> {
        rx.try_iter().collect()
    }

    fn key(e: &Engine, code: u16, down: bool) -> Verdict {
        e.input(Input::Key {
            code,
            down,
            repeat: false,
        })
    }

    #[test]
    fn windows_style_edge_switch_and_return() {
        let (fake, e, rx) = setup();
        assert_eq!(e.input(Input::MotionAbs { x: 1000, y: 540 }), Verdict::Pass);
        assert!(!e.is_remote());
        // Raggiunge il bordo destro a metà altezza.
        assert_eq!(
            e.input(Input::MotionAbs { x: 1919, y: 540 }),
            Verdict::Swallow
        );
        assert!(e.is_remote());
        let along = match drain(&rx).as_slice() {
            [Msg::Enter {
                along,
                center: false,
            }] => *along,
            other => panic!("{other:?}"),
        };
        assert!((along_to_frac(along) - 0.5).abs() < 0.01);
        // Il cursore viene parcheggiato al centro, i movimenti sono delta dal centro.
        assert_eq!(fake.warps.lock().unwrap().last(), Some(&(960, 540)));
        assert_eq!(
            e.input(Input::MotionAbs { x: 965, y: 538 }),
            Verdict::Swallow
        );
        assert_eq!(drain(&rx), vec![Msg::Move { dx: 5, dy: -2 }]);
        // Il client restituisce il controllo dal suo bordo sinistro.
        e.client_left(1, frac_to_along(0.25));
        assert!(!e.is_remote());
        let (wx, wy) = *fake.warps.lock().unwrap().last().unwrap();
        assert_eq!(wx, 1917);
        assert!((wy - 270).abs() <= 1);
        assert_eq!(fake.captures.lock().unwrap().last(), Some(&(false, false)));
        // Subito dopo il ritorno c'è un breve cooldown contro i rimbalzi.
        assert_eq!(e.input(Input::MotionAbs { x: 1919, y: 270 }), Verdict::Pass);
    }

    #[test]
    fn linux_style_edge_uses_cursor_query() {
        let (fake, e, rx) = setup();
        *fake.pos.lock().unwrap() = Some((1500, 10));
        assert_eq!(e.input(Input::Motion { dx: 3.0, dy: 0.0 }), Verdict::Pass);
        *fake.pos.lock().unwrap() = Some((1919, 10));
        // Allontanandosi dal bordo non si passa.
        assert_eq!(e.input(Input::Motion { dx: -3.0, dy: 0.0 }), Verdict::Pass);
        assert_eq!(
            e.input(Input::Motion { dx: 3.0, dy: 0.0 }),
            Verdict::Swallow
        );
        assert!(e.is_remote());
        assert_eq!(fake.captures.lock().unwrap().last(), Some(&(true, true)));
        drain(&rx);
        e.input(Input::Motion { dx: 2.5, dy: 0.0 });
        e.input(Input::Motion { dx: 2.5, dy: 0.0 });
        assert_eq!(
            drain(&rx),
            vec![Msg::Move { dx: 2, dy: 0 }, Msg::Move { dx: 3, dy: 0 }]
        );
    }

    #[test]
    fn no_edge_switch_while_dragging_or_without_client() {
        let (fake, e, _rx) = setup();
        *fake.pos.lock().unwrap() = Some((1919, 500));
        e.input(Input::Button {
            button: Button::Left,
            down: true,
        });
        assert_eq!(e.input(Input::Motion { dx: 5.0, dy: 0.0 }), Verdict::Pass);
        e.input(Input::Button {
            button: Button::Left,
            down: false,
        });
        e.detach(1);
        assert_eq!(e.input(Input::Motion { dx: 5.0, dy: 0.0 }), Verdict::Pass);
        assert!(!e.is_remote());
    }

    #[test]
    fn hotkey_toggles_and_is_swallowed() {
        let (fake, e, rx) = setup();
        assert_eq!(key(&e, 97, true), Verdict::Pass); // RightCtrl locale
        assert_eq!(key(&e, 54, true), Verdict::Swallow); // RightShift completa la combo
        assert!(e.is_remote());
        // Ctrl era premuto in locale: la tastiera si cattura solo dopo il rilascio.
        assert_eq!(fake.captures.lock().unwrap().last(), Some(&(true, false)));
        assert_eq!(
            drain(&rx),
            vec![Msg::Enter {
                along: frac_to_along(0.5),
                center: true
            }]
        );
        assert_eq!(key(&e, 54, false), Verdict::Swallow);
        assert_eq!(key(&e, 97, false), Verdict::Pass);
        assert_eq!(fake.captures.lock().unwrap().last(), Some(&(true, true)));
        // Ora la digitazione va al client.
        assert_eq!(key(&e, 30, true), Verdict::Swallow);
        assert_eq!(
            drain(&rx),
            vec![Msg::Key {
                code: 30,
                down: true
            }]
        );
        // ScrollLock riporta indietro e rilascia sul client i tasti ancora giù.
        assert_eq!(key(&e, 70, true), Verdict::Swallow);
        assert!(!e.is_remote());
        assert_eq!(
            drain(&rx),
            vec![
                Msg::Key {
                    code: 30,
                    down: false
                },
                Msg::Leave { along: 0 }
            ]
        );
        assert_eq!(key(&e, 70, false), Verdict::Swallow);
        assert_eq!(key(&e, 30, false), Verdict::Pass);
    }

    #[test]
    fn hotkey_needs_exact_combo() {
        let (_f, e, _rx) = setup();
        key(&e, 29, true); // LeftCtrl
        assert_eq!(key(&e, 70, true), Verdict::Pass); // Ctrl+ScrollLock non è la hotkey
        assert!(!e.is_remote());
    }

    #[test]
    fn disconnect_while_remote_releases_capture() {
        let (fake, e, _rx) = setup();
        key(&e, 70, true);
        assert!(e.is_remote());
        e.detach(1);
        assert!(!e.is_remote());
        assert_eq!(fake.captures.lock().unwrap().last(), Some(&(false, false)));
    }

    #[test]
    fn buttons_pressed_locally_release_locally() {
        let (_f, e, rx) = setup();
        e.input(Input::Button {
            button: Button::Right,
            down: true,
        });
        key(&e, 70, true);
        drain(&rx);
        assert_eq!(
            e.input(Input::Button {
                button: Button::Right,
                down: false
            }),
            Verdict::Pass
        );
        assert_eq!(
            e.input(Input::Button {
                button: Button::Left,
                down: true
            }),
            Verdict::Swallow
        );
        assert_eq!(e.input(Input::Wheel { dx: 0, dy: 120 }), Verdict::Swallow);
        assert_eq!(
            drain(&rx),
            vec![
                Msg::Button {
                    button: Button::Left,
                    down: true
                },
                Msg::Wheel { dx: 0, dy: 120 }
            ]
        );
    }

    #[test]
    fn text_follows_server_layout_and_shortcuts_stay_physical() {
        const PLAIN: Level = Level {
            shift: false,
            altgr: false,
        };
        const ALTGR: Level = Level {
            shift: false,
            altgr: true,
        };
        let (_f, e, rx) = setup_with(Fake {
            layout: vec![(39, PLAIN, 'ò'), (39, ALTGR, '@'), (46, PLAIN, 'c')],
            ..Fake::default()
        });
        key(&e, 70, true);
        key(&e, 70, false);
        drain(&rx);
        // ò semplice e AltGr+ò = @
        key(&e, 39, true);
        key(&e, 39, false);
        key(&e, 100, true);
        key(&e, 39, true);
        // la ripetizione automatica del server rimanda il carattere
        e.input(Input::Key {
            code: 39,
            down: true,
            repeat: true,
        });
        key(&e, 39, false);
        key(&e, 100, false);
        // Ctrl+Alt fa da AltGr come su Windows
        key(&e, 29, true);
        key(&e, 56, true);
        key(&e, 39, true);
        key(&e, 39, false);
        key(&e, 56, false);
        // Ctrl+C è una scorciatoia: tasto fisico
        key(&e, 46, true);
        key(&e, 46, false);
        key(&e, 29, false);
        let k = |code, down| Msg::Key { code, down };
        let t = |code, ch| Msg::Text { code, ch };
        assert_eq!(
            drain(&rx),
            vec![
                t(39, 'ò'),
                k(39, false),
                k(100, true),
                t(39, '@'),
                t(39, '@'),
                k(39, false),
                k(100, false),
                k(29, true),
                k(56, true),
                t(39, '@'),
                k(39, false),
                k(56, false),
                k(46, true),
                k(46, false),
                k(29, false),
            ]
        );
    }

    #[test]
    fn return_point_per_side() {
        let b = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 50,
        };
        assert_eq!(return_point(Side::Left, b, 0.0), (2, 0));
        assert_eq!(return_point(Side::Top, b, 1.0), (99, 2));
        assert_eq!(return_point(Side::Bottom, b, 0.5), (50, 47));
    }
}
