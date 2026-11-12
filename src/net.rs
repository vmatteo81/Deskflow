//! Trasporto: TCP con Nagle disattivato, handshake con nonce, frame cifrati
//! `[u32 lunghezza][ciphertext+tag]`. Lo scrittore gira su un thread dedicato
//! e accorpa i movimenti in coda in un'unica write.

use std::io::{BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use crossbeam_channel::{Receiver, RecvTimeoutError};

use crate::crypto::{self, Opener, Sealer, NONCE_LEN};
use crate::protocol::{Msg, MAX_CLIPBOARD};

const MAGIC: &[u8; 8] = b"PONTE\x00\x01\x00";
/// Saluto iniziale di una richiesta di abbinamento (vedi `pairing.rs`).
pub const PAIR_MAGIC: &[u8; 8] = b"PONTE\x00P1";

/// Tipo di connessione in arrivo sul server.
pub enum Incoming {
    Session,
    Pair,
}

/// Server: legge il saluto iniziale e capisce cosa vuole il client.
pub fn read_magic(stream: &mut TcpStream) -> Result<Incoming> {
    let mut m = [0u8; 8];
    stream
        .read_exact(&mut m)
        .context("handshake: client muto")?;
    match &m {
        x if x == MAGIC => Ok(Incoming::Session),
        x if x == PAIR_MAGIC => Ok(Incoming::Pair),
        _ => bail!("il peer non parla il protocollo Ponte v1"),
    }
}

/// Connessione TCP con timeout, provando tutti gli indirizzi risolti.
pub fn connect(addr: &str) -> Result<TcpStream> {
    let mut last = None;
    for a in addr
        .to_socket_addrs()
        .with_context(|| format!("indirizzo non valido: {addr}"))?
    {
        match TcpStream::connect_timeout(&a, Duration::from_secs(3)) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    match last {
        Some(e) => Err(e).context("server non raggiungibile"),
        None => bail!("nessun indirizzo per {addr}"),
    }
}
const MAX_FRAME: usize = MAX_CLIPBOARD + 1024;
/// Intervallo dei ping: se il peer tace per `DEAD_AFTER` la connessione è morta.
pub const PING_EVERY: Duration = Duration::from_millis(1000);
pub const DEAD_AFTER: Duration = Duration::from_secs(5);

pub struct FrameReader {
    stream: BufReader<TcpStream>,
    opener: Opener,
}

impl FrameReader {
    pub fn read_msg(&mut self) -> Result<Msg> {
        let mut len = [0u8; 4];
        self.stream.read_exact(&mut len)?;
        let len = u32::from_le_bytes(len) as usize;
        if len > MAX_FRAME {
            bail!("frame troppo grande ({len} byte)");
        }
        let mut buf = vec![0u8; len];
        self.stream.read_exact(&mut buf)?;
        let pt = self.opener.open(&buf)?;
        Msg::decode(&pt)
    }
}

pub struct FrameWriter {
    stream: TcpStream,
    sealer: Sealer,
    plain: Vec<u8>,
    out: Vec<u8>,
}

impl FrameWriter {
    pub fn write_batch(&mut self, msgs: &[Msg]) -> Result<()> {
        self.out.clear();
        for m in msgs {
            self.plain.clear();
            m.encode(&mut self.plain);
            let ct = self.sealer.seal(&self.plain);
            self.out.extend_from_slice(&(ct.len() as u32).to_le_bytes());
            self.out.extend_from_slice(&ct);
        }
        self.stream.write_all(&self.out)?;
        Ok(())
    }

    pub fn send(&mut self, m: Msg) -> Result<()> {
        self.write_batch(std::slice::from_ref(&m))
    }
}

pub fn tune(stream: &TcpStream) -> Result<()> {
    stream.set_nodelay(true)?;
    stream.set_read_timeout(Some(DEAD_AFTER))?;
    stream.set_write_timeout(Some(DEAD_AFTER))?;
    Ok(())
}

fn exchange(
    stream: &mut TcpStream,
    mine: &[u8; NONCE_LEN],
    is_client: bool,
) -> Result<[u8; NONCE_LEN]> {
    let mut hello = Vec::with_capacity(MAGIC.len() + NONCE_LEN);
    hello.extend_from_slice(MAGIC);
    hello.extend_from_slice(mine);
    let mut n = [0u8; NONCE_LEN];
    if is_client {
        stream.write_all(&hello)?;
        let mut theirs = [0u8; MAGIC.len() + NONCE_LEN];
        stream
            .read_exact(&mut theirs)
            .context("handshake: nessuna risposta dal server")?;
        if &theirs[..MAGIC.len()] != MAGIC {
            bail!("il peer non parla il protocollo Ponte v1");
        }
        n.copy_from_slice(&theirs[MAGIC.len()..]);
    } else {
        // Il server ha già letto il magic con `read_magic`.
        stream
            .read_exact(&mut n)
            .context("handshake: client muto")?;
        stream.write_all(&hello)?;
    }
    Ok(n)
}

/// Esegue l'handshake e restituisce (lettore, scrittore) cifrati.
/// Lato server va chiamato dopo `read_magic` che ha restituito `Session`.
pub fn handshake(
    mut stream: TcpStream,
    secret: &[u8],
    is_server: bool,
) -> Result<(FrameReader, FrameWriter)> {
    tune(&stream)?;
    let mine = crypto::random_nonce();
    let theirs = exchange(&mut stream, &mine, !is_server)?;
    let (cn, sn) = if is_server {
        (theirs, mine)
    } else {
        (mine, theirs)
    };
    let keys = crypto::derive(secret, &cn, &sn, is_server);
    let reader = FrameReader {
        stream: BufReader::with_capacity(64 * 1024, stream.try_clone()?),
        opener: Opener::new(&keys.recv),
    };
    let writer = FrameWriter {
        stream,
        sealer: Sealer::new(&keys.send),
        plain: Vec::with_capacity(256),
        out: Vec::with_capacity(4096),
    };
    Ok((reader, writer))
}

/// Ciclo dello scrittore: svuota la coda, fonde i movimenti consecutivi,
/// scrive tutto con una sola syscall e invia un ping quando la linea è ferma.
pub fn writer_loop(mut w: FrameWriter, rx: Receiver<Msg>, epoch: Instant) {
    let mut batch: Vec<Msg> = Vec::with_capacity(64);
    let mut last_ping = Instant::now();
    loop {
        let first = match rx.recv_timeout(PING_EVERY) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => Msg::Ping(epoch.elapsed().as_micros() as u64),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        batch.push(first);
        while batch.len() < 512 {
            match rx.try_recv() {
                Ok(m) => {
                    let merged = batch.last_mut().map(|l| l.try_merge(&m)).unwrap_or(false);
                    if !merged {
                        batch.push(m);
                    }
                }
                Err(_) => break,
            }
        }
        if last_ping.elapsed() >= PING_EVERY && !matches!(batch.last(), Some(Msg::Ping(_))) {
            batch.push(Msg::Ping(epoch.elapsed().as_micros() as u64));
        }
        if matches!(batch.last(), Some(Msg::Ping(_))) {
            last_ping = Instant::now();
        }
        if let Err(e) = w.write_batch(&batch) {
            log::debug!("scrittura fallita: {e}");
            break;
        }
        batch.clear();
    }
    let _ = w.stream.shutdown(std::net::Shutdown::Both);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn handshake_and_messages_over_loopback() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            assert!(matches!(read_magic(&mut s).unwrap(), Incoming::Session));
            let (mut r, mut w) = handshake(s, b"chiave", true).unwrap();
            let m = r.read_msg().unwrap();
            w.send(Msg::Pong(7)).unwrap();
            m
        });
        let s = TcpStream::connect(addr).unwrap();
        let (mut r, mut w) = handshake(s, b"chiave", false).unwrap();
        w.write_batch(&[Msg::Move { dx: 1, dy: 2 }]).unwrap();
        assert_eq!(r.read_msg().unwrap(), Msg::Pong(7));
        assert_eq!(srv.join().unwrap(), Msg::Move { dx: 1, dy: 2 });
    }

    #[test]
    fn wrong_secret_is_detected() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let srv = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            read_magic(&mut s).unwrap();
            let (mut r, _w) = handshake(s, b"giusto", true).unwrap();
            r.read_msg().is_err()
        });
        let s = TcpStream::connect(addr).unwrap();
        let (_r, mut w) = handshake(s, b"sbagliato", false).unwrap();
        w.send(Msg::Ping(1)).unwrap();
        assert!(srv.join().unwrap());
    }
}
