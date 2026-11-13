//! Una sola istanza di Ponte per utente: due motori catturerebbero lo stesso
//! input. Il "lucchetto" è una porta TCP su 127.0.0.1; se è già presa si
//! chiede all'istanza esistente di mostrare la finestra e si esce.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const ADDR: &str = "127.0.0.1:24799";
const SHOW: &str = "ponte-show";
/// Variabile impostata quando Ponte riavvia sé stesso: il nuovo processo
/// aspetta che il vecchio liberi il lucchetto.
const RESTART_ENV: &str = "PONTE_RESTARTING";

static ON_SHOW: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

/// Registra cosa fare quando un'altra istanza chiede di mostrarsi.
pub fn on_show(f: impl Fn() + Send + Sync + 'static) {
    let _ = ON_SHOW.set(Box::new(f));
}

/// Prende il lucchetto. `false` = c'è già un'altra istanza (avvisata).
pub fn acquire() -> bool {
    let restarting = std::env::var_os(RESTART_ENV).is_some();
    std::env::remove_var(RESTART_ENV);
    let deadline = Instant::now() + Duration::from_secs(if restarting { 10 } else { 0 });
    loop {
        match TcpListener::bind(ADDR) {
            Ok(l) => {
                let _ = std::thread::Builder::new()
                    .name("ponte-instance".into())
                    .spawn(move || serve(l));
                return true;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                if Instant::now() >= deadline {
                    if notify() {
                        return false;
                    }
                    // La porta è di qualcun altro (non risponde come Ponte): si va avanti.
                    log::warn!("porta {ADDR} occupata da un altro programma");
                    return true;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                log::warn!("controllo istanza unica non disponibile: {e}");
                return true;
            }
        }
    }
}

fn serve(l: TcpListener) {
    for stream in l.incoming().flatten() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let mut line = String::new();
        let mut s = stream;
        if BufReader::new(&s).read_line(&mut line).is_ok() && line.trim() == SHOW {
            let _ = s.write_all(b"ok\n");
            log::info!("Ponte rilanciato: mostro la finestra");
            if let Some(f) = ON_SHOW.get() {
                f();
            }
        }
    }
}

fn notify() -> bool {
    let Ok(mut s) = TcpStream::connect_timeout(&ADDR.parse().unwrap(), Duration::from_secs(1))
    else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(1)));
    if s.write_all(format!("{SHOW}\n").as_bytes()).is_err() {
        return false;
    }
    let mut reply = String::new();
    BufReader::new(&s).read_line(&mut reply).is_ok() && reply.trim() == "ok"
}

/// Riavvia Ponte (stessi argomenti, finestra visibile) e termina questo processo.
pub fn restart_with(exe: std::path::PathBuf) -> ! {
    let _ = std::process::Command::new(exe)
        .env(RESTART_ENV, "1")
        .spawn();
    std::process::exit(0);
}
