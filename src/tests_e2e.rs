//! Test end-to-end senza hardware: motore del server → TCP cifrato reale →
//! client che inietta in un registratore. Misura anche la latenza.

use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::client::{self, ClientState};
use crate::config::{Config, Mode};
use crate::platform::{Injector, Rect, ServerPlatform};
use crate::protocol::{Button, Side};
use crate::server::{
    self,
    engine::{Engine, Input, Settings},
};

struct FakeDesk;

impl ServerPlatform for FakeDesk {
    fn bounds(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        }
    }
    fn cursor_pos(&self) -> Option<(i32, i32)> {
        None
    }
    fn warp(&self, _: i32, _: i32) {}
    fn capture(&self, _: bool, _: bool) {}
}

#[derive(Default)]
struct Log {
    moves: Vec<((i32, i32), Instant)>,
    keys: Vec<(u16, bool)>,
    buttons: Vec<(Button, bool)>,
}

struct Rec(Arc<Mutex<Log>>);

impl Injector for Rec {
    fn screen(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 2560,
            h: 1440,
        }
    }
    fn cursor_pos(&self) -> Option<(i32, i32)> {
        None
    }
    fn move_to(&mut self, x: i32, y: i32) {
        self.0.lock().unwrap().moves.push(((x, y), Instant::now()));
    }
    fn button(&mut self, b: Button, d: bool) {
        self.0.lock().unwrap().buttons.push((b, d));
    }
    fn wheel(&mut self, _: i16, _: i16) {}
    fn key(&mut self, c: u16, d: bool) {
        self.0.lock().unwrap().keys.push((c, d));
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(5), "timeout: {what}");
        std::thread::sleep(Duration::from_micros(200));
    }
}

#[test]
fn server_to_client_over_real_tcp() {
    let secret = "e2e-segreto-di-prova-abcdef";
    let engine = Arc::new(Engine::new(
        Arc::new(FakeDesk),
        Settings {
            side: Side::Right,
            edge_switch: true,
            hotkeys: vec![vec![70]],
            speed: 1.0,
            accel: 0.0,
            lock_while_dragging: true,
        },
    ));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let e = engine.clone();
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let _ = server::handle_conn(stream, &e, None, secret.as_bytes(), "srv", Side::Right);
    });

    let log = Arc::new(Mutex::new(Log::default()));
    let l2 = log.clone();
    let cfg_text = crate::config::template(Mode::Client, secret, &addr);
    std::thread::spawn(move || {
        let cfg: Config = toml::from_str(&cfg_text).unwrap();
        let mut state = ClientState::new(Box::new(Rec(l2)));
        let current = Arc::new(Mutex::new(None));
        let _ = client::session(&cfg, &addr, &mut state, &current, None);
    });

    // Attende che il client sia collegato, poi passa al client con la hotkey.
    wait_until("connessione", || {
        engine.input(Input::Key {
            code: 70,
            down: true,
            repeat: false,
        });
        engine.input(Input::Key {
            code: 70,
            down: false,
            repeat: false,
        });
        engine.is_remote()
    });
    wait_until("ingresso", || !log.lock().unwrap().moves.is_empty());
    assert_eq!(
        log.lock().unwrap().moves[0].0,
        (1280, 720),
        "entra al centro"
    );
    engine.input(Input::Key {
        code: 30,
        down: true,
        repeat: false,
    });
    engine.input(Input::Button {
        button: Button::Left,
        down: true,
    });
    engine.input(Input::Button {
        button: Button::Left,
        down: false,
    });
    engine.input(Input::Key {
        code: 30,
        down: false,
        repeat: false,
    });
    wait_until("tasti e clic", || {
        let l = log.lock().unwrap();
        l.keys.len() == 2 && l.buttons.len() == 2
    });
    assert_eq!(log.lock().unwrap().keys, vec![(30, true), (30, false)]);
    assert_eq!(
        log.lock().unwrap().buttons,
        vec![(Button::Left, true), (Button::Left, false)]
    );

    // Latenza: un movimento alla volta, aspettando che arrivi.
    let mut lat = Vec::new();
    for i in 0..500 {
        let before = log.lock().unwrap().moves.len();
        let dx = if i % 2 == 0 { 1.0 } else { -1.0 };
        let t0 = Instant::now();
        engine.input(Input::Motion { dx, dy: 0.0 });
        wait_until("movimento", || log.lock().unwrap().moves.len() > before);
        lat.push(log.lock().unwrap().moves.last().unwrap().1 - t0);
    }
    lat.sort();
    let p50 = lat[lat.len() / 2];
    let p99 = lat[lat.len() * 99 / 100];
    println!("latenza server→client (loopback, cifrata): p50 {p50:?}, p99 {p99:?}");
    assert!(
        p50 < Duration::from_millis(5),
        "latenza troppo alta: {p50:?}"
    );
}
