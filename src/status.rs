//! Stato del collegamento e ultime righe di log, condivisi tra il motore
//! (server/client) e l'interfaccia grafica.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Starting,
    /// Server in ascolto, nessun client.
    Listening,
    /// Client che cerca di raggiungere il server.
    Connecting,
    Connected,
    /// Mouse e tastiera stanno lavorando sull'altro PC (server) o su questo (client).
    Controlling,
    Error,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub phase: Phase,
    /// Nome dell'altro PC.
    pub peer: String,
    /// IP dell'altro PC.
    pub peer_addr: String,
    /// Dettaglio (indirizzo, messaggio d'errore, ...).
    pub detail: String,
    pub rtt_us: Option<u64>,
    /// Porta TCP effettiva del server.
    pub listen_port: Option<u16>,
}

static STATUS: Mutex<Status> = Mutex::new(Status {
    phase: Phase::Starting,
    peer: String::new(),
    peer_addr: String::new(),
    detail: String::new(),
    rtt_us: None,
    listen_port: None,
});
static LOGS: Mutex<VecDeque<(log::Level, String)>> = Mutex::new(VecDeque::new());
static REPAINT: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();
const MAX_LOGS: usize = 300;

fn notify() {
    if let Some(f) = REPAINT.get() {
        f();
    }
}

/// L'interfaccia registra qui come farsi ridisegnare quando cambia qualcosa.
pub fn on_change(f: impl Fn() + Send + Sync + 'static) {
    let _ = REPAINT.set(Box::new(f));
}

pub fn get() -> Status {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn update(f: impl FnOnce(&mut Status)) {
    f(&mut STATUS.lock().unwrap_or_else(|e| e.into_inner()));
    notify();
}

pub fn set(phase: Phase, peer: impl Into<String>, detail: impl Into<String>) {
    let (peer, detail) = (peer.into(), detail.into());
    update(|s| {
        if phase != Phase::Connected && phase != Phase::Controlling {
            s.rtt_us = None;
        }
        s.phase = phase;
        s.peer = peer;
        s.peer_addr.clear();
        s.detail = detail;
    });
}

/// Cambia solo la fase, mantenendo nome e dettagli.
pub fn phase(phase: Phase) {
    update(|s| s.phase = phase);
}

pub fn logs() -> Vec<(log::Level, String)> {
    LOGS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect()
}

struct Tee {
    inner: env_logger::Logger,
}

impl log::Log for Tee {
    fn enabled(&self, m: &log::Metadata) -> bool {
        self.inner.enabled(m)
    }

    fn log(&self, r: &log::Record) {
        if !self.inner.enabled(r.metadata()) {
            return;
        }
        self.inner.log(r);
        if r.level() <= log::Level::Info && r.target().starts_with("ponte") {
            let mut l = LOGS.lock().unwrap_or_else(|e| e.into_inner());
            if l.len() >= MAX_LOGS {
                l.pop_front();
            }
            l.push_back((r.level(), r.args().to_string()));
            drop(l);
            notify();
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

/// Inizializza i log: terminale (come prima) + registro mostrato nella finestra.
pub fn init_logging(level: &str) {
    let filter = format!("{level},arboard=error,eframe=warn,egui=warn,egui_glow=warn,winit=warn,sctk=warn,calloop=warn");
    let inner = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(filter))
        .format_target(false)
        .build();
    let max = inner.filter();
    if log::set_boxed_logger(Box::new(Tee { inner })).is_ok() {
        log::set_max_level(max);
    }
}
