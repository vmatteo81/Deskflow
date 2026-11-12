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

pub fn run(path: PathBuf) -> anyhow::Result<()> {
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
        Box::new(move |cc| Ok(Box::new(App::new(cc, path)))),
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
}

struct App {
    path: PathBuf,
    view: View,
    logo: egui::TextureHandle,
    form: Form,
    cfg: Option<Config>,
    started: bool,
    show_secret: bool,
    scanning: Option<mpsc::Receiver<Vec<Found>>>,
    found: Option<Vec<Found>>,
    error: Option<String>,
    copied_at: Option<f64>,
    frames: u32,
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
    fn new(cc: &eframe::CreationContext<'_>, path: PathBuf) -> Self {
        setup_style(&cc.egui_ctx);
        let ctx = cc.egui_ctx.clone();
        status::on_change(move || ctx.request_repaint());

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
            },
            cfg: None,
            started: false,
            show_secret: false,
            scanning: None,
            found: None,
            error: None,
            copied_at: None,
            frames: 0,
        };
        if std::env::var("PONTE_SCREENSHOT_MODE").as_deref() == Ok("client") {
            app.form.mode = Mode::Client;
            app.form.secret.clear();
        }

        if path.exists() {
            match Config::load(&path) {
                Ok(cfg) => {
                    app.fill_form(&cfg);
                    app.start(cfg);
                }
                Err(e) => app.error = Some(format!("{e:#}")),
            }
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
                self.start(cfg);
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
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
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::process::Command::new(exe).spawn();
    }
    std::process::exit(0);
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
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        self.debug_screenshot(ui.ctx());

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
                area.show(ui, |ui| match self.view {
                    View::Setup => self.setup_view(ui),
                    View::Status => self.status_view(ui),
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
        let valid = self.form.secret.trim().len() >= 16
            && (self.form.mode == Mode::Server || !self.form.server.trim().is_empty());
        let label = if self.started {
            "Salva e riavvia"
        } else {
            "Salva e avvia"
        };
        if primary_button(ui, label, valid) {
            self.save_and_start();
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

        section(ui, "Codice segreto");
        card(ui, |ui| {
            hint(ui, "Sull'altro PC incolla esattamente questo codice: protegge e cifra il collegamento.");
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
                        let selected = self.form.server.trim() == f.addr;
                        let text = RichText::new(format!("{}   {}", f.name, f.addr));
                        if ui.selectable_label(selected, text).clicked() {
                            self.form.server = f.addr.clone();
                        }
                    }
                }
                None => hint(ui, "«Cerca in rete» trova da solo i server Ponte sulla tua rete locale."),
            }
        });

        section(ui, "Codice segreto");
        card(ui, |ui| {
            hint(ui, "Incolla il codice mostrato dal server (sul server: «Codice segreto», poi «Copia»).");
            ui.add(
                TextEdit::singleline(&mut self.form.secret)
                    .font(TextStyle::Monospace)
                    .hint_text("codice del server")
                    .desired_width(f32::INFINITY),
            );
        });
    }

    fn status_view(&mut self, ui: &mut egui::Ui) {
        let st = status::get();
        let Some(cfg) = self.cfg.clone() else { return };
        let server = cfg.mode == Mode::Server;
        let peer = if st.peer.is_empty() {
            "altro PC".to_string()
        } else {
            st.peer.clone()
        };
        let hotkey = "ScrollLock (o Ctrl destro + Shift destro)";

        let (title, sub): (String, String) = match (st.phase, server) {
            (Phase::Starting, _) => ("Avvio in corso…".into(), String::new()),
            (Phase::Listening, _) => (
                "In attesa dell'altro PC".into(),
                format!(
                    "Sul client premi «Cerca in rete» oppure inserisci l'indirizzo {}.",
                    if st.detail.is_empty() {
                        "di questo PC".into()
                    } else {
                        st.detail.clone()
                    }
                ),
            ),
            (Phase::Connecting, _) => ("Ricerca del server…".into(), st.detail.clone()),
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
                if let Some(rtt) = st.rtt_us {
                    ui.label(
                        RichText::new(format!("Latenza di rete {:.2} ms", rtt as f64 / 1000.0))
                            .color(pal::GREEN)
                            .size(13.0),
                    );
                }
            });

        section(ui, "Dettagli");
        let ip = discovery::local_ip().unwrap_or_else(|| "sconosciuto".into());
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
                    ui.label(format!("{}  ·  {ip}", cfg.display_name()));
                    ui.end_row();
                    if server {
                        k(ui, "Porta");
                        ui.label(cfg.server.listen.rsplit(':').next().unwrap_or("24800"));
                        ui.end_row();
                        k(ui, "Altro PC");
                        ui.label(format!("lato {}", side_name(cfg.server.client_side)));
                        ui.end_row();
                    } else {
                        k(ui, "Server");
                        ui.label(cfg.server_addr());
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
                        "condivisi"
                    } else {
                        "non condivisi"
                    });
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
                hint(ui, "Chiudendo la finestra la condivisione si ferma.");
            });
        });
    }
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
        Some("La porta è già usata: forse Ponte è già aperto in un'altra finestra o come servizio.")
    } else {
        None
    }
}
