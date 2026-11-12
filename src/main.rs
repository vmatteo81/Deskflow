//! Ponte — mouse, tastiera e appunti condivisi tra PC Linux e Windows.

// Su Windows niente console nera all'avvio con doppio clic; da terminale
// la CLI si riaggancia alla console del genitore (vedi `attach_console`).
#![cfg_attr(windows, windows_subsystem = "windows")]

mod client;
mod clipboard;
mod config;
mod crypto;
mod discovery;
mod firewall;
mod gui;
// Tabelle Windows usate solo dalla build Windows (testate ovunque).
#[cfg_attr(not(windows), allow(dead_code))]
mod keys;
mod net;
mod pairing;
mod platform;
mod protocol;
mod server;
mod status;
#[cfg(test)]
mod tests_e2e;
mod update;

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use config::{Config, Mode};

#[derive(Parser)]
#[command(
    name = "ponte",
    version,
    about = "Mouse, tastiera e appunti condivisi tra due PC (Linux/Windows)"
)]
struct Cli {
    /// File di configurazione (default: cartella di config dell'utente)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    /// Log dettagliati (-v debug, -vv trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Crea il file di configurazione con un segreto nuovo
    Init {
        /// Ruolo di questo PC
        #[arg(long, value_enum, default_value = "server")]
        mode: ModeArg,
        /// Indirizzo del server (per il client)
        #[arg(long, default_value = "")]
        server: String,
        /// Riusa un segreto esistente (copialo dall'altro PC)
        #[arg(long)]
        secret: Option<String>,
        /// Sovrascrive un file esistente
        #[arg(long)]
        force: bool,
    },
    /// Apre la finestra di Ponte (default quando non si passano comandi)
    Gui,
    /// Avvia senza finestra secondo la `mode` del file di configurazione
    Run,
    /// Avvia come server (il PC con mouse e tastiera)
    Server,
    /// Avvia come client (il PC controllato)
    Client {
        /// Indirizzo del server, sovrascrive la configurazione
        #[arg(long)]
        server: Option<String>,
    },
    /// Abbina questo PC (client) a un server con il codice di 6 cifre
    Pair {
        /// Indirizzo del server (IP o nome, porta opzionale)
        #[arg(long)]
        server: String,
        /// Codice di 6 cifre mostrato dal server
        #[arg(long)]
        code: String,
    },
    /// Scarica e installa l'ultima versione da GitHub
    Update,
    /// Elenca i nomi dei tasti utilizzabili nelle hotkey
    Keys,
    /// Mostra i dispositivi di input rilevati (Linux)
    Devices,
    /// Stampa il percorso del file di configurazione
    ConfigPath,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ModeArg {
    Server,
    Client,
}

fn main() {
    attach_console();
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    status::init_logging(level);
    platform::init_process();
    update::cleanup_previous();

    let gui_mode = matches!(cli.cmd, None | Some(Cmd::Gui));
    if let Err(e) = run(cli) {
        eprintln!("errore: {e:#}");
        if gui_mode {
            platform::show_error(&format!("{e:#}"));
        }
        std::process::exit(1);
    }
}

/// Windows: con il sottosistema "windows" stdout/stderr non esistono; se
/// lanciati da un terminale ci si aggancia alla sua console.
fn attach_console() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn run(cli: Cli) -> Result<()> {
    let path = cli.config.clone().unwrap_or_else(config::default_path);
    let cmd = match cli.cmd {
        Some(c) => c,
        None if gui::available() => Cmd::Gui,
        None => Cmd::Run,
    };
    match cmd {
        Cmd::Gui => gui::run(path),
        Cmd::Init {
            mode,
            server,
            secret,
            force,
        } => init(&path, mode, &server, secret, force),
        Cmd::Pair { server, code } => pair(&path, &server, &code),
        Cmd::Update => update::update_cli(),
        Cmd::ConfigPath => {
            println!("{}", path.display());
            Ok(())
        }
        Cmd::Keys => {
            for n in keys::all_names() {
                println!("{n}");
            }
            Ok(())
        }
        Cmd::Devices => {
            #[cfg(target_os = "linux")]
            platform::linux::list_devices();
            #[cfg(not(target_os = "linux"))]
            println!(
                "Su Windows ponte usa gli hook di sistema: nessun dispositivo da configurare."
            );
            Ok(())
        }
        Cmd::Run => {
            let cfg = load(&path)?;
            match cfg.mode {
                Mode::Server => server::run(cfg),
                Mode::Client => client::run(cfg),
            }
        }
        Cmd::Server => {
            let mut cfg = load(&path)?;
            cfg.mode = Mode::Server;
            server::run(cfg)
        }
        Cmd::Client { server } => {
            let mut cfg = load(&path)?;
            cfg.mode = Mode::Client;
            if let Some(s) = server {
                cfg.client.server = s;
            }
            cfg.validate()?;
            client::run(cfg)
        }
    }
}

fn load(path: &std::path::Path) -> Result<Config> {
    if !path.exists() {
        bail!(
            "configurazione non trovata in {}\n\
             Creala con:  ponte init --mode server   (sul PC con mouse e tastiera)\n\
             e poi:       ponte init --mode client --server IP-DEL-SERVER --secret SEGRETO   (sull'altro)",
            path.display()
        );
    }
    let cfg = Config::load(path)?;
    log::debug!("configurazione: {}", path.display());
    Ok(cfg)
}

fn init(
    path: &PathBuf,
    mode: ModeArg,
    server: &str,
    secret: Option<String>,
    force: bool,
) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "{} esiste già (usa --force per sovrascriverlo)",
            path.display()
        );
    }
    let mode = match mode {
        ModeArg::Server => Mode::Server,
        ModeArg::Client => Mode::Client,
    };
    let secret = secret.unwrap_or_else(crypto::generate_secret);
    let text = config::template(mode, &secret, server);
    let cfg: Config = toml::from_str(&text)?;
    if mode == Mode::Client && server.is_empty() {
        println!("Nota: imposta [client] server nel file prima di avviare.");
    } else {
        cfg.validate()?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("impossibile creare {}", dir.display()))?;
    }
    std::fs::write(path, text)
        .with_context(|| format!("impossibile scrivere {}", path.display()))?;
    config::restrict_permissions(path);
    println!("Configurazione creata: {}", path.display());
    println!("Segreto (identico sull'altro PC): {secret}");
    Ok(())
}

fn pair(path: &std::path::Path, server: &str, code: &str) -> Result<()> {
    let mut cfg = if path.exists() {
        Config::load(path)?
    } else {
        toml::from_str(&config::template(
            Mode::Client,
            &crypto::generate_secret(),
            server,
        ))?
    };
    cfg.mode = Mode::Client;
    cfg.client.server = server.to_string();
    let addr = cfg.server_addr();
    let (secret, server_name) = pairing::pair(&addr, code, &cfg.display_name())?;
    cfg.secret = secret;
    cfg.client.server_name = Some(server_name.clone()).filter(|n| !n.is_empty());
    cfg.validate()?;
    config::save(path, &cfg)?;
    println!(
        "Abbinato con «{server_name}» ({addr}). Configurazione: {}",
        path.display()
    );
    println!("Avvia con:  ponte run");
    Ok(())
}
