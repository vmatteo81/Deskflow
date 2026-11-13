//! Aggiornamento automatico dalle Release di GitHub.
//!
//! Controlla l'ultima versione pubblicata; se è più recente scarica il file
//! giusto per la piattaforma, ne verifica lo SHA-256 con `SHA256SUMS.txt` e:
//! - Windows: sostituisce l'eseguibile in uso (stesso percorso, così le
//!   regole del firewall restano valide) e si riavvia;
//! - Linux: installa il `.deb` con `pkexec dpkg -i` (finestra della password
//!   di sistema) e si riavvia. App Center non viene coinvolto.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO: &str = "vmatteo81/Deskflow";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub page: String,
    pub asset_name: String,
    pub asset_url: String,
    pub sums_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Downloading {
        version: String,
        done: u64,
        total: u64,
    },
    Installing(String),
    Failed(String),
}

static STATE: Mutex<State> = Mutex::new(State::Idle);

pub fn state() -> State {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set(s: State) {
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = s;
    crate::status::update(|_| {}); // ridisegna la finestra
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((
        it.next()??,
        it.next().flatten().unwrap_or(0),
        it.next().flatten().unwrap_or(0),
    ))
}

pub fn is_newer(remote: &str, current: &str) -> bool {
    matches!((parse_version(remote), parse_version(current)), (Some(r), Some(c)) if r > c)
}

/// Suffisso del file da scaricare per questa piattaforma.
fn asset_suffix() -> Option<&'static str> {
    if cfg!(windows) {
        Some("-windows-x64.exe")
    } else if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        Some("_amd64.deb")
    } else {
        None
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(120)))
        .user_agent(concat!("ponte-updater/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn fetch_latest() -> Result<Option<Release>> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let rel: GhRelease = agent()
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("impossibile contattare GitHub")?
        .body_mut()
        .read_json()?;
    if rel.draft || rel.prerelease || !is_newer(&rel.tag_name, CURRENT) {
        return Ok(None);
    }
    let suffix =
        asset_suffix().context("aggiornamento automatico non disponibile su questa piattaforma")?;
    let asset = rel
        .assets
        .iter()
        .find(|a| a.name.ends_with(suffix))
        .with_context(|| format!("la release {} non contiene il file {suffix}", rel.tag_name))?;
    let sums = rel.assets.iter().find(|a| a.name == "SHA256SUMS.txt");
    Ok(Some(Release {
        version: rel.tag_name.trim_start_matches('v').to_string(),
        page: rel.html_url,
        asset_name: asset.name.clone(),
        asset_url: asset.browser_download_url.clone(),
        sums_url: sums.map(|a| a.browser_download_url.clone()),
    }))
}

/// Controlla se c'è una nuova versione (in background). Con `manual` gli
/// errori vengono mostrati; il controllo automatico fallisce in silenzio.
pub fn check(manual: bool) {
    if matches!(
        state(),
        State::Checking | State::Downloading { .. } | State::Installing(_)
    ) {
        return;
    }
    set(State::Checking);
    std::thread::spawn(move || match fetch_latest() {
        Ok(Some(r)) => {
            log::info!("nuova versione disponibile: {}", r.version);
            set(State::Available(r));
        }
        Ok(None) => set(if manual { State::UpToDate } else { State::Idle }),
        Err(e) => {
            log::debug!("controllo aggiornamenti: {e:#}");
            set(if manual {
                State::Failed(format!("{e:#}"))
            } else {
                State::Idle
            });
        }
    });
}

/// Controllo all'avvio e poi periodico.
pub fn spawn_periodic() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(3));
        loop {
            if matches!(state(), State::Idle | State::UpToDate) {
                check(false);
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

fn expected_sha(sums_url: &str, name: &str) -> Result<String> {
    let text = agent().get(sums_url).call()?.body_mut().read_to_string()?;
    text.lines()
        .find_map(|l| {
            let mut parts = l.split_whitespace();
            let (hash, file) = (parts.next()?, parts.next()?);
            (file.trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
        })
        .with_context(|| format!("{name} non presente in SHA256SUMS.txt"))
}

fn download(rel: &Release) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("ponte-update");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(&rel.asset_name);
    let mut resp = agent()
        .get(&rel.asset_url)
        .call()
        .context("download non riuscito")?;
    let total = resp.body().content_length().unwrap_or(0);
    let mut reader = resp.body_mut().as_reader();
    let mut file = std::fs::File::create(&path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
        set(State::Downloading {
            version: rel.version.clone(),
            done,
            total,
        });
    }
    file.flush()?;
    drop(file);
    let got = hex::encode(hasher.finalize());
    match &rel.sums_url {
        Some(url) => {
            let want = expected_sha(url, &rel.asset_name)?;
            if got != want {
                let _ = std::fs::remove_file(&path);
                bail!("il file scaricato è corrotto (SHA-256 diverso): riprova");
            }
        }
        None => log::warn!("SHA256SUMS.txt assente: file non verificato"),
    }
    Ok(path)
}

/// Scarica e installa la release, poi riavvia Ponte.
pub fn install(rel: Release) {
    std::thread::spawn(move || {
        let res = download(&rel).and_then(|path| {
            set(State::Installing(rel.version.clone()));
            apply(&path)
        });
        match res {
            Ok(exe) => {
                log::info!("aggiornato alla versione {}: riavvio", rel.version);
                crate::instance::restart_with(exe);
            }
            Err(e) => {
                log::error!("aggiornamento non riuscito: {e:#}");
                set(State::Failed(format!("{e:#}")));
            }
        }
    });
}

/// Applica il file scaricato e restituisce l'eseguibile da avviare.
#[cfg(windows)]
fn apply(downloaded: &Path) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let old = old_path(&exe);
    let _ = std::fs::remove_file(&old);
    // Un .exe in esecuzione non si può sovrascrivere ma si può rinominare.
    std::fs::rename(&exe, &old).context("impossibile sostituire ponte.exe (cartella protetta?)")?;
    if let Err(e) = std::fs::copy(downloaded, &exe) {
        let _ = std::fs::rename(&old, &exe);
        return Err(e).context("impossibile scrivere il nuovo ponte.exe");
    }
    let _ = std::fs::remove_file(downloaded);
    Ok(exe)
}

#[cfg(target_os = "linux")]
fn apply(downloaded: &Path) -> Result<PathBuf> {
    const INSTALLED: &str = "/usr/bin/ponte";
    let exe = std::env::current_exe()?;
    if exe != Path::new(INSTALLED) {
        bail!(
            "Ponte non è installato dal pacchetto .deb (in uso: {}). Installa il file scaricato con:  sudo apt install {}",
            exe.display(),
            downloaded.display()
        );
    }
    let status = std::process::Command::new("pkexec")
        .arg("dpkg")
        .arg("-i")
        .arg(downloaded)
        .status()
        .with_context(|| {
            format!(
                "pkexec non disponibile: esegui  sudo apt install {}",
                downloaded.display()
            )
        })?;
    if !status.success() {
        bail!("installazione annullata o non riuscita");
    }
    let _ = std::fs::remove_file(downloaded);
    Ok(PathBuf::from(INSTALLED))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn apply(_: &Path) -> Result<PathBuf> {
    bail!("aggiornamento automatico non disponibile su questa piattaforma")
}

#[cfg(windows)]
fn old_path(exe: &Path) -> PathBuf {
    let mut name = exe.file_name().unwrap_or_default().to_os_string();
    name.push(".old");
    exe.with_file_name(name)
}

/// All'avvio: rimuove l'eseguibile precedente lasciato dall'aggiornamento.
pub fn cleanup_previous() {
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
}

/// `ponte update` da terminale: controlla e installa in modo sincrono.
pub fn update_cli() -> Result<()> {
    println!("Versione installata: {CURRENT}");
    let Some(rel) = fetch_latest()? else {
        println!("Hai già l'ultima versione.");
        return Ok(());
    };
    println!("Scarico la versione {}…", rel.version);
    let path = download(&rel)?;
    println!("Installo…");
    apply(&path)?;
    println!("Aggiornato alla versione {}. Riavvia Ponte.", rel.version);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(parse_version("v0.3.0"), Some((0, 3, 0)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("2.0.1-beta"), Some((2, 0, 1)));
        assert_eq!(parse_version("pippo"), None);
        assert!(is_newer("v0.10.0", "0.9.9"));
        assert!(is_newer("v1.0.0", "0.3.0"));
        assert!(!is_newer("v0.3.0", "0.3.0"));
        assert!(!is_newer("v0.2.9", "0.3.0"));
        assert!(!is_newer("boh", "0.3.0"));
    }
}
