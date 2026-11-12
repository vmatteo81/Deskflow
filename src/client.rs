//! Client: riceve gli eventi e li inietta subito dal thread di lettura
//! (nessun passaggio intermedio = latenza minima). Tiene un cursore virtuale
//! per sapere quando l'utente esce dal bordo di ritorno.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use crossbeam_channel::Sender;

use crate::clipboard::{self, ClipHandle};
use crate::config::Config;
use crate::keys::KeySet;
use crate::net;
use crate::platform::{self, Injector};
use crate::protocol::{along_to_frac, frac_to_along, Button, Msg, Side, VERSION};

pub struct ClientState {
    inj: Box<dyn Injector>,
    pub side: Side,
    active: bool,
    pos: (i32, i32),
    buttons: u8,
    keys: KeySet,
}

impl ClientState {
    pub fn new(inj: Box<dyn Injector>) -> Self {
        Self {
            inj,
            side: Side::Right,
            active: false,
            pos: (0, 0),
            buttons: 0,
            keys: KeySet::default(),
        }
    }

    #[cfg(test)]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Gestisce un messaggio del server. Restituisce eventuali risposte.
    pub fn handle(&mut self, msg: Msg, out: &Sender<Msg>) {
        match msg {
            Msg::Enter { along, center } => {
                let s = self.inj.screen();
                let f = along_to_frac(along);
                let ax = s.x + (f * (s.w - 1) as f64).round() as i32;
                let ay = s.y + (f * (s.h - 1) as f64).round() as i32;
                self.pos = if center {
                    s.center()
                } else {
                    // Il client entra dal lato opposto a quello in cui si trova.
                    match self.side {
                        Side::Right => (s.x, ay),
                        Side::Left => (s.right(), ay),
                        Side::Top => (ax, s.bottom()),
                        Side::Bottom => (ax, s.y),
                    }
                };
                self.active = true;
                crate::status::phase(crate::status::Phase::Controlling);
                self.inj.move_to(self.pos.0, self.pos.1);
            }
            Msg::Move { dx, dy } if self.active => self.on_move(dx as i32, dy as i32, out),
            Msg::Button { button, down } => {
                let bit = button.bit();
                if down && self.active {
                    self.buttons |= bit;
                    self.inj.button(button, true);
                } else if !down && self.buttons & bit != 0 {
                    self.buttons &= !bit;
                    self.inj.button(button, false);
                }
            }
            Msg::Wheel { dx, dy } if self.active => self.inj.wheel(dx, dy),
            Msg::Key { code, down } => {
                if down && self.active {
                    self.keys.insert(code);
                    self.inj.key(code, true);
                } else if !down && self.keys.remove(code) {
                    self.inj.key(code, false);
                }
            }
            Msg::Leave { .. } => {
                self.release_all();
                self.active = false;
                crate::status::phase(crate::status::Phase::Connected);
            }
            Msg::Ping(t) => {
                let _ = out.send(Msg::Pong(t));
            }
            _ => {}
        }
    }

    fn on_move(&mut self, dx: i32, dy: i32, out: &Sender<Msg>) {
        let s = self.inj.screen();
        if let Some(p) = self.inj.cursor_pos() {
            // Riallinea se nel frattempo è stato usato il mouse locale.
            self.pos = p;
        }
        let (nx, ny) = (self.pos.0 + dx, self.pos.1 + dy);
        let leave = match self.side {
            Side::Right => (nx < s.x).then(|| (ny - s.y) as f64 / (s.h - 1).max(1) as f64),
            Side::Left => (nx > s.right()).then(|| (ny - s.y) as f64 / (s.h - 1).max(1) as f64),
            Side::Top => (ny > s.bottom()).then(|| (nx - s.x) as f64 / (s.w - 1).max(1) as f64),
            Side::Bottom => (ny < s.y).then(|| (nx - s.x) as f64 / (s.w - 1).max(1) as f64),
        };
        if let Some(f) = leave {
            if self.buttons == 0 {
                self.release_all();
                self.active = false;
                crate::status::phase(crate::status::Phase::Connected);
                let _ = out.send(Msg::Leave {
                    along: frac_to_along(f),
                });
                return;
            }
        }
        let p = (nx.clamp(s.x, s.right()), ny.clamp(s.y, s.bottom()));
        if p != self.pos {
            self.pos = p;
            self.inj.move_to(p.0, p.1);
        }
    }

    /// Rilascia tutto quello che è premuto (mai tasti bloccati sul client).
    pub fn release_all(&mut self) {
        for b in Button::ALL {
            if self.buttons & b.bit() != 0 {
                self.inj.button(b, false);
            }
        }
        self.buttons = 0;
        let keys: Vec<u16> = self.keys.iter().collect();
        for k in keys {
            self.inj.key(k, false);
        }
        self.keys.clear();
    }

    pub fn reset(&mut self) {
        self.release_all();
        self.active = false;
    }
}

pub fn run(cfg: Config) -> Result<()> {
    let inj = platform::create_injector(cfg.client.screen)?;
    let screen = inj.screen();
    log::info!("schermo del client: {}x{}", screen.w, screen.h);
    let mut state = ClientState::new(inj);

    let current: Arc<Mutex<Option<Sender<Msg>>>> = Arc::new(Mutex::new(None));
    let clip = if cfg.clipboard.enabled {
        let cur = current.clone();
        clipboard::start(&cfg.clipboard, move |text| {
            if let Some(tx) = cur.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                let _ = tx.send(Msg::Clipboard(text));
            }
        })
    } else {
        None
    };

    let mut addr = cfg.server_addr();
    let mut backoff = Duration::from_millis(500);
    loop {
        let started = Instant::now();
        match session(&cfg, &addr, &mut state, &current, clip.as_ref()) {
            Ok(()) => log::info!("disconnesso dal server"),
            Err(e) => {
                log::warn!("{addr}: {e:#}");
                let unreachable = format!("{e:#}").contains("non raggiungibile");
                if unreachable {
                    let host = addr
                        .rsplit_once(':')
                        .map(|(h, _)| h)
                        .unwrap_or(&addr)
                        .to_string();
                    if let Some(found) =
                        crate::discovery::rediscover(&host, cfg.client.server_name.as_deref())
                    {
                        if found != addr {
                            log::info!("server ritrovato in rete a {found}");
                            addr = found;
                            continue;
                        }
                    }
                }
                let msg = format!("{e:#}");
                crate::status::update(|s| {
                    s.phase = crate::status::Phase::Connecting;
                    s.peer.clear();
                    s.peer_addr.clear();
                    s.detail = msg;
                    s.rtt_us = None;
                });
            }
        }
        *current.lock().unwrap_or_else(|e| e.into_inner()) = None;
        state.reset();
        if started.elapsed() > Duration::from_secs(10) {
            backoff = Duration::from_millis(500);
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

pub(crate) fn session(
    cfg: &Config,
    addr: &str,
    state: &mut ClientState,
    current: &Arc<Mutex<Option<Sender<Msg>>>>,
    clip: Option<&ClipHandle>,
) -> Result<()> {
    crate::status::update(|s| {
        if s.phase != crate::status::Phase::Connecting {
            s.phase = crate::status::Phase::Connecting;
            s.detail = format!("connessione a {addr}…");
        }
    });
    let stream = net::connect(addr)?;
    let server_ip = stream
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let (mut reader, mut writer) = net::handshake(stream, cfg.secret.as_bytes(), false)?;
    let s = state.inj.screen();
    writer.send(Msg::Hello {
        version: VERSION,
        name: cfg.display_name(),
        width: s.w as u32,
        height: s.h as u32,
    })?;
    let welcome = reader
        .read_msg()
        .context("il server ha rifiutato la connessione (il `secret` è identico sui due PC?)")?;
    let Msg::Welcome {
        version,
        name,
        side,
    } = welcome
    else {
        bail!("risposta inattesa dal server");
    };
    if version != VERSION {
        bail!("versione del protocollo diversa (server {version}, client {VERSION})");
    }
    state.side = side;
    log::info!("connesso a «{name}» ({addr}), questo PC è a {side:?} del server");
    crate::status::update(|s| {
        s.phase = crate::status::Phase::Connected;
        s.peer = name;
        s.peer_addr = server_ip;
        s.detail = addr.to_string();
    });

    let (tx, rx) = crossbeam_channel::unbounded();
    let epoch = Instant::now();
    std::thread::Builder::new()
        .name("ponte-writer".into())
        .spawn(move || net::writer_loop(writer, rx, epoch))?;
    *current.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx.clone());

    loop {
        match reader.read_msg()? {
            Msg::Clipboard(text) => {
                if let Some(c) = clip {
                    c.apply_remote(text);
                }
            }
            Msg::Pong(t) => {
                let rtt = (epoch.elapsed().as_micros() as u64).saturating_sub(t);
                crate::status::update(|s| s.rtt_us = Some(rtt));
            }
            msg => state.handle(msg, &tx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Rect;
    use crossbeam_channel::unbounded;

    #[derive(Debug, PartialEq)]
    enum Ev {
        Move(i32, i32),
        Btn(Button, bool),
        Key(u16, bool),
        Wheel(i16, i16),
    }

    struct Rec(Arc<Mutex<Vec<Ev>>>);

    impl Injector for Rec {
        fn screen(&self) -> Rect {
            Rect {
                x: 0,
                y: 0,
                w: 1000,
                h: 500,
            }
        }
        fn cursor_pos(&self) -> Option<(i32, i32)> {
            None
        }
        fn move_to(&mut self, x: i32, y: i32) {
            self.0.lock().unwrap().push(Ev::Move(x, y));
        }
        fn button(&mut self, b: Button, d: bool) {
            self.0.lock().unwrap().push(Ev::Btn(b, d));
        }
        fn wheel(&mut self, dx: i16, dy: i16) {
            self.0.lock().unwrap().push(Ev::Wheel(dx, dy));
        }
        fn key(&mut self, c: u16, d: bool) {
            self.0.lock().unwrap().push(Ev::Key(c, d));
        }
    }

    fn setup(side: Side) -> (ClientState, Arc<Mutex<Vec<Ev>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut s = ClientState::new(Box::new(Rec(log.clone())));
        s.side = side;
        (s, log)
    }

    #[test]
    fn enter_move_and_leave_on_left_edge() {
        let (mut c, log) = setup(Side::Right);
        let (tx, rx) = unbounded();
        c.handle(Msg::Move { dx: 5, dy: 5 }, &tx); // ignorato: non attivo
        c.handle(
            Msg::Enter {
                along: frac_to_along(0.5),
                center: false,
            },
            &tx,
        );
        assert!(c.is_active());
        c.handle(Msg::Move { dx: 10, dy: -3 }, &tx);
        c.handle(
            Msg::Key {
                code: 30,
                down: true,
            },
            &tx,
        );
        c.handle(Msg::Move { dx: -2000, dy: 0 }, &tx); // oltre il bordo sinistro
        assert!(!c.is_active());
        assert_eq!(
            *log.lock().unwrap(),
            vec![
                Ev::Move(0, 250),
                Ev::Move(10, 247),
                Ev::Key(30, true),
                Ev::Key(30, false)
            ]
        );
        match rx.try_recv().unwrap() {
            Msg::Leave { along } => assert!((along_to_frac(along) - 247.0 / 499.0).abs() < 0.01),
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn clamps_inside_screen_and_no_leave_while_dragging() {
        let (mut c, log) = setup(Side::Right);
        let (tx, rx) = unbounded();
        c.handle(
            Msg::Enter {
                along: 0,
                center: true,
            },
            &tx,
        );
        c.handle(
            Msg::Button {
                button: Button::Left,
                down: true,
            },
            &tx,
        );
        c.handle(
            Msg::Move {
                dx: -5000,
                dy: 9000,
            },
            &tx,
        );
        assert!(c.is_active(), "durante il trascinamento resta sul client");
        assert!(rx.try_recv().is_err());
        assert_eq!(log.lock().unwrap().last(), Some(&Ev::Move(0, 499)));
        c.handle(Msg::Wheel { dx: 0, dy: 120 }, &tx);
        c.handle(Msg::Leave { along: 0 }, &tx); // hotkey sul server
        assert!(!c.is_active());
        assert_eq!(
            log.lock().unwrap().last(),
            Some(&Ev::Btn(Button::Left, false))
        );
    }

    #[test]
    fn enter_positions_per_side() {
        for (side, expected) in [
            (Side::Left, Ev::Move(999, 0)),
            (Side::Top, Ev::Move(0, 499)),
            (Side::Bottom, Ev::Move(0, 0)),
        ] {
            let (mut c, log) = setup(side);
            let (tx, _rx) = unbounded();
            c.handle(
                Msg::Enter {
                    along: 0,
                    center: false,
                },
                &tx,
            );
            assert_eq!(log.lock().unwrap()[0], expected);
        }
    }

    #[test]
    fn answers_ping_and_ignores_stray_releases() {
        let (mut c, log) = setup(Side::Right);
        let (tx, rx) = unbounded();
        c.handle(Msg::Ping(9), &tx);
        assert_eq!(rx.try_recv().unwrap(), Msg::Pong(9));
        c.handle(
            Msg::Key {
                code: 30,
                down: false,
            },
            &tx,
        );
        c.handle(
            Msg::Button {
                button: Button::Right,
                down: false,
            },
            &tx,
        );
        assert!(log.lock().unwrap().is_empty());
    }
}
