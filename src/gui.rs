//! Interfaccia grafica: configurazione guidata al primo avvio e schermata
//! di stato. Il motore (server/client) gira su thread separati nello stesso
//! processo; la finestra legge lo stato da `status`.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::anyhow;
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Frame, Margin, Rect, RichText, Sense,
    Stroke, StrokeKind, TextEdit, TextStyle,
};

use crate::config::{self, Config, Mode};
use crate::discovery::{self, Found};
use crate::protocol::Side;
use crate::status::{self, Phase};

const LOGO_PNG: &[u8] = include_bytes!("../assets/ponte-256.png");

mod pal {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(0x0A, 0x0D, 0x24);
    pub const PANEL: Color32 = Color32::from_rgb(0x10, 0x14, 0x36);
    pub const CARD: Color32 = Color32::from_rgb(0x16, 0x1B, 0x45);
    pub const CARD_HOVER: Color32 = Color32::from_rgb(0x1D, 0x23, 0x56);
    pub const BORDER: Color32 = Color32::from_rgb(0x2A, 0x30, 0x63);
    pub const TEXT: Color32 = Color32::from_rgb(0xE8, 0xEA, 0xFF);
    pub const MUTED: Color32 = Color32::from_rgb(0x9C, 0xA2, 0xCC);
    pub const VIOLET: Color32 = Color32::from_rgb(0x8B, 0x5C, 0xF6);
    pub const INDIGO: Color32 = Color32::from_rgb(0x63, 0x66, 0xF1);
    pub const CYAN: Color32 = Color32::from_rgb(0x22, 0xD3, 0xEE);
    pub const GREEN: Color32 = Color32::from_rgb(0x34, 0xD3, 0x99);
    pub const AMBER: Color32 = Color32::from_rgb(0xFB, 0xBF, 0x24);
    pub const RED: Color32 = Color32::from_rgb(0xF8, 0x71, 0x71);
}

/// C'è un display su cui aprire la finestra?
pub fn available() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

fn is_wayland() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some()
}

pub fn run(path: PathBuf, minimized: bool) -> anyhow::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Ponte")
        .with_app_id("ponte")
        .with_inner_size([580.0, 720.0])
        .with_min_inner_size([480.0, 560.0]);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(LOGO_PNG) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Ponte",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, path, minimized)))),
    )
    .map_err(|e| anyhow!("interfaccia grafica non disponibile: {e}"))
}

#[derive(PartialEq)]
enum View {
    Setup,
    Status,
}

struct Form {
    mode: Mode,
    side: Side,
    server: String,
    secret: String,
    name: String,
    edge_switch: bool,
    clipboard: bool,
    /// Codice di abbinamento a 6 cifre (solo client).
    code: String,
}

type PairResult = anyhow::Result<(String, String)>;

struct App {
    path: PathBuf,
    view: View,
    logo: egui::TextureHandle,
    form: Form,
    cfg: Option<Config>,
    started: bool,
    show_secret: bool,
    scanning: Option<mpsc::Receiver<Vec<Found>>>,
    pairing: Option<mpsc::Receiver<PairResult>>,
    ips: (Vec<String>, f64),
    found: Option<Vec<Found>>,
    error: Option<String>,
    copied_at: Option<f64>,
    frames: u32,
    /// Nome del server ricevuto con l'abbinamento.
    paired_server: Option<String>,
    fw_checked: f64,
    /// Avvio automatico: riduce la finestra appena è comparsa.
    minimize_pending: bool,
    /// Un'altra istanza ha chiesto di mostrare la finestra.
    show_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = pal::BG;
    v.window_fill = pal::PANEL;
    v.extreme_bg_color = Color32::from_rgb(0x08, 0x0A, 0x1C);
    v.faint_bg_color = pal::CARD;
    v.override_text_color = Some(pal::TEXT);
    v.hyperlink_color = pal::CYAN;
    v.selection.bg_fill = pal::INDIGO;
    v.selection.stroke = Stroke::new(1.0, pal::TEXT);
    let r = CornerRadius::same(8);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = r;
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, pal::BORDER);
    v.widgets.inactive.weak_bg_fill = pal::CARD_HOVER;
    v.widgets.inactive.bg_fill = pal::CARD_HOVER;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x28, 0x2F, 0x6E);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, pal::INDIGO);
    v.widgets.active.weak_bg_fill = pal::INDIGO;
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(10.0, 10.0);
        s.spacing.button_padding = vec2(14.0, 7.0);
        s.spacing.interact_size.y = 30.0;
        s.text_styles
            .insert(TextStyle::Body, FontId::proportional(15.0));
        s.text_styles
            .insert(TextStyle::Button, FontId::proportional(15.0));
        s.text_styles
            .insert(TextStyle::Heading, FontId::proportional(22.0));
        s.text_styles
            .insert(TextStyle::Small, FontId::proportional(12.5));
        s.text_styles
            .insert(TextStyle::Monospace, FontId::monospace(14.0));
    });
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, path: PathBuf, minimized: bool) -> Self {
        setup_style(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        status::on_change(move || ctx.request_repaint());
        let show_requested = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let (ctx, flag) = (cc.egui_ctx.clone(), show_requested.clone());
            crate::instance::on_show(move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                ctx.request_repaint();
            });
        }

        let logo = {
            let icon = eframe::icon_data::from_png_bytes(LOGO_PNG).expect("logo valido");
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            cc.egui_ctx
                .load_texture("logo", img, egui::TextureOptions::LINEAR)
        };

        let mut app = App {
            path: path.clone(),
            view: View::Setup,
            logo,
            form: Form {
                mode: Mode::Server,
                side: Side::Right,
                server: String::new(),
                secret: crate::crypto::generate_secret(),
                name: config::hostname(),
                edge_switch: true,
                clipboard: true,
                code: String::new(),
            },
            cfg: None,
            started: false,
            show_secret: false,
            scanning: None,
            pairing: None,
            ips: (Vec::new(), -100.0),
            found: None,
            error: None,
            copied_at: None,
            frames: 0,
            paired_server: None,
            fw_checked: -100.0,
            minimize_pending: minimized,
            show_requested,
        };
        if std::env::var("PONTE_SCREENSHOT_MODE").as_deref() == Ok("client") {
            app.form.mode = Mode::Client;
            app.form.secret.clear();
        }

        if path.exists() {
            match Config::load(&path) {
                Ok(cfg) => {
                    app.fill_form(&cfg);
                    sync_autostart(cfg.autostart);
                    app.start(cfg);
                }
                Err(e) => app.error = Some(format!("{e:#}")),
            }
        }
        let auto = app.cfg.as_ref().is_none_or(|c| c.update.auto_check);
        if auto && std::env::var_os("PONTE_SCREENSHOT").is_none() {
            crate::update::spawn_periodic();
        }
        app
    }

    fn fill_form(&mut self, cfg: &Config) {
        self.form = Form {
            mode: cfg.mode,
            side: cfg.server.client_side,
            server: cfg.client.server.clone(),
            secret: cfg.secret.clone(),
            name: cfg.display_name(),
            edge_switch: cfg.server.edge_switch,
            clipboard: cfg.clipboard.enabled,
            code: String::new(),
        };
    }

    fn start(&mut self, cfg: Config) {
        if self.started {
            // Hook e dispositivi sono legati al processo: si riparte da capo.
            restart();
            return;
        }
        self.started = true;
        status::set(Phase::Starting, "", "");
        let run_cfg = cfg.clone();
        let _ = std::thread::Builder::new()
            .name("ponte-engine".into())
            .spawn(move || {
                let res = match run_cfg.mode {
                    Mode::Server => crate::server::run(run_cfg),
                    Mode::Client => crate::client::run(run_cfg),
                };
                if let Err(e) = res {
                    log::error!("{e:#}");
                    status::set(Phase::Error, "", format!("{e:#}"));
                }
            });
        self.cfg = Some(cfg);
        self.view = View::Status;
    }

    fn set_autostart(&mut self, on: bool) {
        let Some(cfg) = self.cfg.as_mut() else { return };
        cfg.autostart = on;
        if let Err(e) = config::save(&self.path, cfg) {
            self.error = Some(format!("{e:#}"));
        }
        sync_autostart(on);
    }

    fn build_config(&self) -> anyhow::Result<Config> {
        let mut cfg = match &self.cfg {
            Some(c) => c.clone(),
            None => toml::from_str(&config::template(
                self.form.mode,
                &self.form.secret,
                &self.form.server,
            ))?,
        };
        let f = &self.form;
        cfg.mode = f.mode;
        cfg.secret = f.secret.trim().to_string();
        let name = f.name.trim();
        cfg.name = (!name.is_empty() && name != config::hostname()).then(|| name.to_string());
        cfg.server.client_side = f.side;
        cfg.server.edge_switch = f.edge_switch;
        cfg.client.server = f.server.trim().to_string();
        cfg.clipboard.enabled = f.clipboard;
        if let Some(n) = &self.paired_server {
            cfg.client.server_name = Some(n.clone());
        }
        cfg.validate()?;
        Ok(cfg)
    }

    fn save_and_start(&mut self) {
        match self
            .build_config()
            .and_then(|c| config::save(&self.path, &c).map(|_| c))
        {
            Ok(cfg) => {
                self.error = None;
                sync_autostart(cfg.autostart);
                self.start(cfg);
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    /// IP di questo PC (aggiornati ogni pochi secondi).
    fn frames_minimize(&mut self, ctx: &egui::Context) {
        // Si riduce dopo i primi frame: prima la finestra deve esistere davvero.
        if ctx.cumulative_pass_nr() >= 2 {
            self.minimize_pending = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        } else {
            ctx.request_repaint();
        }
    }

    fn local_ips(&mut self, ui: &egui::Ui) -> Vec<String> {
        let now = ui.input(|i| i.time);
        if now - self.ips.1 > 5.0 {
            self.ips = (discovery::local_ips(), now);
        }
        self.ips.0.clone()
    }

    fn start_pairing(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (server, code, name) = (
            self.form.server.trim().to_string(),
            self.form.code.clone(),
            self.form.name.trim().to_string(),
        );
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let addr = if server.contains(':') {
                server
            } else {
                format!("{server}:{}", crate::protocol::DEFAULT_PORT)
            };
            let _ = tx.send(crate::pairing::pair(&addr, &code, &name));
            ctx.request_repaint();
        });
        self.error = None;
        self.pairing = Some(rx);
    }

    fn copy(&mut self, ui: &egui::Ui, text: &str) {
        ui.ctx().copy_text(text.to_string());
        self.copied_at = Some(ui.input(|i| i.time));
    }

    fn just_copied(&self, ui: &egui::Ui) -> bool {
        self.copied_at
            .is_some_and(|t| ui.input(|i| i.time) - t < 1.5)
    }
}

impl App {
    /// Solo per sviluppo: con PONTE_SCREENSHOT=file salva la finestra
    /// (RGBA grezzo con intestazione larghezza/altezza) ed esce.
    fn debug_screenshot(&mut self, ctx: &egui::Context) {
        let Ok(path) = std::env::var("PONTE_SCREENSHOT") else {
            return;
        };
        self.frames += 1;
        // Stati finti per controllare la grafica senza un secondo PC.
        if std::env::var("PONTE_SCREENSHOT_PHASE").as_deref() == Ok("listening") {
            status::set(Phase::Listening, "", "");
        }
        if std::env::var_os("PONTE_SCREENSHOT_FOUND").is_some() && self.frames == 1 {
            self.found = Some(vec![
                Found {
                    name: "fisso-ufficio".into(),
                    addr: "192.168.1.10:24800".into(),
                },
                Found {
                    name: "workstation".into(),
                    addr: "192.168.1.23:24800".into(),
                },
            ]);
            self.form.server = "192.168.1.10:24800".into();
            self.form.code = "482913".into();
        }
        ctx.request_repaint();
        if self.frames == 20 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(
                egui::ScreenshotCallback::new(move |img| {
                    let mut out = Vec::with_capacity(8 + img.pixels.len() * 4);
                    out.extend_from_slice(&(img.size[0] as u32).to_le_bytes());
                    out.extend_from_slice(&(img.size[1] as u32).to_le_bytes());
                    for px in &img.pixels {
                        out.extend_from_slice(&px.to_array());
                    }
                    let _ = std::fs::write(&path, out);
                    std::process::exit(0);
                }),
            ));
        }
    }
}

fn restart() {
    match std::env::current_exe() {
        Ok(exe) => crate::instance::restart_with(exe),
        Err(_) => std::process::exit(0),
    }
}

/// Allinea l'avvio automatico del sistema alla configurazione.
fn sync_autostart(enabled: bool) {
    if std::env::var_os("PONTE_SCREENSHOT").is_some() {
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = crate::autostart::set(enabled) {
            log::warn!("avvio automatico: {e:#}");
        }
    });
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.scanning {
            if let Ok(found) = rx.try_recv() {
                if found.len() == 1 && self.form.server.trim().is_empty() {
                    self.form.server = found[0].addr.clone();
                }
                self.found = Some(found);
                self.scanning = None;
            }
        }
        if let Some(rx) = &self.pairing {
            if let Ok(res) = rx.try_recv() {
                self.pairing = None;
                match res {
                    Ok((secret, server_name)) => {
                        log::info!("abbinato con «{server_name}»");
                        self.form.secret = secret;
                        self.form.code.clear();
                        self.paired_server = Some(server_name).filter(|n| !n.is_empty());
                        self.save_and_start();
                    }
                    Err(e) => self.error = Some(format!("{e:#}")),
                }
            }
        }
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        self.debug_screenshot(ui.ctx());
        if self.minimize_pending {
            self.frames_minimize(ui.ctx());
        }
        if self
            .show_requested
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
        }

        egui::Panel::top("header")
            .frame(
                Frame::new()
                    .fill(pal::PANEL)
                    .inner_margin(Margin::symmetric(20, 14)),
            )
            .show(ui, |ui| self.header(ui));

        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .fill(pal::BG)
                    .inner_margin(Margin::symmetric(20, 16)),
            )
            .show(ui, |ui| {
                let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
                if let Some(off) = std::env::var("PONTE_SCREENSHOT_SCROLL")
                    .ok()
                    .and_then(|v| v.parse().ok())
                {
                    area = area.vertical_scroll_offset(off);
                }
                area.show(ui, |ui| {
                    update_banner(ui);
                    match self.view {
                        View::Setup => self.setup_view(ui),
                        View::Status => self.status_view(ui),
                    }
                });
            });
    }
}

// ------------------------------------------------------------- componenti

fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    Frame::new()
        .fill(pal::CARD)
        .stroke(Stroke::new(1.0, pal::BORDER))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).size(16.0).strong());
}

fn hint(ui: &mut egui::Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).color(pal::MUTED).size(13.0));
}

fn primary_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), sense);
    let p = ui.painter();
    let fill = match (enabled, resp.hovered()) {
        (false, _) => pal::CARD_HOVER,
        (true, true) => Color32::from_rgb(0x75, 0x78, 0xF6),
        (true, false) => pal::INDIGO,
    };
    p.rect_filled(rect, CornerRadius::same(10), fill);
    let color = if enabled { Color32::WHITE } else { pal::MUTED };
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        FontId::proportional(16.0),
        color,
    );
    if enabled {
        resp.clone().on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    enabled && resp.clicked()
}

/// Pillola colorata con pallino e testo.
fn badge(ui: &mut egui::Ui, color: Color32, text: &str) {
    let font = FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
    let size = vec2(galley.size().x + 38.0, 28.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(14), color.gamma_multiply(0.15));
    p.circle_filled(pos2(rect.left() + 15.0, rect.center().y), 4.5, color);
    p.galley(
        pos2(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Right => "destro",
        Side::Left => "sinistro",
        Side::Top => "superiore",
        Side::Bottom => "inferiore",
    }
}

/// Piccolo monitor stilizzato.
fn paint_monitor(p: &egui::Painter, center: egui::Pos2, w: f32, color: Color32, fill: Color32) {
    let h = w * 0.66;
    let r = Rect::from_center_size(center - vec2(0.0, h * 0.12), vec2(w, h));
    p.rect_filled(r, CornerRadius::same(5), fill);
    p.rect_stroke(
        r,
        CornerRadius::same(5),
        Stroke::new(2.5, color),
        StrokeKind::Inside,
    );
    let base = Rect::from_center_size(pos2(center.x, r.bottom() + h * 0.18), vec2(w * 0.36, 3.5));
    p.rect_filled(base, CornerRadius::same(2), color);
}

fn paint_cursor(p: &egui::Painter, tip: egui::Pos2, s: f32, color: Color32) {
    let pts = [
        (0.0, 0.0),
        (0.0, 1.0),
        (0.26, 0.76),
        (0.44, 1.12),
        (0.58, 1.05),
        (0.4, 0.7),
        (0.72, 0.7),
    ];
    let pts: Vec<_> = pts.iter().map(|(x, y)| tip + vec2(x * s, y * s)).collect();
    p.add(egui::Shape::convex_polygon(
        pts,
        color,
        Stroke::new(1.5, pal::BG),
    ));
}

/// Scheda selezionabile per il ruolo. Restituisce true se cliccata.
fn role_card(ui: &mut egui::Ui, selected: bool, title: &str, body: &str, server: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 116.0), Sense::click());
    let p = ui.painter_at(rect);
    let accent = if server { pal::VIOLET } else { pal::CYAN };
    let fill = if selected {
        accent.gamma_multiply(0.16)
    } else if resp.hovered() {
        pal::CARD_HOVER
    } else {
        pal::CARD
    };
    p.rect_filled(rect, CornerRadius::same(14), fill);
    let stroke = if selected {
        Stroke::new(2.0, accent)
    } else {
        Stroke::new(1.0, pal::BORDER)
    };
    p.rect_stroke(rect, CornerRadius::same(14), stroke, StrokeKind::Inside);

    let icon_c = pos2(rect.left() + 36.0, rect.top() + 34.0);
    paint_monitor(&p, icon_c, 38.0, accent, pal::BG);
    if server {
        // tastiera sotto il monitor
        let kb = Rect::from_center_size(icon_c + vec2(0.0, 22.0), vec2(30.0, 5.0));
        p.rect_filled(kb, CornerRadius::same(2), accent.gamma_multiply(0.7));
    } else {
        paint_cursor(&p, icon_c + vec2(2.0, -8.0), 15.0, Color32::WHITE);
    }
    if selected {
        let c = pos2(rect.right() - 22.0, rect.top() + 22.0);
        p.circle_filled(c, 10.0, accent);
        let tick = Stroke::new(2.2, pal::BG);
        p.line_segment([c + vec2(-4.5, 0.0), c + vec2(-1.2, 3.4)], tick);
        p.line_segment([c + vec2(-1.2, 3.4), c + vec2(4.8, -3.6)], tick);
    }
    p.text(
        pos2(rect.left() + 66.0, rect.top() + 26.0),
        Align2::LEFT_CENTER,
        title,
        FontId::proportional(19.0),
        pal::TEXT,
    );
    let galley = p.layout(
        body.to_string(),
        FontId::proportional(13.5),
        pal::MUTED,
        rect.width() - 32.0,
    );
    p.galley(
        pos2(rect.left() + 18.0, rect.top() + 64.0),
        galley,
        pal::MUTED,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Schema per scegliere dove sta il client rispetto al server.
fn side_picker(ui: &mut egui::Ui, side: &mut Side) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 230.0), Sense::hover());
    let p = ui.painter_at(rect);
    let c = rect.center();
    let me = Rect::from_center_size(c, vec2(130.0, 82.0));
    let slots = [
        (
            Side::Left,
            Rect::from_center_size(c - vec2(165.0, 0.0), vec2(108.0, 70.0)),
        ),
        (
            Side::Right,
            Rect::from_center_size(c + vec2(165.0, 0.0), vec2(108.0, 70.0)),
        ),
        (
            Side::Top,
            Rect::from_center_size(c - vec2(0.0, 82.0), vec2(108.0, 44.0)),
        ),
        (
            Side::Bottom,
            Rect::from_center_size(c + vec2(0.0, 82.0), vec2(108.0, 44.0)),
        ),
    ];
    for (s, r) in slots {
        let resp = ui.interact(r, ui.scope_id().with(("side", s as u8)), Sense::click());
        if resp.clicked() {
            *side = s;
        }
        let selected = *side == s;
        if selected {
            // collegamento tra i due PC
            let (a, b) = match s {
                Side::Left => (pos2(me.left(), c.y), pos2(r.right(), c.y)),
                Side::Right => (pos2(me.right(), c.y), pos2(r.left(), c.y)),
                Side::Top => (pos2(c.x, me.top()), pos2(c.x, r.bottom())),
                Side::Bottom => (pos2(c.x, me.bottom()), pos2(c.x, r.top())),
            };
            p.line_segment([a, b], Stroke::new(4.0, pal::INDIGO));
            p.rect_filled(r, CornerRadius::same(10), pal::CYAN.gamma_multiply(0.22));
            p.rect_stroke(
                r,
                CornerRadius::same(10),
                Stroke::new(2.0, pal::CYAN),
                StrokeKind::Inside,
            );
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                "Altro PC",
                FontId::proportional(14.0),
                pal::TEXT,
            );
        } else {
            let col = if resp.hovered() {
                pal::MUTED
            } else {
                pal::BORDER
            };
            p.rect_stroke(
                r,
                CornerRadius::same(10),
                Stroke::new(1.5, col),
                StrokeKind::Inside,
            );
            p.text(
                r.center(),
                Align2::CENTER_CENTER,
                "qui?",
                FontId::proportional(13.0),
                col,
            );
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    p.rect_filled(me, CornerRadius::same(12), pal::VIOLET.gamma_multiply(0.25));
    p.rect_stroke(
        me,
        CornerRadius::same(12),
        Stroke::new(2.0, pal::VIOLET),
        StrokeKind::Inside,
    );
    p.text(
        me.center(),
        Align2::CENTER_CENTER,
        "Questo PC",
        FontId::proportional(15.0),
        pal::TEXT,
    );
}

// ---------------------------------------------------------------- viste

impl App {
    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.image(egui::load::SizedTexture::new(
                self.logo.id(),
                vec2(46.0, 46.0),
            ));
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.label(RichText::new("Ponte").size(23.0).strong());
                ui.label(
                    RichText::new("Mouse, tastiera e appunti condivisi")
                        .color(pal::MUTED)
                        .size(13.0),
                );
            });
            if self.view == View::Status {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (color, text) = phase_badge(status::get().phase);
                    badge(ui, color, text);
                });
            }
        });
    }

    fn setup_view(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new(if self.started {
                "Impostazioni"
            } else {
                "Benvenuto! Configuriamo Ponte"
            })
            .size(20.0)
            .strong(),
        );
        hint(ui, "Serve un'installazione di Ponte su ognuno dei due PC.");
        ui.add_space(4.0);

        section(ui, "Come usi questo PC?");
        let mut pick = None;
        ui.columns(2, |cols| {
            if role_card(
                &mut cols[0],
                self.form.mode == Mode::Server,
                "Server",
                "Ha mouse e tastiera collegati e li condivide con l'altro PC.",
                true,
            ) {
                pick = Some(Mode::Server);
            }
            if role_card(
                &mut cols[1],
                self.form.mode == Mode::Client,
                "Client",
                "Viene comandato dal mouse e dalla tastiera del server.",
                false,
            ) {
                pick = Some(Mode::Client);
            }
        });
        if let Some(m) = pick {
            if m != self.form.mode && self.cfg.is_none() {
                // Il server genera il codice, il client lo riceve: niente precompilato.
                self.form.secret = match m {
                    Mode::Server => crate::crypto::generate_secret(),
                    Mode::Client => String::new(),
                };
            }
            self.form.mode = m;
        }

        match self.form.mode {
            Mode::Server => self.server_form(ui),
            Mode::Client => self.client_form(ui),
        }

        section(ui, "Opzioni");
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Nome di questo PC");
                ui.add(TextEdit::singleline(&mut self.form.name).desired_width(f32::INFINITY));
            });
            let ips = self.local_ips(ui);
            hint(
                ui,
                format!(
                    "Indirizzo IP di questo PC: {}",
                    if ips.is_empty() {
                        "non rilevato".into()
                    } else {
                        ips.join(", ")
                    }
                ),
            );
            ui.checkbox(
                &mut self.form.clipboard,
                "Condividi gli appunti (copia e incolla tra i PC)",
            );
        });

        if let Some(err) = &self.error {
            ui.add_space(4.0);
            Frame::new()
                .fill(pal::RED.gamma_multiply(0.12))
                .stroke(Stroke::new(1.0, pal::RED.gamma_multiply(0.6)))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(err).color(pal::RED));
                });
        }

        ui.add_space(8.0);
        let client = self.form.mode == Mode::Client;
        let code_ok = crate::pairing::normalize(&self.form.code).is_some();
        let valid = self.pairing.is_none()
            && if client {
                !self.form.server.trim().is_empty()
                    && (code_ok || self.form.secret.trim().len() >= 16)
            } else {
                self.form.secret.trim().len() >= 16
            };
        let label = if self.pairing.is_some() {
            "Abbinamento in corso…"
        } else if client && code_ok {
            "Abbina e avvia"
        } else if self.started {
            "Salva e riavvia"
        } else {
            "Salva e avvia"
        };
        if primary_button(ui, label, valid) {
            if client && code_ok {
                self.start_pairing(ui.ctx());
            } else {
                self.save_and_start();
            }
        }
        if self.started
            && ui
                .add_sized([ui.available_width(), 36.0], egui::Button::new("Annulla"))
                .clicked()
        {
            if let Some(c) = self.cfg.clone() {
                self.fill_form(&c);
            }
            self.error = None;
            self.view = View::Status;
        }
        ui.add_space(8.0);
    }

    fn server_form(&mut self, ui: &mut egui::Ui) {
        section(ui, "Dove si trova l'altro PC?");
        card(ui, |ui| {
            hint(
                ui,
                "Clicca sulla posizione dello schermo del client rispetto a questo.",
            );
            side_picker(ui, &mut self.form.side);
            ui.checkbox(
                &mut self.form.edge_switch,
                format!(
                    "Passa all'altro PC spingendo il mouse oltre il bordo {}",
                    side_name(self.form.side)
                ),
            );
            if is_wayland() {
                hint(ui, "Su Wayland il passaggio verso il client si fa con il tasto rapido; il ritorno dal bordo funziona sempre.");
            }
            hint(
                ui,
                "Tasto rapido: ScrollLock oppure Ctrl destro + Shift destro.",
            );
        });

        section(ui, "Abbinamento");
        card(ui, |ui| {
            hint(
                ui,
                "Dopo «Salva e avvia» la finestra mostrerà un codice di 6 cifre: \
                 sull'altro PC basta inserire quello. Nient'altro da copiare.",
            );
            egui::CollapsingHeader::new(
                RichText::new("Avanzate: codice segreto completo")
                    .color(pal::MUTED)
                    .size(13.0),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let w = ui.available_width() - 170.0;
                    ui.add(
                        TextEdit::singleline(&mut self.form.secret)
                            .font(TextStyle::Monospace)
                            .desired_width(w),
                    );
                    let secret = self.form.secret.clone();
                    let label = if self.just_copied(ui) {
                        "Copiato!"
                    } else {
                        "Copia"
                    };
                    if ui.button(label).clicked() {
                        self.copy(ui, &secret);
                    }
                    if ui.button("Nuovo").clicked() {
                        self.form.secret = crate::crypto::generate_secret();
                    }
                });
                hint(
                    ui,
                    "Cambiandolo, i PC già abbinati dovranno abbinarsi di nuovo.",
                );
            });
        });
    }

    fn client_form(&mut self, ui: &mut egui::Ui) {
        section(ui, "A quale server collegarsi?");
        card(ui, |ui| {
            ui.horizontal(|ui| {
                let w = ui.available_width() - 150.0;
                ui.add(
                    TextEdit::singleline(&mut self.form.server)
                        .hint_text("es. 192.168.1.10")
                        .desired_width(w),
                );
                if self.scanning.is_some() {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new("Ricerca…").color(pal::MUTED));
                } else if ui.button("Cerca in rete").clicked() {
                    let (tx, rx) = mpsc::channel();
                    let ctx = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(discovery::scan(Duration::from_millis(1500)));
                        ctx.request_repaint();
                    });
                    self.scanning = Some(rx);
                    self.found = None;
                }
            });
            match &self.found {
                Some(list) if list.is_empty() => hint(
                    ui,
                    "Nessun server trovato. Avvia Ponte come server sull'altro PC, oppure scrivi qui il suo indirizzo IP.",
                ),
                Some(list) => {
                    for f in list.clone() {
                        if server_row(ui, &f, self.form.server.trim() == f.addr) {
                            self.form.server = f.addr.clone();
                        }
                    }
                }
                None => hint(ui, "«Cerca in rete» trova da solo i server Ponte sulla tua rete locale."),
            }
        });

        section(ui, "Codice di abbinamento");
        card(ui, |ui| {
            hint(
                ui,
                "Inserisci le 6 cifre mostrate nella finestra di Ponte sul server.",
            );
            ui.add(
                TextEdit::singleline(&mut self.form.code)
                    .font(FontId::monospace(28.0))
                    .hint_text("000 000")
                    .char_limit(7)
                    .align(Align2::CENTER_CENTER)
                    .desired_width(200.0),
            );
            if self.pairing.is_some() {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label(RichText::new("Abbinamento con il server…").color(pal::MUTED));
                });
            } else if self.cfg.is_some() && self.form.secret.len() >= 16 {
                hint(
                    ui,
                    "Questo PC è già abbinato: lascia vuoto per mantenere l'abbinamento attuale.",
                );
            }
            egui::CollapsingHeader::new(
                RichText::new("Avanzate: incolla il codice segreto completo")
                    .color(pal::MUTED)
                    .size(13.0),
            )
            .show(ui, |ui| {
                ui.add(
                    TextEdit::singleline(&mut self.form.secret)
                        .font(TextStyle::Monospace)
                        .hint_text("codice segreto del server")
                        .desired_width(f32::INFINITY),
                );
            });
        });
    }

    fn status_view(&mut self, ui: &mut egui::Ui) {
        let st = status::get();
        let Some(cfg) = self.cfg.clone() else { return };
        let server = cfg.mode == Mode::Server;
        let peer = match (st.peer.is_empty(), st.peer_addr.is_empty()) {
            (true, _) => "altro PC".to_string(),
            (false, true) => st.peer.clone(),
            (false, false) => format!("{} ({})", st.peer, st.peer_addr),
        };
        let ips = self.local_ips(ui);
        let hotkey = "ScrollLock (o Ctrl destro + Shift destro)";

        let (title, sub): (String, String) = match (st.phase, server) {
            (Phase::Starting, _) => ("Avvio in corso…".into(), String::new()),
            (Phase::Listening, _) => (
                "In attesa dell'altro PC".into(),
                "Sull'altro PC scegli «Client», premi «Cerca in rete» e inserisci il codice qui sotto.".into(),
            ),
            (Phase::Connecting, _) => ("Collegamento al server…".into(), st.detail.clone()),
            (Phase::Connected, true) => (
                format!("Collegato a «{peer}»"),
                if cfg.server.edge_switch && !is_wayland() {
                    format!(
                        "Porta il mouse oltre il bordo {} oppure premi {hotkey}.",
                        side_name(cfg.server.client_side)
                    )
                } else {
                    format!("Premi {hotkey} per passare all'altro PC.")
                },
            ),
            (Phase::Connected, false) => (
                format!("Collegato a «{peer}»"),
                "Pronto: il mouse del server può arrivare su questo PC.".into(),
            ),
            (Phase::Controlling, true) => (
                format!("Stai usando «{peer}»"),
                format!("Per tornare esci dal bordo dello schermo oppure premi {hotkey}."),
            ),
            (Phase::Controlling, false) => (
                format!("In uso da «{peer}»"),
                "Mouse e tastiera del server stanno lavorando su questo PC.".into(),
            ),
            (Phase::Error, _) => ("Si è verificato un problema".into(), st.detail.clone()),
        };

        // Scheda di stato principale
        let (color, _) = phase_badge(st.phase);
        Frame::new()
            .fill(color.gamma_multiply(0.10))
            .stroke(Stroke::new(1.5, color.gamma_multiply(0.7)))
            .corner_radius(CornerRadius::same(16))
            .inner_margin(Margin::same(18))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                    let t = ui.input(|i| i.time) as f32;
                    let waiting = matches!(
                        st.phase,
                        Phase::Listening | Phase::Connecting | Phase::Starting
                    );
                    if waiting {
                        let k = (t * 2.5).sin() * 0.5 + 0.5;
                        ui.painter().circle_filled(
                            r.center(),
                            7.0 + 6.0 * k,
                            color.gamma_multiply(0.25 * (1.0 - k)),
                        );
                        ui.ctx().request_repaint();
                    }
                    ui.painter().circle_filled(r.center(), 7.0, color);
                    ui.label(RichText::new(title).size(20.0).strong());
                });
                if !sub.is_empty() {
                    ui.label(RichText::new(sub).color(pal::MUTED));
                }
                if st.phase == Phase::Error {
                    if let Some(h) = error_hint(&st.detail) {
                        ui.label(RichText::new(h).color(pal::AMBER));
                    }
                    if ui.button("Riprova").clicked() {
                        restart();
                    }
                }
                if st.phase == Phase::Connecting && (st.detail.contains("secret") || st.detail.contains("rifiutato")) {
                    ui.label(
                        RichText::new("Il server non riconosce questo PC: apri «Impostazioni» e inserisci di nuovo il codice di abbinamento.")
                            .color(pal::AMBER),
                    );
                }
                if let Some(rtt) = st.rtt_us {
                    ui.label(
                        RichText::new(format!("Latenza di rete {:.2} ms", rtt as f64 / 1000.0))
                            .color(pal::GREEN)
                            .size(13.0),
                    );
                }
            });

        if server {
            let now = ui.input(|i| i.time);
            if now - self.fw_checked > 30.0 {
                self.fw_checked = now;
                crate::firewall::refresh();
            }
            firewall_card(ui);
        }

        if server {
            let waiting = matches!(st.phase, Phase::Listening | Phase::Starting);
            if waiting {
                section(ui, "Abbina un nuovo PC");
                card(ui, pairing_card);
            } else {
                ui.add_space(4.0);
                egui::CollapsingHeader::new(RichText::new("Abbina un altro PC").color(pal::MUTED))
                    .show(ui, |ui| card(ui, pairing_card));
            }
        }

        section(ui, "Dettagli");
        let ip = if ips.is_empty() {
            "non rilevato".to_string()
        } else {
            ips.join(", ")
        };
        card(ui, |ui| {
            egui::Grid::new("info")
                .num_columns(2)
                .spacing(vec2(18.0, 10.0))
                .show(ui, |ui| {
                    let k =
                        |ui: &mut egui::Ui, t: &str| ui.label(RichText::new(t).color(pal::MUTED));
                    k(ui, "Ruolo");
                    ui.label(if server {
                        "Server (mouse e tastiera)"
                    } else {
                        "Client (comandato)"
                    });
                    ui.end_row();
                    k(ui, "Questo PC");
                    ui.label(cfg.display_name());
                    ui.end_row();
                    k(ui, "Indirizzo IP");
                    ui.label(RichText::new(ip).monospace());
                    ui.end_row();
                    if server {
                        k(ui, "Porta");
                        let configured = cfg
                            .server
                            .listen
                            .rsplit(':')
                            .next()
                            .unwrap_or("24800")
                            .to_string();
                        match st.listen_port {
                            Some(p) if p.to_string() != configured => ui.label(
                                RichText::new(format!("{p}  (la {configured} era occupata)"))
                                    .color(pal::AMBER),
                            ),
                            Some(p) => ui.label(p.to_string()),
                            None => ui.label(configured),
                        };
                        ui.end_row();
                        k(ui, "Altro PC");
                        ui.label(format!("lato {}", side_name(cfg.server.client_side)));
                        ui.end_row();
                    } else {
                        k(ui, "Server");
                        ui.label(if st.peer.is_empty() {
                            cfg.server_addr()
                        } else {
                            format!("{}  ·  {}", st.peer, cfg.server_addr())
                        });
                        ui.end_row();
                    }
                    k(ui, "Codice segreto");
                    ui.horizontal(|ui| {
                        let shown = if self.show_secret {
                            cfg.secret.clone()
                        } else {
                            format!("{}…", cfg.secret.chars().take(6).collect::<String>())
                        };
                        ui.label(RichText::new(shown).monospace());
                        if ui
                            .small_button(if self.show_secret {
                                "Nascondi"
                            } else {
                                "Mostra"
                            })
                            .clicked()
                        {
                            self.show_secret = !self.show_secret;
                        }
                        let label = if self.just_copied(ui) {
                            "Copiato!"
                        } else {
                            "Copia"
                        };
                        if ui.small_button(label).clicked() {
                            self.copy(ui, &cfg.secret);
                        }
                    });
                    ui.end_row();
                    k(ui, "Appunti");
                    ui.label(if cfg.clipboard.enabled {
                        "testo e immagini condivisi"
                    } else {
                        "non condivisi"
                    });
                    ui.end_row();
                    k(ui, "Avvio automatico");
                    let mut on = cfg.autostart;
                    if ui
                        .checkbox(&mut on, "all'accesso, con la finestra ridotta")
                        .changed()
                    {
                        self.set_autostart(on);
                    }
                    ui.end_row();
                });
        });

        ui.add_space(4.0);
        egui::CollapsingHeader::new(RichText::new("Registro attività").color(pal::MUTED)).show(
            ui,
            |ui| {
                Frame::new()
                    .fill(Color32::from_rgb(0x08, 0x0A, 0x1C))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .stick_to_bottom(true)
                            .show(ui, |ui| {
                                let logs = status::logs();
                                if logs.is_empty() {
                                    ui.label(RichText::new("Nessun evento.").color(pal::MUTED));
                                }
                                for (lvl, line) in logs {
                                    let c = match lvl {
                                        log::Level::Error => pal::RED,
                                        log::Level::Warn => pal::AMBER,
                                        _ => pal::MUTED,
                                    };
                                    ui.label(RichText::new(line).monospace().size(12.5).color(c));
                                }
                            });
                    });
            },
        );

        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("Impostazioni").clicked() {
                self.view = View::Setup;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Esci").clicked() {
                    std::process::exit(0);
                }
            });
        });
        hint(
            ui,
            "Chiudendo la finestra la condivisione si ferma: riducila a icona per lasciarla attiva.",
        );
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            hint(ui, format!("Ponte {}", crate::update::CURRENT));
            if ui.small_button("Controlla aggiornamenti").clicked() {
                crate::update::check(true);
            }
        });
    }
}

/// Avviso firewall sul server, con il pulsante per autorizzare la rete locale.
fn firewall_card(ui: &mut egui::Ui) {
    use crate::firewall::Status;
    let st = crate::firewall::status();
    if matches!(st, Status::Ok | Status::Unknown) {
        return;
    }
    let color = if matches!(st, Status::Failed(_)) {
        pal::RED
    } else {
        pal::AMBER
    };
    Frame::new()
        .fill(color.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Il firewall potrebbe bloccare l'altro PC").size(16.0).strong());
            let what = if cfg!(windows) { "il firewall di Windows" } else { "ufw" };
            hint(
                ui,
                format!(
                    "Per ricevere il collegamento serve consentire Ponte su {what}, solo dalla rete locale. \
                     Si fa una volta sola e chiede i permessi di amministratore."
                ),
            );
            if let Status::Failed(msg) = &st {
                ui.label(RichText::new(msg).color(pal::RED));
            }
            if st == Status::Applying {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new());
                    ui.label("Conferma la richiesta di autorizzazione…");
                });
            } else {
                let btn = egui::Button::new(RichText::new("Consenti in rete locale").strong().color(Color32::WHITE))
                    .fill(pal::INDIGO);
                if ui.add(btn).clicked() {
                    crate::firewall::allow_lan();
                }
            }
        });
    ui.add_space(8.0);
}

/// Riquadro in alto con lo stato degli aggiornamenti.
fn update_banner(ui: &mut egui::Ui) {
    use crate::update::State;
    let state = crate::update::state();
    let (color, show) = match &state {
        State::Available(_) | State::Downloading { .. } | State::Installing(_) => (pal::CYAN, true),
        State::UpToDate | State::Checking => (pal::GREEN, true),
        State::Failed(_) => (pal::RED, true),
        State::Idle => (pal::MUTED, false),
    };
    if !show {
        return;
    }
    Frame::new()
        .fill(color.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            match state {
                State::Available(rel) => {
                    ui.label(RichText::new(format!("Nuova versione {} disponibile", rel.version)).size(16.0).strong());
                    hint(ui, format!("Hai la {}. L'aggiornamento si installa e riavvia Ponte da solo.", crate::update::CURRENT));
                    ui.horizontal(|ui| {
                        let btn = egui::Button::new(RichText::new("Aggiorna ora").strong().color(Color32::WHITE)).fill(pal::INDIGO);
                        if ui.add(btn).clicked() {
                            crate::update::install(rel.clone());
                        }
                        ui.hyperlink_to("Novità", &rel.page);
                    });
                }
                State::Downloading { version, done, total } => {
                    ui.label(format!("Scaricamento della versione {version}…"));
                    let frac = if total > 0 { done as f32 / total as f32 } else { 0.0 };
                    ui.add(egui::ProgressBar::new(frac).show_percentage());
                }
                State::Installing(version) => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label(format!("Installazione della versione {version}…"));
                    });
                    if cfg!(target_os = "linux") {
                        hint(ui, "Inserisci la password nella finestra di sistema per completare l'installazione.");
                    }
                }
                State::Checking => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new());
                        ui.label("Controllo aggiornamenti…");
                    });
                }
                State::UpToDate => {
                    ui.label(RichText::new(format!("Hai già l'ultima versione ({}).", crate::update::CURRENT)).color(pal::GREEN));
                }
                State::Failed(msg) => {
                    ui.label(RichText::new("Aggiornamento non riuscito").color(pal::RED).strong());
                    hint(ui, msg);
                    if ui.small_button("Riprova").clicked() {
                        crate::update::check(true);
                    }
                }
                State::Idle => {}
            }
        });
    ui.add_space(8.0);
}

/// Riga cliccabile di un server trovato in rete: nome e IP.
fn server_row(ui: &mut egui::Ui, f: &Found, selected: bool) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 50.0), Sense::click());
    let p = ui.painter();
    let fill = if selected {
        pal::CYAN.gamma_multiply(0.16)
    } else if resp.hovered() {
        pal::CARD_HOVER
    } else {
        Color32::from_rgb(0x12, 0x16, 0x3B)
    };
    p.rect_filled(rect, CornerRadius::same(10), fill);
    let stroke = if selected {
        Stroke::new(1.5, pal::CYAN)
    } else {
        Stroke::new(1.0, pal::BORDER)
    };
    p.rect_stroke(rect, CornerRadius::same(10), stroke, StrokeKind::Inside);
    paint_monitor(
        p,
        pos2(rect.left() + 26.0, rect.center().y + 2.0),
        24.0,
        pal::VIOLET,
        pal::BG,
    );
    p.text(
        pos2(rect.left() + 50.0, rect.center().y - 9.0),
        Align2::LEFT_CENTER,
        &f.name,
        FontId::proportional(15.0),
        pal::TEXT,
    );
    p.text(
        pos2(rect.left() + 50.0, rect.center().y + 10.0),
        Align2::LEFT_CENTER,
        &f.addr,
        FontId::monospace(12.5),
        pal::MUTED,
    );
    if selected {
        p.text(
            pos2(rect.right() - 14.0, rect.center().y),
            Align2::RIGHT_CENTER,
            "selezionato",
            FontId::proportional(12.5),
            pal::CYAN,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .clicked()
}

/// Codice di abbinamento in grande, cifra per cifra.
fn pairing_card(ui: &mut egui::Ui) {
    let (code, left) = crate::pairing::current_code();
    hint(
        ui,
        "Sull'altro PC: scegli «Client», premi «Cerca in rete», seleziona questo PC e inserisci:",
    );
    let (bw, bh, gap, mid) = (46.0, 62.0, 8.0, 18.0);
    let total = 6.0 * bw + 4.0 * gap + mid;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), bh + 8.0), Sense::hover());
    let p = ui.painter();
    let mut x = rect.center().x - total / 2.0;
    for (i, ch) in code.chars().enumerate() {
        if i == 3 {
            x += mid - gap;
        }
        let r = Rect::from_min_size(pos2(x, rect.top() + 4.0), vec2(bw, bh));
        p.rect_filled(r, CornerRadius::same(10), pal::BG);
        p.rect_stroke(
            r,
            CornerRadius::same(10),
            Stroke::new(1.5, pal::INDIGO),
            StrokeKind::Inside,
        );
        p.text(
            r.center(),
            Align2::CENTER_CENTER,
            ch,
            FontId::monospace(32.0),
            pal::TEXT,
        );
        x += bw + gap;
    }
    ui.vertical_centered(|ui| {
        let mins = left.as_secs().div_ceil(60);
        hint(
            ui,
            format!("Valido ancora {mins} min · cambia dopo l'uso o dopo 3 tentativi sbagliati"),
        );
        if ui.small_button("Nuovo codice").clicked() {
            crate::pairing::new_code();
        }
    });
}

fn phase_badge(phase: Phase) -> (Color32, &'static str) {
    match phase {
        Phase::Starting => (pal::MUTED, "avvio"),
        Phase::Listening => (pal::AMBER, "in attesa"),
        Phase::Connecting => (pal::AMBER, "connessione"),
        Phase::Connected => (pal::GREEN, "collegato"),
        Phase::Controlling => (pal::CYAN, "attivo"),
        Phase::Error => (pal::RED, "errore"),
    }
}

/// Suggerimenti pratici per gli errori più comuni.
fn error_hint(detail: &str) -> Option<&'static str> {
    if detail.contains("sudo") {
        None // il messaggio contiene già le istruzioni
    } else if detail.contains("/dev/input") {
        Some("Esegui nel terminale:  sudo usermod -aG input $USER  poi esci e rientra nella sessione.")
    } else if detail.contains("uinput") {
        Some("Installa il pacchetto .deb di Ponte (regola udev) ed esci e rientra nella sessione.")
    } else if detail.contains("ascoltare") || detail.contains("in use") || detail.contains("10048")
    {
        Some("La porta è già usata: forse Ponte è già aperto in un'altra finestra o come servizio (systemctl --user stop ponte).")
    } else {
        None
    }
}
