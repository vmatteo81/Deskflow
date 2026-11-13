//! Sincronizzazione degli appunti: testo e immagini (es. screenshot, viaggiano
//! come PNG). Un solo thread possiede l'oggetto `arboard::Clipboard` (su X11
//! deve restare vivo per "servire" il contenuto), controlla i cambiamenti
//! locali e applica quelli in arrivo, evitando l'eco.
//!
//! Leggere un'immagine grande costa: si rilegge solo quando gli appunti sono
//! davvero cambiati (Windows: numero di sequenza; Linux: notifiche XFixes,
//! altrimenti al massimo una volta al secondo).

use std::borrow::Cow;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use crossbeam_channel::{RecvTimeoutError, Sender};

use crate::config::ClipboardConfig;

/// Contenuto degli appunti scambiato tra i PC.
#[derive(Debug, Clone, PartialEq)]
pub enum Clip {
    Text(String),
    /// Immagine codificata PNG.
    Image(Vec<u8>),
}

#[derive(Clone)]
pub struct ClipHandle {
    tx: Sender<Clip>,
}

impl ClipHandle {
    pub fn apply_remote(&self, clip: Clip) {
        let _ = self.tx.send(clip);
    }
}

fn digest<T: Hash + ?Sized>(v: &T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

fn pixels_digest(img: &arboard::ImageData) -> u64 {
    let mut h = DefaultHasher::new();
    (img.width, img.height).hash(&mut h);
    img.bytes.hash(&mut h);
    h.finish()
}

/// Senza notifiche: ogni quanto rileggere un'immagine.
const IMAGE_POLL: Duration = Duration::from_secs(1);
/// Con le notifiche: controllo di sicurezza nel caso se ne perda una.
const IMAGE_POLL_SAFETY: Duration = Duration::from_secs(5);
/// Windows: il numero di sequenza è affidabile anche per il testo.
const GATE_TEXT: bool = cfg!(windows);

/// Avvia il thread degli appunti. `on_local_change` riceve ogni nuovo
/// contenuto copiato su questo PC.
pub fn start<F>(cfg: &ClipboardConfig, on_local_change: F) -> Option<ClipHandle>
where
    F: Fn(Clip) + Send + 'static,
{
    let (tx, rx) = crossbeam_channel::unbounded::<Clip>();
    let poll = Duration::from_millis(cfg.poll_ms.clamp(50, 5000));
    let max = cfg.max_bytes;
    let (ready_tx, ready_rx) = crossbeam_channel::bounded::<bool>(1);
    std::thread::Builder::new()
        .name("ponte-clipboard".into())
        .spawn(move || {
            let cb = match arboard::Clipboard::new() {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("appunti non disponibili: {e}");
                    let _ = ready_tx.send(false);
                    return;
                }
            };
            let _ = ready_tx.send(true);
            let mut w = Watcher::new(cb, max);
            loop {
                match rx.recv_timeout(poll) {
                    Ok(clip) => w.apply(clip),
                    Err(RecvTimeoutError::Timeout) => {
                        if let Some(clip) = w.poll() {
                            on_local_change(clip);
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

struct Watcher {
    cb: arboard::Clipboard,
    max: usize,
    changes: Changes,
    /// Ultimo testo visto/scritto (contro l'eco).
    last_text: Option<u64>,
    /// Pixel dell'ultima immagine vista/scritta: confrontare i pixel e non il
    /// PNG regge alle ricodifiche fatte da ogni sistema.
    last_image: Option<u64>,
    last_image_poll: Option<Instant>,
}

impl Watcher {
    fn new(mut cb: arboard::Clipboard, max: usize) -> Self {
        let mut changes = Changes::new();
        changes.changed(); // azzera: il contenuto iniziale non si invia
        let last_text = cb.get_text().ok().map(|t| digest(&t));
        let last_image = if last_text.is_none() {
            cb.get_image().ok().map(|i| pixels_digest(&i))
        } else {
            None
        };
        Self {
            cb,
            max,
            changes,
            last_text,
            last_image,
            last_image_poll: Some(Instant::now()),
        }
    }

    fn apply(&mut self, clip: Clip) {
        match clip {
            Clip::Text(text) => {
                let h = digest(&text);
                if Some(h) == self.last_text {
                    return;
                }
                match self.cb.set_text(text) {
                    Ok(()) => {
                        self.last_text = Some(h);
                        self.last_image = None;
                        log::debug!("appunti aggiornati dall'altro PC");
                    }
                    Err(e) => log::warn!("impossibile scrivere gli appunti: {e}"),
                }
            }
            Clip::Image(png) => {
                let img = match decode_png(&png) {
                    Ok(i) => i,
                    Err(e) => {
                        log::warn!("immagine ricevuta non valida: {e}");
                        return;
                    }
                };
                let h = pixels_digest(&img);
                if Some(h) == self.last_image {
                    return;
                }
                let (w, hgt) = (img.width, img.height);
                match self.cb.set_image(img) {
                    Ok(()) => {
                        self.last_image = Some(h);
                        self.last_text = None;
                        log::debug!("immagine {w}x{hgt} negli appunti dall'altro PC");
                    }
                    Err(e) => log::warn!("impossibile scrivere l'immagine negli appunti: {e}"),
                }
            }
        }
        // La nostra stessa scrittura non è un cambiamento da rimandare indietro.
        self.changes.changed();
    }

    fn poll(&mut self) -> Option<Clip> {
        let changed = self.changes.changed();
        if GATE_TEXT && changed == Some(false) {
            return None;
        }
        if let Ok(text) = self.cb.get_text() {
            if !text.is_empty() {
                let h = digest(&text);
                if Some(h) == self.last_text {
                    return None;
                }
                self.last_text = Some(h);
                self.last_image = None;
                if text.len() > self.max {
                    log::warn!("appunti troppo grandi ({} byte), non inviati", text.len());
                    return None;
                }
                return Some(Clip::Text(text));
            }
        }
        // Le immagini si rileggono solo se cambiate (o, senza notifiche, di rado).
        let every = if changed.is_some() {
            IMAGE_POLL_SAFETY
        } else {
            IMAGE_POLL
        };
        let due = self.last_image_poll.is_none_or(|t| t.elapsed() >= every);
        if changed != Some(true) && !due {
            return None;
        }
        self.last_image_poll = Some(Instant::now());
        let img = self.cb.get_image().ok()?;
        let h = pixels_digest(&img);
        if Some(h) == self.last_image {
            return None;
        }
        self.last_image = Some(h);
        self.last_text = None;
        let png = match encode_png(&img) {
            Ok(p) => p,
            Err(e) => {
                log::warn!("impossibile codificare l'immagine degli appunti: {e}");
                return None;
            }
        };
        if png.len() > self.max {
            log::warn!(
                "immagine negli appunti troppo grande ({} byte), non inviata",
                png.len()
            );
            return None;
        }
        log::debug!(
            "immagine {}x{} copiata ({} KB)",
            img.width,
            img.height,
            png.len() / 1024
        );
        Some(Clip::Image(png))
    }
}

fn encode_png(img: &arboard::ImageData) -> anyhow::Result<Vec<u8>> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Fast, FilterType::Adaptive)
        .write_image(
            &img.bytes,
            img.width as u32,
            img.height as u32,
            image::ExtendedColorType::Rgba8,
        )?;
    Ok(out)
}

fn decode_png(png: &[u8]) -> anyhow::Result<arboard::ImageData<'static>> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)?.into_rgba8();
    let (w, h) = img.dimensions();
    if (w as u64) * (h as u64) > 64 * 1024 * 1024 {
        anyhow::bail!("immagine troppo grande ({w}x{h})");
    }
    Ok(arboard::ImageData {
        width: w as usize,
        height: h as usize,
        bytes: Cow::Owned(img.into_raw()),
    })
}

// ------------------------------------------------- notifiche di cambiamento

/// Sa dire se gli appunti sono cambiati dall'ultima chiamata.
/// `None` = non lo sa (bisogna controllare).
#[cfg(windows)]
struct Changes(Option<u32>);

#[cfg(windows)]
impl Changes {
    fn new() -> Self {
        Changes(None)
    }
    fn changed(&mut self) -> Option<bool> {
        use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;
        let n = unsafe { GetClipboardSequenceNumber() };
        if n == 0 {
            return None;
        }
        let changed = self.0 != Some(n);
        self.0 = Some(n);
        Some(changed)
    }
}

/// Linux: XFixes avvisa quando cambia il proprietario di CLIPBOARD (anche
/// sotto Wayland tramite XWayland). Senza X11 si torna al polling.
#[cfg(target_os = "linux")]
struct Changes(Option<x11rb::rust_connection::RustConnection>);

#[cfg(target_os = "linux")]
impl Changes {
    fn new() -> Self {
        Changes(Self::connect())
    }

    fn connect() -> Option<x11rb::rust_connection::RustConnection> {
        use x11rb::connection::Connection;
        use x11rb::protocol::xfixes::{self, ConnectionExt as _};
        use x11rb::protocol::xproto::ConnectionExt as _;
        std::env::var_os("DISPLAY")?;
        let (conn, screen) = x11rb::connect(None).ok()?;
        let root = conn.setup().roots[screen].root;
        conn.xfixes_query_version(5, 0).ok()?.reply().ok()?;
        let atom = conn
            .intern_atom(false, b"CLIPBOARD")
            .ok()?
            .reply()
            .ok()?
            .atom;
        let mask = xfixes::SelectionEventMask::SET_SELECTION_OWNER
            | xfixes::SelectionEventMask::SELECTION_WINDOW_DESTROY
            | xfixes::SelectionEventMask::SELECTION_CLIENT_CLOSE;
        conn.xfixes_select_selection_input(root, atom, mask)
            .ok()?
            .check()
            .ok()?;
        conn.flush().ok()?;
        Some(conn)
    }

    fn changed(&mut self) -> Option<bool> {
        use x11rb::connection::Connection;
        let conn = self.0.as_ref()?;
        let mut any = false;
        loop {
            match conn.poll_for_event() {
                Ok(Some(_)) => any = true,
                Ok(None) => break,
                Err(_) => {
                    self.0 = None;
                    return None;
                }
            }
        }
        Some(any)
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
struct Changes;

#[cfg(not(any(windows, target_os = "linux")))]
impl Changes {
    fn new() -> Self {
        Changes
    }
    fn changed(&mut self) -> Option<bool> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip_keeps_pixels() {
        let img = arboard::ImageData {
            width: 3,
            height: 2,
            bytes: Cow::Owned((0..24u8).map(|i| i.wrapping_mul(37)).collect()),
        };
        let png = encode_png(&img).unwrap();
        let back = decode_png(&png).unwrap();
        assert_eq!(pixels_digest(&img), pixels_digest(&back));
        assert!(decode_png(b"non sono un png").is_err());
    }
}
