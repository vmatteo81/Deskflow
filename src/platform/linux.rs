//! Linux: lettura diretta da evdev (/dev/input) e iniezione tramite uinput.
//! Funziona sotto X11 e Wayland perché lavora sotto il compositor.
//! Su X11 si usa in più XQueryPointer per il passaggio al bordo.

use std::collections::HashSet;
use std::os::fd::{AsRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use evdev::uinput::{VirtualDevice, VirtualDeviceBuilder};
use evdev::{
    AbsInfo, AbsoluteAxisType, AttributeSet, Device, EventType, InputEvent, Key, PropType,
    RelativeAxisType, UinputAbsSetup,
};

use super::xkb;
use super::{Injector, Rect, ServerPlatform};
use crate::keys::{CharKey, Level};
use crate::protocol::Button;
use crate::server::engine::{Engine, Input};

// EVIOCGRAB = _IOW('E', 0x90, int)
const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
const OWN_PREFIX: &str = "Ponte Virtual";

// ------------------------------------------------------------------ schermo

struct X11 {
    conn: x11rb::rust_connection::RustConnection,
    root: u32,
    size: (i32, i32),
}

impl X11 {
    /// Solo su sessioni X11 vere: sotto Wayland XWayland non conosce la
    /// posizione del cursore fuori dalle sue finestre.
    fn connect() -> Option<X11> {
        if std::env::var("XDG_SESSION_TYPE")
            .map(|v| v == "wayland")
            .unwrap_or(false)
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var_os("DISPLAY").is_none()
        {
            return None;
        }
        use x11rb::connection::Connection;
        let (conn, screen) = x11rb::connect(None).ok()?;
        let s = &conn.setup().roots[screen];
        let (root, size) = (
            s.root,
            (s.width_in_pixels as i32, s.height_in_pixels as i32),
        );
        Some(X11 { conn, root, size })
    }

    fn pointer(&self) -> Option<(i32, i32)> {
        use x11rb::protocol::xproto::ConnectionExt;
        let r = self.conn.query_pointer(self.root).ok()?.reply().ok()?;
        Some((r.root_x as i32, r.root_y as i32))
    }

    fn warp(&self, x: i32, y: i32) {
        use x11rb::connection::Connection;
        use x11rb::protocol::xproto::ConnectionExt;
        let _ = self
            .conn
            .warp_pointer(x11rb::NONE, self.root, 0, 0, 0, 0, x as i16, y as i16);
        let _ = self.conn.flush();
    }
}

/// Risoluzione del desktop: X11 se disponibile (anche XWayland), altrimenti
/// somma delle modalità dei monitor collegati letta da /sys/class/drm.
pub fn detect_screen() -> Option<(i32, i32)> {
    if std::env::var_os("DISPLAY").is_some() {
        if let Ok((conn, screen)) = x11rb::connect(None) {
            use x11rb::connection::Connection;
            let s = &conn.setup().roots[screen];
            if s.width_in_pixels > 0 {
                return Some((s.width_in_pixels as i32, s.height_in_pixels as i32));
            }
        }
    }
    let mut w = 0;
    let mut h = 0;
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let p = entry.path();
        let connected = std::fs::read_to_string(p.join("status"))
            .map(|s| s.trim() == "connected")
            .unwrap_or(false);
        if !connected {
            continue;
        }
        let modes = std::fs::read_to_string(p.join("modes")).unwrap_or_default();
        if let Some((mw, mh)) = modes.lines().next().and_then(|l| l.split_once('x')) {
            let mh: String = mh.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let (Ok(mw), Ok(mh)) = (mw.parse::<i32>(), mh.parse::<i32>()) {
                w += mw;
                h = h.max(mh);
            }
        }
    }
    (w > 0 && h > 0).then_some((w, h))
}

// ------------------------------------------------------------ dispositivi

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Keyboard,
    Mouse,
    Touchpad,
}

pub fn classify(dev: &Device) -> Option<Kind> {
    if dev.name().unwrap_or("").starts_with(OWN_PREFIX) {
        return None;
    }
    let keys = dev.supported_keys();
    let has = |k: Key| keys.is_some_and(|s| s.contains(k));
    let rel = dev.supported_relative_axes();
    let has_rel = |r: RelativeAxisType| rel.is_some_and(|s| s.contains(r));
    let abs = dev.supported_absolute_axes();
    let has_abs = |a: AbsoluteAxisType| abs.is_some_and(|s| s.contains(a));

    if has_rel(RelativeAxisType::REL_X) && has_rel(RelativeAxisType::REL_Y) && has(Key::BTN_LEFT) {
        return Some(Kind::Mouse);
    }
    if has_abs(AbsoluteAxisType::ABS_X)
        && has(Key::BTN_TOOL_FINGER)
        && has(Key::BTN_TOUCH)
        && !dev.properties().contains(PropType::DIRECT)
    {
        return Some(Kind::Touchpad);
    }
    if has(Key::KEY_A) && has(Key::KEY_Z) && has(Key::KEY_SPACE) && has(Key::KEY_ENTER) {
        return Some(Kind::Keyboard);
    }
    None
}

struct DevEntry {
    path: PathBuf,
    fd: RawFd,
    kind: Kind,
    grabbed: bool,
}

pub struct LinuxPlatform {
    x11: Option<Mutex<X11>>,
    bounds: Rect,
    devices: Mutex<Vec<DevEntry>>,
    grab_pointer: AtomicBool,
    grab_keyboard: AtomicBool,
    keymap: xkb::Shared,
}

fn set_grab(fd: RawFd, on: bool) -> bool {
    // SAFETY: ioctl su un fd evdev aperto; l'argomento è un int per valore.
    unsafe { libc::ioctl(fd, EVIOCGRAB as _, on as libc::c_int) == 0 }
}

impl LinuxPlatform {
    fn want(&self, kind: Kind) -> bool {
        match kind {
            Kind::Keyboard => self.grab_keyboard.load(Ordering::SeqCst),
            Kind::Mouse | Kind::Touchpad => self.grab_pointer.load(Ordering::SeqCst),
        }
    }

    fn register(&self, path: &Path, fd: RawFd, kind: Kind) {
        let mut d = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        let want = self.want(kind);
        let grabbed = want && set_grab(fd, true);
        d.push(DevEntry {
            path: path.to_path_buf(),
            fd,
            kind,
            grabbed,
        });
    }

    fn unregister(&self, path: &Path) {
        let mut d = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        d.retain(|e| e.path != path);
    }

    fn known(&self) -> HashSet<PathBuf> {
        self.devices
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|e| e.path.clone())
            .collect()
    }
}

impl ServerPlatform for LinuxPlatform {
    fn bounds(&self) -> Rect {
        self.bounds
    }

    fn cursor_pos(&self) -> Option<(i32, i32)> {
        self.x11.as_ref()?.lock().ok()?.pointer()
    }

    fn warp(&self, x: i32, y: i32) {
        if let Some(x11) = &self.x11 {
            if let Ok(x11) = x11.lock() {
                x11.warp(x, y);
            }
        }
    }

    fn capture(&self, pointer: bool, keyboards: bool) {
        let mut d = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        self.grab_pointer.store(pointer, Ordering::SeqCst);
        self.grab_keyboard.store(keyboards, Ordering::SeqCst);
        for e in d.iter_mut() {
            let want = self.want(e.kind);
            if want != e.grabbed {
                if set_grab(e.fd, want) || !want {
                    e.grabbed = want;
                } else {
                    log::warn!("impossibile catturare {}", e.path.display());
                }
            }
        }
    }

    fn char_for(&self, code: u16, level: Level) -> Option<char> {
        self.keymap.char_for(code, level)
    }
}

type Slot = Arc<OnceLock<Arc<Engine>>>;

pub fn start_capture(slot: Slot) -> Result<Arc<dyn ServerPlatform>> {
    let x11 = X11::connect();
    let (w, h) = x11
        .as_ref()
        .map(|x| x.size)
        .or_else(detect_screen)
        .unwrap_or((1920, 1080));
    if x11.is_none() {
        log::warn!(
            "sessione Wayland: posizione del cursore non accessibile, passa al client con la hotkey \
             (il ritorno dal bordo del client funziona comunque)"
        );
    }
    let plat = Arc::new(LinuxPlatform {
        x11: x11.map(Mutex::new),
        bounds: Rect { x: 0, y: 0, w, h },
        devices: Mutex::new(Vec::new()),
        grab_pointer: AtomicBool::new(false),
        grab_keyboard: AtomicBool::new(false),
        keymap: xkb::Shared::start(),
    });

    let found = scan(&plat, &slot);
    if found == 0 {
        anyhow::bail!(
            "nessuna tastiera o mouse leggibile in /dev/input.\n\
             Aggiungi l'utente al gruppo input e riaccedi:  sudo usermod -aG input $USER"
        );
    }
    // Hotplug: ricontrolla periodicamente i nuovi dispositivi.
    let (p, s) = (plat.clone(), slot);
    std::thread::Builder::new()
        .name("ponte-hotplug".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(2));
            scan(&p, &s);
        })?;
    Ok(plat)
}

fn scan(plat: &Arc<LinuxPlatform>, slot: &Slot) -> usize {
    let known = plat.known();
    let mut started = 0;
    let Ok(dir) = std::fs::read_dir("/dev/input") else {
        return 0;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        let is_event = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("event"));
        if !is_event || known.contains(&path) {
            continue;
        }
        let Ok(dev) = Device::open(&path) else {
            continue;
        };
        let Some(kind) = classify(&dev) else { continue };
        log::info!(
            "input: {} ({:?}) {}",
            dev.name().unwrap_or("?"),
            kind,
            path.display()
        );
        plat.register(&path, dev.as_raw_fd(), kind);
        let (p, s) = (plat.clone(), slot.clone());
        let spawned = std::thread::Builder::new()
            .name("ponte-evdev".into())
            .spawn(move || reader(path, dev, kind, p, s));
        if spawned.is_ok() {
            started += 1;
        }
    }
    started + known.len()
}

fn button_from_evdev(code: u16) -> Option<Button> {
    Some(match Key::new(code) {
        Key::BTN_LEFT => Button::Left,
        Key::BTN_RIGHT => Button::Right,
        Key::BTN_MIDDLE => Button::Middle,
        Key::BTN_SIDE | Key::BTN_BACK => Button::Back,
        Key::BTN_EXTRA | Key::BTN_FORWARD => Button::Forward,
        _ => return None,
    })
}

fn button_to_evdev(b: Button) -> Key {
    match b {
        Button::Left => Key::BTN_LEFT,
        Button::Right => Key::BTN_RIGHT,
        Button::Middle => Key::BTN_MIDDLE,
        Button::Back => Key::BTN_SIDE,
        Button::Forward => Key::BTN_EXTRA,
    }
}

/// Touchpad catturato: lo convertiamo noi in movimento, scroll a due dita e tap.
struct Touchpad {
    res: (f64, f64),
    fingers: u8,
    max_fingers: u8,
    touching: bool,
    cur: (Option<i32>, Option<i32>),
    last: Option<(i32, i32)>,
    down_at: Option<Instant>,
    travel_mm: f64,
}

const TP_PX_PER_MM: f64 = 6.0;
const TP_SCROLL_PER_MM: f64 = 24.0;

impl Touchpad {
    fn new(dev: &Device) -> Self {
        let mut res = (10.0, 10.0);
        if let Ok(abs) = dev.get_abs_state() {
            let r = |i: usize| {
                let a = abs[i];
                if a.resolution > 0 {
                    a.resolution as f64
                } else {
                    ((a.maximum - a.minimum) as f64 / 100.0).max(1.0)
                }
            };
            res = (
                r(AbsoluteAxisType::ABS_X.0 as usize),
                r(AbsoluteAxisType::ABS_Y.0 as usize),
            );
        }
        Self {
            res,
            fingers: 0,
            max_fingers: 0,
            touching: false,
            cur: (None, None),
            last: None,
            down_at: None,
            travel_mm: 0.0,
        }
    }

    fn key(&mut self, code: u16, value: i32, out: &mut Vec<Input>) {
        let n = match Key::new(code) {
            Key::BTN_TOOL_FINGER => 1,
            Key::BTN_TOOL_DOUBLETAP => 2,
            Key::BTN_TOOL_TRIPLETAP => 3,
            Key::BTN_TOOL_QUADTAP => 4,
            Key::BTN_TOOL_QUINTTAP => 5,
            Key::BTN_TOUCH => {
                if value != 0 {
                    self.touching = true;
                    self.down_at = Some(Instant::now());
                    self.travel_mm = 0.0;
                    self.max_fingers = self.fingers.max(1);
                } else {
                    self.touching = false;
                    self.last = None;
                    if let Some(t) = self.down_at.take() {
                        if t.elapsed() < Duration::from_millis(180) && self.travel_mm < 1.5 {
                            let button = match self.max_fingers {
                                2 => Button::Right,
                                3 => Button::Middle,
                                _ => Button::Left,
                            };
                            out.push(Input::Button { button, down: true });
                            out.push(Input::Button {
                                button,
                                down: false,
                            });
                        }
                    }
                }
                return;
            }
            _ => return,
        };
        if value != 0 {
            self.fingers = n;
            self.max_fingers = self.max_fingers.max(n);
        } else if self.fingers == n {
            self.fingers = 0;
        }
        self.last = None; // cambio di dita: niente salti
    }

    fn abs(&mut self, code: u16, value: i32) {
        if code == AbsoluteAxisType::ABS_X.0 {
            self.cur.0 = Some(value);
        } else if code == AbsoluteAxisType::ABS_Y.0 {
            self.cur.1 = Some(value);
        }
    }

    fn frame(&mut self, out: &mut Vec<Input>) {
        let (Some(x), Some(y)) = self.cur else { return };
        if !self.touching {
            return;
        }
        if let Some((lx, ly)) = self.last {
            let mx = (x - lx) as f64 / self.res.0;
            let my = (y - ly) as f64 / self.res.1;
            self.travel_mm += (mx * mx + my * my).sqrt();
            match self.fingers {
                1 => out.push(Input::Motion {
                    dx: mx * TP_PX_PER_MM,
                    dy: my * TP_PX_PER_MM,
                }),
                2 => {
                    // Scorrimento "naturale": il contenuto segue le dita.
                    let c = |v: f64| v.round().clamp(-32000.0, 32000.0) as i16;
                    let (wx, wy) = (c(-mx * TP_SCROLL_PER_MM), c(my * TP_SCROLL_PER_MM));
                    if wx != 0 || wy != 0 {
                        out.push(Input::Wheel { dx: wx, dy: wy });
                    }
                }
                _ => {}
            }
        }
        self.last = Some((x, y));
    }
}

fn reader(path: PathBuf, mut dev: Device, kind: Kind, plat: Arc<LinuxPlatform>, slot: Slot) {
    let rel = dev.supported_relative_axes();
    let hires_v = rel.is_some_and(|r| r.contains(RelativeAxisType::REL_WHEEL_HI_RES));
    let hires_h = rel.is_some_and(|r| r.contains(RelativeAxisType::REL_HWHEEL_HI_RES));
    let mut tp = (kind == Kind::Touchpad).then(|| Touchpad::new(&dev));
    let mut motion = (0.0f64, 0.0f64);
    let mut wheel = (0i32, 0i32);
    let mut out: Vec<Input> = Vec::with_capacity(16);

    let flush = |motion: &mut (f64, f64), wheel: &mut (i32, i32), out: &mut Vec<Input>| {
        if motion.0 != 0.0 || motion.1 != 0.0 {
            out.push(Input::Motion {
                dx: motion.0,
                dy: motion.1,
            });
            *motion = (0.0, 0.0);
        }
        if wheel.0 != 0 || wheel.1 != 0 {
            let c = |v: i32| v.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            out.push(Input::Wheel {
                dx: c(wheel.0),
                dy: c(wheel.1),
            });
            *wheel = (0, 0);
        }
    };

    loop {
        let events: Vec<InputEvent> = match dev.fetch_events() {
            Ok(it) => it.collect(),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                log::info!("dispositivo rimosso: {} ({e})", path.display());
                break;
            }
        };
        for ev in events {
            let (code, value) = (ev.code(), ev.value());
            match ev.event_type() {
                EventType::RELATIVE => match RelativeAxisType(code) {
                    RelativeAxisType::REL_X => motion.0 += value as f64,
                    RelativeAxisType::REL_Y => motion.1 += value as f64,
                    RelativeAxisType::REL_WHEEL if !hires_v => wheel.1 += value * 120,
                    RelativeAxisType::REL_WHEEL_HI_RES => wheel.1 += value,
                    RelativeAxisType::REL_HWHEEL if !hires_h => wheel.0 += value * 120,
                    RelativeAxisType::REL_HWHEEL_HI_RES => wheel.0 += value,
                    _ => {}
                },
                EventType::KEY => {
                    if let Some(button) = button_from_evdev(code) {
                        flush(&mut motion, &mut wheel, &mut out);
                        if value != 2 {
                            out.push(Input::Button {
                                button,
                                down: value != 0,
                            });
                        }
                    } else if code < 0x100 {
                        out.push(Input::Key {
                            code,
                            down: value != 0,
                            repeat: value == 2,
                        });
                    } else if let Some(tp) = tp.as_mut() {
                        tp.key(code, value, &mut out);
                    }
                }
                EventType::ABSOLUTE => {
                    if let Some(tp) = tp.as_mut() {
                        tp.abs(code, value);
                    }
                }
                EventType::SYNCHRONIZATION => {
                    flush(&mut motion, &mut wheel, &mut out);
                    if let Some(tp) = tp.as_mut() {
                        tp.frame(&mut out);
                    }
                }
                _ => {}
            }
        }
        flush(&mut motion, &mut wheel, &mut out);
        if let Some(engine) = slot.get() {
            for ev in out.drain(..) {
                engine.input(ev);
            }
        } else {
            out.clear();
        }
    }
    plat.unregister(&path);
}

// ------------------------------------------------------------- iniezione

pub struct UinputInjector {
    keyboard: VirtualDevice,
    pointer: VirtualDevice,
    screen: Rect,
    wheel_acc: (i32, i32),
    keymap: xkb::Shared,
}

impl UinputInjector {
    pub fn new(screen_override: Option<[u32; 2]>) -> Result<Self> {
        let (w, h) = screen_override
            .map(|[w, h]| (w as i32, h as i32))
            .or_else(detect_screen)
            .unwrap_or((1920, 1080));
        let hint =
            "serve accesso a /dev/uinput: installa il pacchetto .deb (regola udev) e riaccedi, \
                    oppure  sudo modprobe uinput && sudo setfacl -m u:$USER:rw /dev/uinput";

        let mut keys = AttributeSet::<Key>::new();
        for code in 1..=248u16 {
            keys.insert(Key::new(code));
        }
        let keyboard = VirtualDeviceBuilder::new()
            .context(hint)?
            .name("Ponte Virtual Keyboard")
            .with_keys(&keys)?
            .build()
            .context(hint)?;

        let mut buttons = AttributeSet::<Key>::new();
        for b in [
            Key::BTN_LEFT,
            Key::BTN_RIGHT,
            Key::BTN_MIDDLE,
            Key::BTN_SIDE,
            Key::BTN_EXTRA,
        ] {
            buttons.insert(b);
        }
        let mut rel = AttributeSet::<RelativeAxisType>::new();
        for r in [
            RelativeAxisType::REL_WHEEL,
            RelativeAxisType::REL_HWHEEL,
            RelativeAxisType::REL_WHEEL_HI_RES,
            RelativeAxisType::REL_HWHEEL_HI_RES,
        ] {
            rel.insert(r);
        }
        // Puntatore assoluto (come il "tablet" delle macchine virtuali): così
        // conosciamo sempre la posizione esatta, anche sotto Wayland.
        let ax = UinputAbsSetup::new(AbsoluteAxisType::ABS_X, AbsInfo::new(0, 0, w - 1, 0, 0, 0));
        let ay = UinputAbsSetup::new(AbsoluteAxisType::ABS_Y, AbsInfo::new(0, 0, h - 1, 0, 0, 0));
        let pointer = VirtualDeviceBuilder::new()
            .context(hint)?
            .name("Ponte Virtual Pointer")
            .with_keys(&buttons)?
            .with_relative_axes(&rel)?
            .with_absolute_axis(&ax)?
            .with_absolute_axis(&ay)?
            .build()
            .context(hint)?;
        Ok(Self {
            keyboard,
            pointer,
            screen: Rect { x: 0, y: 0, w, h },
            wheel_acc: (0, 0),
            keymap: xkb::Shared::start(),
        })
    }

    fn emit_ptr(&mut self, evs: &[InputEvent]) {
        if let Err(e) = self.pointer.emit(evs) {
            log::warn!("uinput: {e}");
        }
    }
}

impl Injector for UinputInjector {
    fn screen(&self) -> Rect {
        self.screen
    }

    fn cursor_pos(&self) -> Option<(i32, i32)> {
        None
    }

    fn move_to(&mut self, x: i32, y: i32) {
        self.emit_ptr(&[
            InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_X.0, x),
            InputEvent::new(EventType::ABSOLUTE, AbsoluteAxisType::ABS_Y.0, y),
        ]);
    }

    fn button(&mut self, button: Button, down: bool) {
        let k = button_to_evdev(button);
        self.emit_ptr(&[InputEvent::new(EventType::KEY, k.code(), down as i32)]);
    }

    fn wheel(&mut self, dx: i16, dy: i16) {
        let mut evs = Vec::with_capacity(4);
        if dy != 0 {
            evs.push(InputEvent::new(
                EventType::RELATIVE,
                RelativeAxisType::REL_WHEEL_HI_RES.0,
                dy as i32,
            ));
            self.wheel_acc.1 += dy as i32;
            let notches = self.wheel_acc.1 / 120;
            if notches != 0 {
                self.wheel_acc.1 -= notches * 120;
                evs.push(InputEvent::new(
                    EventType::RELATIVE,
                    RelativeAxisType::REL_WHEEL.0,
                    notches,
                ));
            }
        }
        if dx != 0 {
            evs.push(InputEvent::new(
                EventType::RELATIVE,
                RelativeAxisType::REL_HWHEEL_HI_RES.0,
                dx as i32,
            ));
            self.wheel_acc.0 += dx as i32;
            let notches = self.wheel_acc.0 / 120;
            if notches != 0 {
                self.wheel_acc.0 -= notches * 120;
                evs.push(InputEvent::new(
                    EventType::RELATIVE,
                    RelativeAxisType::REL_HWHEEL.0,
                    notches,
                ));
            }
        }
        if !evs.is_empty() {
            self.emit_ptr(&evs);
        }
    }

    fn key(&mut self, code: u16, down: bool) {
        if code == 0 || code > 248 {
            return;
        }
        if let Err(e) = self
            .keyboard
            .emit(&[InputEvent::new(EventType::KEY, code, down as i32)])
        {
            log::warn!("uinput: {e}");
        }
    }

    fn char_key(&mut self, ch: char) -> Option<CharKey> {
        self.keymap.char_key(ch)
    }
}

/// `ponte devices`: elenco dei dispositivi e di come vengono classificati.
pub fn list_devices() {
    let mut list: Vec<_> = evdev::enumerate().collect();
    list.sort_by(|a, b| a.0.cmp(&b.0));
    if list.is_empty() {
        println!(
            "Nessun dispositivo leggibile: serve il gruppo `input` (sudo usermod -aG input $USER)."
        );
    }
    for (path, dev) in list {
        let kind = classify(&dev)
            .map(|k| format!("{k:?}"))
            .unwrap_or_else(|| "ignorato".into());
        println!(
            "{:<22} {:<10} {}",
            path.display(),
            kind,
            dev.name().unwrap_or("?")
        );
    }
}
