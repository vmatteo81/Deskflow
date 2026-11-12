pub mod engine;

use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::{bail, Context, Result};

use crate::clipboard::{self, ClipHandle};
use crate::config::Config;
use crate::net;
use crate::platform;
use crate::protocol::{Msg, VERSION};
use engine::{ClientLink, Engine, Settings};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn run(cfg: Config) -> Result<()> {
    let slot: Arc<OnceLock<Arc<Engine>>> = Arc::new(OnceLock::new());
    let platform = platform::start_server_capture(slot.clone())?;
    let settings = Settings {
        side: cfg.server.client_side,
        edge_switch: cfg.server.edge_switch,
        hotkeys: cfg.hotkey_codes()?,
        speed: cfg.server.pointer_speed,
        accel: cfg.server.pointer_accel,
        lock_while_dragging: cfg.server.lock_while_dragging,
    };
    let engine = Arc::new(Engine::new(platform, settings));
    let _ = slot.set(engine.clone());

    let clip = if cfg.clipboard.enabled {
        let e = engine.clone();
        clipboard::start(&cfg.clipboard, move |text| e.send(Msg::Clipboard(text)))
    } else {
        None
    };

    let listener = TcpListener::bind(&cfg.server.listen)
        .with_context(|| format!("impossibile ascoltare su {}", cfg.server.listen))?;
    let port = listener
        .local_addr()
        .map(|a| a.port())
        .unwrap_or(crate::protocol::DEFAULT_PORT);
    let ip = crate::discovery::local_ip().unwrap_or_else(|| "?".into());
    crate::status::set(crate::status::Phase::Listening, "", format!("{ip}:{port}"));
    crate::discovery::spawn_responder(cfg.display_name(), port);
    log::info!(
        "server «{}» in ascolto su {} — client a {:?}, hotkey {:?}",
        cfg.display_name(),
        cfg.server.listen,
        cfg.server.client_side,
        cfg.server.hotkeys
    );
    let secret = Arc::new(cfg.secret.clone().into_bytes());
    let name = Arc::new(cfg.display_name());
    let side = cfg.server.client_side;
    for conn in listener.incoming() {
        let stream = match conn {
            Ok(s) => s,
            Err(e) => {
                log::warn!("accept fallita: {e}");
                continue;
            }
        };
        let (engine, clip, secret, name) =
            (engine.clone(), clip.clone(), secret.clone(), name.clone());
        std::thread::Builder::new()
            .name("ponte-conn".into())
            .spawn(move || {
                let peer = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_default();
                if let Err(e) = handle_conn(stream, &engine, clip.as_ref(), &secret, &name, side) {
                    log::warn!("connessione {peer}: {e:#}");
                }
            })?;
    }
    Ok(())
}

pub(crate) fn handle_conn(
    stream: TcpStream,
    engine: &Engine,
    clip: Option<&ClipHandle>,
    secret: &[u8],
    name: &str,
    side: crate::protocol::Side,
) -> Result<()> {
    let peer = stream.peer_addr()?;
    let close_handle = stream.try_clone()?;
    let (mut reader, mut writer) = net::handshake(stream, secret, true)?;
    let hello = reader
        .read_msg()
        .context("autenticazione fallita: il `secret` del client è diverso?")?;
    let Msg::Hello {
        version,
        name: client_name,
        width,
        height,
    } = hello
    else {
        bail!("il client non ha inviato Hello");
    };
    if version != VERSION {
        bail!("versione del protocollo diversa (client {version}, server {VERSION})");
    }
    writer.send(Msg::Welcome {
        version: VERSION,
        name: name.to_string(),
        side,
    })?;
    log::debug!("handshake ok con {peer}");

    let (tx, rx) = crossbeam_channel::unbounded();
    let epoch = Instant::now();
    std::thread::Builder::new()
        .name("ponte-writer".into())
        .spawn(move || net::writer_loop(writer, rx, epoch))?;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    engine.attach(ClientLink {
        id,
        tx,
        name: client_name,
        width,
        height,
        stream: Some(close_handle),
    });

    let result = loop {
        match reader.read_msg() {
            Ok(Msg::Leave { along }) => engine.client_left(id, along),
            Ok(Msg::Clipboard(text)) => {
                if let Some(c) = clip {
                    c.apply_remote(text);
                }
            }
            Ok(Msg::Ping(t)) => engine.send_to(id, Msg::Pong(t)),
            Ok(Msg::Pong(t)) => {
                let rtt = (epoch.elapsed().as_micros() as u64).saturating_sub(t);
                log::trace!("latenza andata/ritorno {rtt} µs");
                crate::status::update(|s| s.rtt_us = Some(rtt));
            }
            Ok(other) => log::debug!("messaggio inatteso dal client: {other:?}"),
            Err(e) => break Err(e),
        }
    };
    engine.detach(id);
    result.map_err(|e| e.context("connessione chiusa"))
}
