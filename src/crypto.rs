//! Cifratura del canale: chiave pre-condivisa (segreto nel file di config),
//! nonce casuali di sessione per ciascun lato e HKDF-SHA256 per derivare due
//! chiavi ChaCha20-Poly1305 indipendenti (una per direzione). Ogni frame usa
//! un contatore come nonce, quindi replay e riordino vengono rifiutati.

use anyhow::{anyhow, Result};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;

pub const NONCE_LEN: usize = 32;
#[cfg(test)]
pub const TAG_LEN: usize = 16;

pub struct SessionKeys {
    pub send: [u8; 32],
    pub recv: [u8; 32],
}

/// Deriva le chiavi di sessione. Client e server ottengono le stesse due
/// chiavi, scambiate di ruolo.
pub fn derive(
    secret: &[u8],
    client_nonce: &[u8; NONCE_LEN],
    server_nonce: &[u8; NONCE_LEN],
    is_server: bool,
) -> SessionKeys {
    let mut salt = [0u8; NONCE_LEN * 2];
    salt[..NONCE_LEN].copy_from_slice(client_nonce);
    salt[NONCE_LEN..].copy_from_slice(server_nonce);
    let hk = Hkdf::<Sha256>::new(Some(&salt), secret);
    let mut c2s = [0u8; 32];
    let mut s2c = [0u8; 32];
    hk.expand(b"ponte v1 client->server", &mut c2s)
        .expect("lunghezza HKDF valida");
    hk.expand(b"ponte v1 server->client", &mut s2c)
        .expect("lunghezza HKDF valida");
    if is_server {
        SessionKeys {
            send: s2c,
            recv: c2s,
        }
    } else {
        SessionKeys {
            send: c2s,
            recv: s2c,
        }
    }
}

fn nonce(counter: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&counter.to_le_bytes());
    Nonce::from(n)
}

pub struct Sealer {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl Sealer {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            counter: 0,
        }
    }

    pub fn seal(&mut self, plaintext: &[u8]) -> Vec<u8> {
        let n = nonce(self.counter);
        self.counter += 1;
        self.cipher
            .encrypt(&n, plaintext)
            .expect("cifratura in memoria")
    }
}

pub struct Opener {
    cipher: ChaCha20Poly1305,
    counter: u64,
}

impl Opener {
    pub fn new(key: &[u8; 32]) -> Self {
        Self {
            cipher: ChaCha20Poly1305::new(Key::from_slice(key)),
            counter: 0,
        }
    }

    pub fn open(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let n = nonce(self.counter);
        let pt = self
            .cipher
            .decrypt(&n, ciphertext)
            .map_err(|_| anyhow!("frame non autenticato (segreto diverso o dati manomessi)"))?;
        self.counter += 1;
        Ok(pt)
    }
}

/// Genera un nuovo segreto casuale (256 bit, esadecimale).
pub fn generate_secret() -> String {
    let mut b = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    hex::encode(b)
}

pub fn random_nonce() -> [u8; NONCE_LEN] {
    let mut b = [0u8; NONCE_LEN];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut b);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_sides_agree() {
        let (c, s) = (random_nonce(), random_nonce());
        let ck = derive(b"segreto", &c, &s, false);
        let sk = derive(b"segreto", &c, &s, true);
        let mut client_tx = Sealer::new(&ck.send);
        let mut server_rx = Opener::new(&sk.recv);
        let mut server_tx = Sealer::new(&sk.send);
        let mut client_rx = Opener::new(&ck.recv);
        for i in 0..5u8 {
            let ct = client_tx.seal(&[i; 10]);
            assert_eq!(ct.len(), 10 + TAG_LEN);
            assert_eq!(server_rx.open(&ct).unwrap(), vec![i; 10]);
            let ct = server_tx.seal(b"pong");
            assert_eq!(client_rx.open(&ct).unwrap(), b"pong");
        }
    }

    #[test]
    fn wrong_secret_fails() {
        let (c, s) = (random_nonce(), random_nonce());
        let ck = derive(b"uno", &c, &s, false);
        let sk = derive(b"due", &c, &s, true);
        let ct = Sealer::new(&ck.send).seal(b"ciao");
        assert!(Opener::new(&sk.recv).open(&ct).is_err());
    }

    #[test]
    fn replay_is_rejected() {
        let (c, s) = (random_nonce(), random_nonce());
        let ck = derive(b"x", &c, &s, false);
        let sk = derive(b"x", &c, &s, true);
        let mut tx = Sealer::new(&ck.send);
        let mut rx = Opener::new(&sk.recv);
        let first = tx.seal(b"a");
        rx.open(&first).unwrap();
        assert!(rx.open(&first).is_err());
    }
}
