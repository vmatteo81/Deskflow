//! Ponte — mouse, tastiera e appunti condivisi tra PC Linux e Windows.

mod client;
mod clipboard;
mod config;
mod crypto;
// Tabelle Windows usate solo dalla build Windows (testate ovunque).
#[cfg_attr(not(windows), allow(dead_code))]
mod keys;
mod net;
mod platform;
mod protocol;
mod server;
#[cfg(test)]
mod tests_e2e;

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
    /// Avvia secondo la `mode` del file di configurazione (default)
    Run,
    /// Avvia come server (il PC con mouse e tastiera)
    Server,
    /// Avvia come client (il PC controllato)
    Client {
        /// Indirizzo del server, sovrascrive la configurazione
        #[arg(long)]
        server: Option<String>,
    },
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
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(format!("{level},arboard=error")),
    )
    .format_target(false)
    .init();
    platform::init_process();

    let interactive = cli.cmd.is_none();
    if let Err(e) = run(cli) {
        eprintln!("errore: {e:#}");
        if interactive && cfg!(windows) {
            // Avvio con doppio clic: non chiudere la console prima di aver letto.
            eprintln!("\nPremi Invio per chiudere...");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let path = cli.config.clone().unwrap_or_else(config::default_path);
    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Init {
            mode,
            server,
            secret,
            force,
        } => init(&path, mode, &server, secret, force),
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
    restrict_permissions(path);
    println!("Configurazione creata: {}", path.display());
    println!("Segreto (identico sull'altro PC): {secret}");
    Ok(())
}

fn restrict_permissions(path: &PathBuf) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}
