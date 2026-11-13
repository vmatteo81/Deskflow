//! Windows: hook low-level di mouse e tastiera (catturano e, se serve,
//! bloccano gli eventi) e SendInput per l'iniezione sul client.

use std::cell::RefCell;
use std::ptr::null_mut;
use std::sync::{Arc, OnceLock};

use anyhow::{bail, Result};
use windows_sys::Win32::Foundation::{LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, MapVirtualKeyExW, SendInput, ToUnicodeEx, VkKeyScanExW, HKL, INPUT, INPUT_0,
    INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MAPVK_VSC_TO_VK_EX,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, VK_CONTROL, VK_LCONTROL, VK_LSHIFT, VK_MENU, VK_RMENU, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW,
    GetSystemMetrics, GetWindowThreadProcessId, SetCursorPos, SetWindowsHookExW, TranslateMessage,
    HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_EXTENDED, LLKHF_INJECTED, LLMHF_INJECTED,
    LLMHF_LOWER_IL_INJECTED, MSG, MSLLHOOKSTRUCT, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use super::{Injector, Rect, ServerPlatform};
use crate::keys::{self, CharKey, KeySet, Level, WinKey};
use crate::protocol::Button;
use crate::server::engine::{Engine, Input, Verdict};

pub fn message_box(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (t, m) = (wide(title), wide(text));
    unsafe {
        MessageBoxW(null_mut(), m.as_ptr(), t.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

pub fn init_process() {
    // Coordinate in pixel fisici anche con scaling al 125/150%.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

fn virtual_screen() -> Rect {
    unsafe {
        Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            w: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1),
            h: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1),
        }
    }
}

fn cursor() -> Option<(i32, i32)> {
    let mut p = POINT { x: 0, y: 0 };
    (unsafe { GetCursorPos(&mut p) } != 0).then_some((p.x, p.y))
}

/// Layout di tastiera della finestra in primo piano (su Windows è per finestra).
fn foreground_layout() -> HKL {
    unsafe {
        let tid = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
        GetKeyboardLayout(tid)
    }
}

/// Carattere prodotto dal tasto `code` col layout attivo.
fn char_for(code: u16, level: Level) -> Option<char> {
    let WinKey::Scan {
        scan,
        extended: false,
    } = keys::evdev_to_win(code)?
    else {
        return None;
    };
    unsafe {
        let hkl = foreground_layout();
        let vk = MapVirtualKeyExW(scan as u32, MAPVK_VSC_TO_VK_EX, hkl);
        if vk == 0 {
            return None;
        }
        let mut state = [0u8; 256];
        if level.shift {
            state[VK_SHIFT as usize] = 0x80;
            state[VK_LSHIFT as usize] = 0x80;
        }
        if level.altgr {
            // AltGr per Windows è Ctrl+Alt.
            for vk in [VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU] {
                state[vk as usize] = 0x80;
            }
        }
        let mut buf = [0u16; 8];
        // Flag 4: non toccare lo stato dei tasti morti del sistema.
        let n = ToUnicodeEx(
            vk,
            scan as u32,
            state.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as i32,
            4,
            hkl,
        );
        if n <= 0 {
            return None; // nessun carattere o tasto morto
        }
        let mut chars = char::decode_utf16(buf[..n as usize].iter().copied());
        match (chars.next(), chars.next()) {
            (Some(Ok(c)), None) => Some(c),
            _ => None,
        }
    }
}

/// Tasto che produce `ch` col layout della finestra in primo piano.
fn char_key(ch: char) -> Option<CharKey> {
    let mut units = [0u16; 2];
    let [u] = ch.encode_utf16(&mut units) else {
        return None;
    };
    unsafe {
        let hkl = foreground_layout();
        let r = VkKeyScanExW(*u, hkl);
        if r == -1 {
            return None;
        }
        let vk = (r & 0xff) as u32;
        let st = (r >> 8) & 0xff;
        let (shift, ctrl, alt) = (st & 1 != 0, st & 2 != 0, st & 4 != 0);
        if st & !7 != 0 || ctrl != alt {
            return None;
        }
        let scan = MapVirtualKeyExW(vk, MAPVK_VK_TO_VSC, hkl);
        let code = keys::win_to_evdev(vk, scan, false)?;
        (keys::is_text_key(code) || code == 57).then_some(CharKey {
            code,
            level: Level {
                shift,
                altgr: ctrl && alt,
            },
        })
    }
}

// ------------------------------------------------------------------ server

type Slot = Arc<OnceLock<Arc<Engine>>>;
static SLOT: OnceLock<Slot> = OnceLock::new();

thread_local! {
    static DOWN: RefCell<KeySet> = RefCell::new(KeySet::default());
}

fn engine() -> Option<&'static Arc<Engine>> {
    SLOT.get()?.get()
}

pub struct WinPlatform;

impl ServerPlatform for WinPlatform {
    fn bounds(&self) -> Rect {
        virtual_screen()
    }
    fn cursor_pos(&self) -> Option<(i32, i32)> {
        cursor()
    }
    fn warp(&self, x: i32, y: i32) {
        unsafe {
            SetCursorPos(x, y);
        }
    }
    fn capture(&self, _mice: bool, _keyboards: bool) {}
    fn char_for(&self, code: u16, level: Level) -> Option<char> {
        char_for(code, level)
    }
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let ms = &*(lparam as *const MSLLHOOKSTRUCT);
        if ms.flags & (LLMHF_INJECTED | LLMHF_LOWER_IL_INJECTED) == 0 {
            if let Some(engine) = engine() {
                let hi = (ms.mouseData >> 16) as u16;
                let xbtn = if hi == 1 {
                    Button::Back
                } else {
                    Button::Forward
                };
                let ev = match wparam as u32 {
                    WM_MOUSEMOVE => Some(Input::MotionAbs {
                        x: ms.pt.x,
                        y: ms.pt.y,
                    }),
                    WM_LBUTTONDOWN => Some(Input::Button {
                        button: Button::Left,
                        down: true,
                    }),
                    WM_LBUTTONUP => Some(Input::Button {
                        button: Button::Left,
                        down: false,
                    }),
                    WM_RBUTTONDOWN => Some(Input::Button {
                        button: Button::Right,
                        down: true,
                    }),
                    WM_RBUTTONUP => Some(Input::Button {
                        button: Button::Right,
                        down: false,
                    }),
                    WM_MBUTTONDOWN => Some(Input::Button {
                        button: Button::Middle,
                        down: true,
                    }),
                    WM_MBUTTONUP => Some(Input::Button {
                        button: Button::Middle,
                        down: false,
                    }),
                    WM_XBUTTONDOWN => Some(Input::Button {
                        button: xbtn,
                        down: true,
                    }),
                    WM_XBUTTONUP => Some(Input::Button {
                        button: xbtn,
                        down: false,
                    }),
                    WM_MOUSEWHEEL => Some(Input::Wheel {
                        dx: 0,
                        dy: hi as i16,
                    }),
                    WM_MOUSEHWHEEL => Some(Input::Wheel {
                        dx: hi as i16,
                        dy: 0,
                    }),
                    _ => None,
                };
                if let Some(ev) = ev {
                    if engine.input(ev) == Verdict::Swallow {
                        return 1;
                    }
                }
            }
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
        if kb.flags & LLKHF_INJECTED == 0 {
            if let Some(engine) = engine() {
                let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
                let extended = kb.flags & LLKHF_EXTENDED != 0;
                if let Some(key) = keys::win_to_evdev(kb.vkCode, kb.scanCode, extended) {
                    let repeat = DOWN.with(|d| {
                        let mut d = d.borrow_mut();
                        if down {
                            !d.insert(key)
                        } else {
                            d.remove(key);
                            false
                        }
                    });
                    if engine.input(Input::Key {
                        code: key,
                        down,
                        repeat,
                    }) == Verdict::Swallow
                    {
                        return 1;
                    }
                }
            }
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

pub fn start_capture(slot: Slot) -> Result<Arc<dyn ServerPlatform>> {
    if SLOT.set(slot).is_err() {
        bail!("cattura già avviata");
    }
    let (tx, rx) = crossbeam_channel::bounded::<Result<(), String>>(1);
    std::thread::Builder::new()
        .name("ponte-hooks".into())
        .spawn(move || unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            let mh = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
            let kh = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
            if mh.is_null() || kh.is_null() {
                let _ = tx.send(Err("SetWindowsHookEx fallita".into()));
                return;
            }
            let _ = tx.send(Ok(()));
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        })?;
    match rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => bail!(e),
        Err(_) => bail!("thread degli hook terminato"),
    }
    Ok(Arc::new(WinPlatform))
}

// ------------------------------------------------------------------ client

pub struct SendInputInjector;

impl SendInputInjector {
    pub fn new() -> Self {
        SendInputInjector
    }
}

fn mouse_input(dx: i32, dy: i32, data: i32, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as _,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_input(vk: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(inputs: &[INPUT]) {
    unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        );
    }
}

impl Injector for SendInputInjector {
    fn screen(&self) -> Rect {
        virtual_screen()
    }

    fn cursor_pos(&self) -> Option<(i32, i32)> {
        cursor()
    }

    fn move_to(&mut self, x: i32, y: i32) {
        let s = virtual_screen();
        let nx = ((x - s.x) as i64 * 65535 / (s.w - 1).max(1) as i64) as i32;
        let ny = ((y - s.y) as i64 * 65535 / (s.h - 1).max(1) as i64) as i32;
        send(&[mouse_input(
            nx,
            ny,
            0,
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
        )]);
    }

    fn button(&mut self, button: Button, down: bool) {
        let (flags, data) = match (button, down) {
            (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (Button::Back, true) => (MOUSEEVENTF_XDOWN, 1),
            (Button::Back, false) => (MOUSEEVENTF_XUP, 1),
            (Button::Forward, true) => (MOUSEEVENTF_XDOWN, 2),
            (Button::Forward, false) => (MOUSEEVENTF_XUP, 2),
        };
        send(&[mouse_input(0, 0, data, flags)]);
    }

    fn wheel(&mut self, dx: i16, dy: i16) {
        if dy != 0 {
            send(&[mouse_input(0, 0, dy as i32, MOUSEEVENTF_WHEEL)]);
        }
        if dx != 0 {
            send(&[mouse_input(0, 0, dx as i32, MOUSEEVENTF_HWHEEL)]);
        }
    }

    fn key(&mut self, code: u16, down: bool) {
        let up = if down { 0 } else { KEYEVENTF_KEYUP };
        match keys::evdev_to_win(code) {
            Some(WinKey::Scan { scan, extended }) => {
                let ext = if extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
                send(&[key_input(0, scan, KEYEVENTF_SCANCODE | ext | up)]);
            }
            Some(WinKey::Vk(vk)) => send(&[key_input(vk, 0, up)]),
            None => log::debug!("tasto {code} non mappato su Windows"),
        }
    }

    fn char_key(&mut self, ch: char) -> Option<CharKey> {
        char_key(ch)
    }

    fn type_char(&mut self, ch: char) -> bool {
        let mut units = [0u16; 2];
        let mut inputs = Vec::with_capacity(4);
        for u in ch.encode_utf16(&mut units).iter() {
            inputs.push(key_input(0, *u, KEYEVENTF_UNICODE));
            inputs.push(key_input(0, *u, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
        send(&inputs);
        true
    }
}
