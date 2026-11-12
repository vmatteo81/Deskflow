//! Abbinamento con codice di 6 cifre (come il Bluetooth).
//!
//! Il server mostra un codice; il client lo inserisce. Con SPAKE2 i due PC
//! derivano una chiave comune solo se il codice coincide, senza mai inviarlo:
//! chi intercetta il traffico non può provare i codici "offline", e dopo
//! `MAX_ATTEMPTS` tentativi sbagliati il codice cambia. Con la chiave il server
//! consegna al client il segreto lungo, usato poi per tutte le connessioni.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::Rng;
use spake2::{Ed25519Group, Identity, Password, Spake2};

pub const CODE_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_ATTEMPTS: u8 = 3;
const CONFIRM: &[u8] = b"ponte-pair-ok";
const ID_CLIENT: &[u8] = b"ponte-pair-client-v1";
const ID_SERVER: &[u8] = b"ponte-pair-server-v1";

struct Code {
    digits: String,
    created: Instant,
    attempts: u8,
}

static CODE: Mutex<Option<Code>> = Mutex::new(None);

fn code_slot() -> std::sync::MutexGuard<'static, Option<Code>> {
    CODE.lock().unwrap_or_else(|e| e.into_inner())
}

fn fresh(slot: &mut Option<Code>) -> String {
    let digits = format!("{:06}", rand::rngs::OsRng.gen_range(0..1_000_000u32));
    log::info!("codice di abbinamento: {}", pretty(&digits));
    *slot = Some(Code {
        digits: digits.clone(),
        created: Instant::now(),
        attempts: 0,
    });
    crate::status::update(|_| {});
    digits
}

/// Codice valido in questo momento (ne crea uno nuovo se scaduto) e tempo residuo.
pub fn current_code() -> (String, Duration) {
    let mut slot = code_slot();
    let valid = slot
        .as_ref()
        .filter(|c| c.created.elapsed() < CODE_TTL && c.attempts < MAX_ATTEMPTS)
        .map(|c| {
            (
                c.digits.clone(),
                CODE_TTL.saturating_sub(c.created.elapsed()),
            )
        });
    match valid {
        Some(v) => v,
        None => (fresh(&mut slot), CODE_TTL),
    }
}

pub fn new_code() -> String {
    fresh(&mut code_slot())
}

fn register_failure() {
    let mut slot = code_slot();
    if let Some(c) = slot.as_mut() {
        c.attempts += 1;
        if c.attempts >= MAX_ATTEMPTS {
            log::warn!("troppi tentativi con codice errato: genero un nuovo codice");
            fresh(&mut slot);
        }
    }
}

fn consume() {
    *code_slot() = None;
    crate::status::update(|_| {});
}

/// "482913" → "482 913"
pub fn pretty(code: &str) -> String {
    if code.len() == 6 {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_string()
    }
}

/// Toglie spazi e trattini; restituisce il codice se sono 6 cifre.
pub fn normalize(input: &str) -> Option<String> {
    let d: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    (d.len() == 6 && d.chars().all(|c| c.is_ascii_digit())).then_some(d)
}

fn put_blob(s: &mut TcpStream, data: &[u8]) -> Result<()> {
    s.write_all(&(data.len() as u16).to_le_bytes())?;
    s.write_all(data)?;
    Ok(())
}

fn get_blob(s: &mut TcpStream, max: usize) -> Result<Vec<u8>> {
    let mut len = [0u8; 2];
    s.read_exact(&mut len)?;
    let len = u16::from_le_bytes(len) as usize;
    if len > max {
        bail!("messaggio di abbinamento troppo lungo");
    }
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf)?;
    Ok(buf)
}

fn cipher(key: &[u8]) -> Result<ChaCha20Poly1305> {
    if key.len() < 32 {
        bail!("chiave di abbinamento non valida");
    }
    Ok(ChaCha20Poly1305::new(Key::from_slice(&key[..32])))
}

fn nonce(n: u8) -> Nonce {
    let mut b = [0u8; 12];
    b[0] = n;
    Nonce::from(b)
}

fn spake(code: &str, client: bool) -> (Spake2<Ed25519Group>, Vec<u8>) {
    let (pw, a, b) = (
        Password::new(code.as_bytes()),
        Identity::new(ID_CLIENT),
        Identity::new(ID_SERVER),
    );
    if client {
        Spake2::<Ed25519Group>::start_a(&pw, &a, &b)
    } else {
        Spake2::<Ed25519Group>::start_b(&pw, &a, &b)
    }
}

/// Lato server: il magic di abbinamento è già stato letto da `net::read_magic`.
pub fn serve(mut s: TcpStream, secret: &str, server_name: &str) -> Result<()> {
    let peer = s
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_default();
    let client_name = String::from_utf8_lossy(&get_blob(&mut s, 128)?).to_string();
    let msg_a = get_blob(&mut s, 128)?;
    let (code, _) = current_code();
    let (state, msg_b) = spake(&code, false);
    put_blob(&mut s, &msg_b)?;
    let key = state
        .finish(&msg_a)
        .map_err(|e| anyhow::anyhow!("abbinamento non valido: {e:?}"))?;
    let c = cipher(&key)?;
    let confirm = get_blob(&mut s, 128)?;
    if c.decrypt(&nonce(0), confirm.as_slice()).ok().as_deref() != Some(CONFIRM) {
        register_failure();
        // Rallenta chi prova a indovinare.
        std::thread::sleep(Duration::from_millis(800));
        s.write_all(&[1])?;
        bail!("abbinamento rifiutato per «{client_name}» ({peer}): codice errato");
    }
    s.write_all(&[0])?;
    let payload = format!("{secret}\n{server_name}");
    let sealed = c
        .encrypt(&nonce(1), payload.as_bytes())
        .map_err(|_| anyhow::anyhow!("cifratura fallita"))?;
    put_blob(&mut s, &sealed)?;
    consume();
    log::info!("abbinato «{client_name}» ({peer})");
    Ok(())
}

/// Lato client: restituisce (segreto, nome del server).
pub fn pair(addr: &str, code: &str, my_name: &str) -> Result<(String, String)> {
    let code = normalize(code).context("il codice di abbinamento è di 6 cifre")?;
    let mut s = crate::net::connect(addr)?;
    crate::net::tune(&s)?;
    s.write_all(crate::net::PAIR_MAGIC)?;
    put_blob(&mut s, my_name.as_bytes())?;
    let (state, msg_a) = spake(&code, true);
    put_blob(&mut s, &msg_a)?;
    let msg_b = get_blob(&mut s, 128).context("il server non ha risposto all'abbinamento")?;
    let key = state
        .finish(&msg_b)
        .map_err(|e| anyhow::anyhow!("abbinamento non valido: {e:?}"))?;
    let c = cipher(&key)?;
    let confirm = c
        .encrypt(&nonce(0), CONFIRM)
        .map_err(|_| anyhow::anyhow!("cifratura fallita"))?;
    put_blob(&mut s, &confirm)?;
    let mut ok = [0u8; 1];
    s.read_exact(&mut ok)
        .context("il server ha chiuso l'abbinamento")?;
    if ok[0] != 0 {
        bail!("codice errato o scaduto: controlla le 6 cifre mostrate sul server");
    }
    let sealed = get_blob(&mut s, 4096)?;
    let payload = c
        .decrypt(&nonce(1), sealed.as_slice())
        .map_err(|_| anyhow::anyhow!("risposta del server non valida"))?;
    let payload = String::from_utf8(payload)?;
    let (secret, name) = payload.split_once('\n').unwrap_or((payload.as_str(), ""));
    if secret.len() < 16 {
        bail!("il server ha inviato un segreto non valido");
    }
    Ok((secret.to_string(), name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn server_once(secret: &'static str) -> (String, std::thread::JoinHandle<Result<()>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let h = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            crate::net::tune(&s).unwrap();
            assert!(matches!(
                crate::net::read_magic(&mut s).unwrap(),
                crate::net::Incoming::Pair
            ));
            serve(s, secret, "fisso")
        });
        (addr, h)
    }

    // Un solo test per tutto: il codice è globale al processo.
    #[test]
    fn pairing_flow() {
        let secret = "0123456789abcdef0123456789abcdef";
        let code = new_code();
        // Codice giusto (anche scritto con lo spazio): si riceve il segreto.
        let (addr, h) = server_once(secret);
        let (got, name) = pair(&addr, &pretty(&code), "portatile").unwrap();
        assert_eq!(got, secret);
        assert_eq!(name, "fisso");
        h.join().unwrap().unwrap();
        // Dopo l'uso il codice cambia.
        let (next, _) = current_code();
        // Codice sbagliato: rifiutato da entrambi i lati.
        let wrong = if next == "000000" { "111111" } else { "000000" };
        let (addr, h) = server_once(secret);
        assert!(pair(&addr, wrong, "intruso").is_err());
        assert!(h.join().unwrap().is_err());
        // Formato non valido: errore prima ancora di connettersi.
        assert!(pair("127.0.0.1:1", "12ab", "x").is_err());
        assert_eq!(normalize(" 482-913 "), Some("482913".into()));
    }
}
