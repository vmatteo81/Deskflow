//! Astrazione sulle piattaforme: cattura dell'input (server) e iniezione
//! (client). Il resto del programma non sa se gira su Linux o Windows.

use crate::protocol::Button;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.w - 1
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h - 1
    }
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

/// Servizi che il motore del server chiede alla piattaforma.
pub trait ServerPlatform: Send + Sync {
    /// Rettangolo del desktop locale (tutti i monitor).
    fn bounds(&self) -> Rect;
    /// Posizione del cursore, se la piattaforma la conosce.
    fn cursor_pos(&self) -> Option<(i32, i32)>;
    /// Sposta il cursore locale (se supportato).
    fn warp(&self, x: i32, y: i32);
    /// Cattura in esclusiva mouse e/o tastiere (Linux: EVIOCGRAB).
    /// Su Windows gli hook decidono evento per evento, quindi è un no-op.
    fn capture(&self, mice: bool, keyboards: bool);
}

/// Iniezione dell'input sul client.
pub trait Injector: Send {
    fn screen(&self) -> Rect;
    /// Posizione reale del cursore (per riallinearsi se si usa il mouse locale).
    fn cursor_pos(&self) -> Option<(i32, i32)>;
    fn move_to(&mut self, x: i32, y: i32);
    fn button(&mut self, button: Button, down: bool);
    fn wheel(&mut self, dx: i16, dy: i16);
    fn key(&mut self, code: u16, down: bool);
}

/// Mostra un errore bloccante (finestra di dialogo su Windows, stderr altrove).
pub fn show_error(msg: &str) {
    #[cfg(windows)]
    windows::message_box("Ponte", msg);
    #[cfg(not(windows))]
    let _ = msg;
}

/// Prepara il processo (DPI awareness su Windows).
pub fn init_process() {
    #[cfg(windows)]
    windows::init_process();
}

/// Avvia la cattura dell'input locale e restituisce la piattaforma server.
pub fn start_server_capture(
    engine_slot: std::sync::Arc<std::sync::OnceLock<std::sync::Arc<crate::server::engine::Engine>>>,
) -> anyhow::Result<std::sync::Arc<dyn ServerPlatform>> {
    #[cfg(target_os = "linux")]
    {
        linux::start_capture(engine_slot)
    }
    #[cfg(windows)]
    {
        windows::start_capture(engine_slot)
    }
}

pub fn create_injector(screen_override: Option<[u32; 2]>) -> anyhow::Result<Box<dyn Injector>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::UinputInjector::new(screen_override)?))
    }
    #[cfg(windows)]
    {
        let _ = screen_override;
        Ok(Box::new(windows::SendInputInjector::new()))
    }
}
