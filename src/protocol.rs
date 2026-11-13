//! Protocollo binario compatto. Ogni messaggio viaggia in un frame cifrato
//! (vedi `net.rs`); qui c'è solo la codifica del contenuto in chiaro.
//! I tasti usano i keycode Linux evdev come formato canonico.

use anyhow::{bail, Result};

pub const VERSION: u16 = 2;
pub const DEFAULT_PORT: u16 = 24800;
/// Limite massimo per il testo degli appunti in un singolo messaggio.
pub const MAX_CLIPBOARD: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Button {
    Left = 1,
    Right = 2,
    Middle = 3,
    Back = 4,
    Forward = 5,
}

impl Button {
    pub const ALL: [Button; 5] = [
        Button::Left,
        Button::Right,
        Button::Middle,
        Button::Back,
        Button::Forward,
    ];

    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Button::Left,
            2 => Button::Right,
            3 => Button::Middle,
            4 => Button::Back,
            5 => Button::Forward,
            _ => return None,
        })
    }

    pub fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// Lato in cui si trova il client rispetto al server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
#[repr(u8)]
pub enum Side {
    Left = 0,
    Right = 1,
    Top = 2,
    Bottom = 3,
}

impl Side {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Side::Left,
            1 => Side::Right,
            2 => Side::Top,
            3 => Side::Bottom,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// client → server, primo messaggio dopo l'handshake.
    Hello {
        version: u16,
        name: String,
        width: u32,
        height: u32,
    },
    /// server → client, risposta a Hello. `side` = dove sta il client.
    Welcome {
        version: u16,
        name: String,
        side: Side,
    },
    /// server → client: il cursore entra nel client. `along` è la posizione
    /// lungo il bordo (0..=65535). Con `center` il cursore va al centro.
    Enter {
        along: u16,
        center: bool,
    },
    /// client → server: il cursore è uscito dal bordo di ritorno.
    /// server → client: il controllo torna al server (hotkey).
    Leave {
        along: u16,
    },
    Move {
        dx: i16,
        dy: i16,
    },
    Button {
        button: Button,
        down: bool,
    },
    /// In 1/120 di scatto (come Windows e REL_WHEEL_HI_RES). dy>0 = su, dx>0 = destra.
    Wheel {
        dx: i16,
        dy: i16,
    },
    Key {
        code: u16,
        down: bool,
    },
    /// server → client: il tasto `code` sul server (col suo layout) produce
    /// il carattere `ch`. Il client scrive quel carattere, non il tasto fisico.
    Text {
        code: u16,
        ch: char,
    },
    Clipboard(String),
    /// Immagine negli appunti (es. uno screenshot), codificata PNG.
    ClipboardImage(Vec<u8>),
    Ping(u64),
    Pong(u64),
}

mod tag {
    pub const HELLO: u8 = 1;
    pub const WELCOME: u8 = 2;
    pub const ENTER: u8 = 3;
    pub const LEAVE: u8 = 4;
    pub const MOVE: u8 = 5;
    pub const BUTTON: u8 = 6;
    pub const WHEEL: u8 = 7;
    pub const KEY: u8 = 8;
    pub const CLIPBOARD: u8 = 9;
    pub const PING: u8 = 10;
    pub const PONG: u8 = 11;
    pub const TEXT: u8 = 12;
    pub const CLIPBOARD_IMAGE: u8 = 13;
}

impl Msg {
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Msg::Hello {
                version,
                name,
                width,
                height,
            } => {
                out.push(tag::HELLO);
                out.extend_from_slice(&version.to_le_bytes());
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
                put_str(out, name);
            }
            Msg::Welcome {
                version,
                name,
                side,
            } => {
                out.push(tag::WELCOME);
                out.extend_from_slice(&version.to_le_bytes());
                out.push(*side as u8);
                put_str(out, name);
            }
            Msg::Enter { along, center } => {
                out.push(tag::ENTER);
                out.extend_from_slice(&along.to_le_bytes());
                out.push(*center as u8);
            }
            Msg::Leave { along } => {
                out.push(tag::LEAVE);
                out.extend_from_slice(&along.to_le_bytes());
            }
            Msg::Move { dx, dy } => {
                out.push(tag::MOVE);
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            Msg::Button { button, down } => {
                out.push(tag::BUTTON);
                out.push(*button as u8);
                out.push(*down as u8);
            }
            Msg::Wheel { dx, dy } => {
                out.push(tag::WHEEL);
                out.extend_from_slice(&dx.to_le_bytes());
                out.extend_from_slice(&dy.to_le_bytes());
            }
            Msg::Key { code, down } => {
                out.push(tag::KEY);
                out.extend_from_slice(&code.to_le_bytes());
                out.push(*down as u8);
            }
            Msg::Text { code, ch } => {
                out.push(tag::TEXT);
                out.extend_from_slice(&code.to_le_bytes());
                out.extend_from_slice(&(*ch as u32).to_le_bytes());
            }
            Msg::Clipboard(text) => {
                out.push(tag::CLIPBOARD);
                out.extend_from_slice(text.as_bytes());
            }
            Msg::ClipboardImage(png) => {
                out.push(tag::CLIPBOARD_IMAGE);
                out.extend_from_slice(png);
            }
            Msg::Ping(t) => {
                out.push(tag::PING);
                out.extend_from_slice(&t.to_le_bytes());
            }
            Msg::Pong(t) => {
                out.push(tag::PONG);
                out.extend_from_slice(&t.to_le_bytes());
            }
        }
    }

    pub fn decode(buf: &[u8]) -> Result<Msg> {
        let (&t, rest) = match buf.split_first() {
            Some(x) => x,
            None => bail!("messaggio vuoto"),
        };
        let mut r = Reader(rest);
        let msg = match t {
            tag::HELLO => Msg::Hello {
                version: r.u16()?,
                width: r.u32()?,
                height: r.u32()?,
                name: r.str()?,
            },
            tag::WELCOME => {
                let version = r.u16()?;
                let side = match Side::from_u8(r.u8()?) {
                    Some(s) => s,
                    None => bail!("lato non valido"),
                };
                Msg::Welcome {
                    version,
                    side,
                    name: r.str()?,
                }
            }
            tag::ENTER => Msg::Enter {
                along: r.u16()?,
                center: r.u8()? != 0,
            },
            tag::LEAVE => Msg::Leave { along: r.u16()? },
            tag::MOVE => Msg::Move {
                dx: r.u16()? as i16,
                dy: r.u16()? as i16,
            },
            tag::BUTTON => {
                let b = r.u8()?;
                let button = match Button::from_u8(b) {
                    Some(b) => b,
                    None => bail!("pulsante sconosciuto {b}"),
                };
                Msg::Button {
                    button,
                    down: r.u8()? != 0,
                }
            }
            tag::WHEEL => Msg::Wheel {
                dx: r.u16()? as i16,
                dy: r.u16()? as i16,
            },
            tag::KEY => Msg::Key {
                code: r.u16()?,
                down: r.u8()? != 0,
            },
            tag::CLIPBOARD => {
                if rest.len() > MAX_CLIPBOARD {
                    bail!("appunti troppo grandi");
                }
                return Ok(Msg::Clipboard(String::from_utf8(rest.to_vec())?));
            }
            tag::CLIPBOARD_IMAGE => {
                if rest.len() > MAX_CLIPBOARD {
                    bail!("immagine negli appunti troppo grande");
                }
                return Ok(Msg::ClipboardImage(rest.to_vec()));
            }
            tag::TEXT => {
                let code = r.u16()?;
                let v = r.u32()?;
                let Some(ch) = char::from_u32(v) else {
                    bail!("carattere non valido {v:#x}")
                };
                Msg::Text { code, ch }
            }
            tag::PING => Msg::Ping(r.u64()?),
            tag::PONG => Msg::Pong(r.u64()?),
            other => bail!("tipo di messaggio sconosciuto {other}"),
        };
        if !r.0.is_empty() {
            bail!("byte in eccesso nel messaggio {t}");
        }
        Ok(msg)
    }

    /// Prova a fondere `next` in `self` (movimenti e rotella consecutivi),
    /// così sotto carico si scrive un frame solo senza perdere precisione.
    pub fn try_merge(&mut self, next: &Msg) -> bool {
        match (self, next) {
            (Msg::Move { dx, dy }, Msg::Move { dx: ndx, dy: ndy })
            | (Msg::Wheel { dx, dy }, Msg::Wheel { dx: ndx, dy: ndy }) => {
                match (dx.checked_add(*ndx), dy.checked_add(*ndy)) {
                    (Some(x), Some(y)) => {
                        *dx = x;
                        *dy = y;
                        true
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    let n = b.len().min(u16::MAX as usize);
    out.extend_from_slice(&(n as u16).to_le_bytes());
    out.extend_from_slice(&b[..n]);
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if self.0.len() < n {
            bail!("messaggio troncato");
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into()?))
    }
    fn str(&mut self) -> Result<String> {
        let n = self.u16()? as usize;
        Ok(String::from_utf8(self.take(n)?.to_vec())?)
    }
}

/// Converte una frazione 0.0..=1.0 nel formato `along` del protocollo.
pub fn frac_to_along(f: f64) -> u16 {
    (f.clamp(0.0, 1.0) * 65535.0).round() as u16
}

pub fn along_to_frac(a: u16) -> f64 {
    a as f64 / 65535.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(m: Msg) {
        let mut buf = Vec::new();
        m.encode(&mut buf);
        assert_eq!(Msg::decode(&buf).unwrap(), m);
    }

    #[test]
    fn all_messages_roundtrip() {
        roundtrip(Msg::Hello {
            version: 1,
            name: "portatile".into(),
            width: 2560,
            height: 1440,
        });
        roundtrip(Msg::Welcome {
            version: 1,
            name: "fisso".into(),
            side: Side::Top,
        });
        roundtrip(Msg::Enter {
            along: 1234,
            center: true,
        });
        roundtrip(Msg::Leave { along: 65535 });
        roundtrip(Msg::Move { dx: -300, dy: 7 });
        roundtrip(Msg::Button {
            button: Button::Forward,
            down: true,
        });
        roundtrip(Msg::Wheel { dx: 0, dy: -120 });
        roundtrip(Msg::Key {
            code: 125,
            down: false,
        });
        roundtrip(Msg::Clipboard("ciao è€ 🚀".into()));
        roundtrip(Msg::Clipboard(String::new()));
        roundtrip(Msg::ClipboardImage(vec![0x89, b'P', b'N', b'G', 0, 255]));
        roundtrip(Msg::Text { code: 39, ch: '@' });
        roundtrip(Msg::Text {
            code: 18, ch: '€'
        });
        roundtrip(Msg::Ping(u64::MAX));
        roundtrip(Msg::Pong(42));
    }

    #[test]
    fn rejects_garbage() {
        assert!(Msg::decode(&[]).is_err());
        assert!(Msg::decode(&[200]).is_err());
        assert!(Msg::decode(&[tag::MOVE, 1]).is_err());
        assert!(Msg::decode(&[tag::LEAVE, 1, 2, 3]).is_err());
        assert!(Msg::decode(&[tag::BUTTON, 9, 1]).is_err());
        // Surrogato UTF-16: non è un carattere valido.
        assert!(Msg::decode(&[tag::TEXT, 1, 0, 0x00, 0xD8, 0, 0]).is_err());
    }

    #[test]
    fn merge_moves_without_overflow() {
        let mut a = Msg::Move { dx: 10, dy: -5 };
        assert!(a.try_merge(&Msg::Move { dx: 3, dy: 5 }));
        assert_eq!(a, Msg::Move { dx: 13, dy: 0 });
        let mut b = Msg::Move {
            dx: i16::MAX,
            dy: 0,
        };
        assert!(!b.try_merge(&Msg::Move { dx: 1, dy: 0 }));
        let mut c = Msg::Move { dx: 1, dy: 1 };
        assert!(!c.try_merge(&Msg::Wheel { dx: 0, dy: 120 }));
    }

    #[test]
    fn along_conversion() {
        assert_eq!(frac_to_along(0.0), 0);
        assert_eq!(frac_to_along(1.0), 65535);
        assert_eq!(frac_to_along(2.0), 65535);
        assert!((along_to_frac(frac_to_along(0.25)) - 0.25).abs() < 1e-4);
    }
}
