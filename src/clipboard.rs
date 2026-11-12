//! Sincronizzazione degli appunti (testo). Un solo thread possiede l'oggetto
//! `arboard::Clipboard` (su X11 deve restare vivo per "servire" il contenuto),
//! controlla i cambiamenti locali e applica quelli in arrivo, evitando l'eco.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use crossbeam_channel::{RecvTimeoutError, Sender};

use crate::config::ClipboardConfig;

#[derive(Clone)]
pub struct ClipHandle {
    tx: Sender<String>,
}

impl ClipHandle {
    pub fn apply_remote(&self, text: String) {
        let _ = self.tx.send(text);
    }
}

fn digest(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Avvia il thread degli appunti. `on_local_change` riceve ogni nuovo testo
/// copiato su questo PC.
pub fn start<F>(cfg: &ClipboardConfig, on_local_change: F) -> Option<ClipHandle>
where
    F: Fn(String) + Send + 'static,
{
    let (tx, rx) = crossbeam_channel::unbounded::<String>();
    let poll = Duration::from_millis(cfg.poll_ms.clamp(50, 5000));
    let max = cfg.max_bytes;
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<bool>(1);
    std::thread::Builder::new()
        .name("ponte-clipboard".into())
        .spawn(move || {
            let mut cb = match arboard::Clipboard::new() {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("appunti non disponibili: {e}");
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            let _ = ready_tx.send(true);
            let mut last = cb.get_text().ok().map(|t| digest(&t));
            loop {
                match rx.recv_timeout(poll) {
                    Ok(text) => {
                        let h = digest(&text);
                        if Some(h) == last {
                            continue;
                        }
                        match cb.set_text(text) {
                            Ok(()) => {
                                last = Some(h);
                                log::debug!("appunti aggiornati dall'altro PC");
                            }
                            Err(e) => log::warn!("impossibile scrivere gli appunti: {e}"),
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        if let Ok(text) = cb.get_text() {
                            let h = digest(&text);
                            if Some(h) != last {
                                last = Some(h);
                                if text.is_empty() {
                                    continue;
                                }
                                if text.len() > max {
                                    log::warn!(
                                        "appunti troppo grandi ({} byte), non inviati",
                                        text.len()
                                    );
                                    continue;
                                }
                                on_local_change(text);
                            }
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
        })
        .ok()?;
    if ready_rx.recv().unwrap_or(false) {
        Some(ClipHandle { tx })
    } else {
        None
    }
}
