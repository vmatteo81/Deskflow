use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::keys;
use crate::protocol::{Side, DEFAULT_PORT};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Server,
    Client,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Ruolo usato da `ponte run` (o doppio clic sull'eseguibile).
    pub mode: Mode,
    /// Segreto condiviso: deve essere identico sui due PC.
    pub secret: String,
    /// Nome mostrato all'altro PC (default: hostname).
    #[serde(default)]
    pub name: Option<String>,
    /// Avvia Ponte all'accesso dell'utente (con la finestra ridotta).
    #[serde(default = "yes")]
    pub autostart: bool,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub client: ClientConfig,
    #[serde(default)]
    pub clipboard: ClipboardConfig,
    #[serde(default)]
    pub update: UpdateConfig,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateConfig {
    /// Controlla da solo se su GitHub c'è una versione nuova.
    pub auto_check: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self { auto_check: true }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: String,
    /// Dove si trova fisicamente il client rispetto al server.
    pub client_side: Side,
    /// Passa al client spingendo il cursore oltre il bordo.
    pub edge_switch: bool,
    /// Combinazioni che spostano il controllo avanti/indietro.
    pub hotkeys: Vec<Vec<String>>,
    /// Moltiplicatore della velocità del puntatore verso il client.
    pub pointer_speed: f64,
    /// Accelerazione applicata ai movimenti grezzi letti da evdev (Linux).
    pub pointer_accel: f64,
    /// Non cambiare schermo mentre un tasto del mouse è premuto.
    pub lock_while_dragging: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: format!("0.0.0.0:{DEFAULT_PORT}"),
            client_side: Side::Right,
            edge_switch: true,
            hotkeys: vec![
                vec!["ScrollLock".into()],
                vec!["RightCtrl".into(), "RightShift".into()],
            ],
            pointer_speed: 1.0,
            pointer_accel: 0.6,
            lock_while_dragging: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClientConfig {
    /// Indirizzo del server, es. "192.168.1.10" o "fisso.lan:24800".
    pub server: String,
    /// Risoluzione totale del desktop, se il rilevamento automatico sbaglia.
    pub screen: Option<[u32; 2]>,
    /// Nome del server abbinato: serve a ritrovarlo se cambia IP o porta.
    pub server_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClipboardConfig {
    pub enabled: bool,
    pub max_bytes: usize,
    pub poll_ms: u64,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bytes: 8 * 1024 * 1024,
            poll_ms: 200,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("impossibile leggere {}", path.display()))?;
        let cfg: Config = toml::from_str(&text)
            .with_context(|| format!("configurazione non valida in {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        if self.secret.trim().len() < 16 {
            bail!("`secret` troppo corto: usa `ponte init` per generarne uno sicuro");
        }
        self.hotkey_codes()?;
        if self.mode == Mode::Client && self.client.server.trim().is_empty() {
            bail!("modalità client: imposta [client] server = \"IP-del-server\"");
        }
        if !(0.05..=20.0).contains(&self.server.pointer_speed) {
            bail!("pointer_speed fuori intervallo (0.05..20)");
        }
        Ok(())
    }

    pub fn hotkey_codes(&self) -> Result<Vec<Vec<u16>>> {
        self.server
            .hotkeys
            .iter()
            .map(|combo| {
                if combo.is_empty() {
                    bail!("hotkey vuota");
                }
                combo
                    .iter()
                    .map(|n| {
                        keys::code_from_name(n)
                            .with_context(|| format!("tasto sconosciuto `{n}` (vedi `ponte keys`)"))
                    })
                    .collect()
            })
            .collect()
    }

    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(hostname)
    }

    pub fn server_addr(&self) -> String {
        let s = self.client.server.trim();
        if s.contains(':') && !s.ends_with(']') {
            s.to_string()
        } else {
            format!("{s}:{DEFAULT_PORT}")
        }
    }
}

/// Scrive la configurazione (leggibile solo dall'utente su Linux).
pub fn save(path: &Path, cfg: &Config) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("impossibile creare {}", dir.display()))?;
    }
    let body = toml::to_string_pretty(cfg)?;
    let text = format!(
        "# Configurazione di Ponte (modificabile anche dalla finestra Impostazioni)\n\n{body}"
    );
    std::fs::write(path, text)
        .with_context(|| format!("impossibile scrivere {}", path.display()))?;
    restrict_permissions(path);
    Ok(())
}

pub fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

pub fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "pc".into())
}

pub fn default_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ponte")
        .join("ponte.toml")
}

pub fn template(mode: Mode, secret: &str, server: &str) -> String {
    let mode = match mode {
        Mode::Server => "server",
        Mode::Client => "client",
    };
    format!(
        r#"# Configurazione di Ponte
# Copia questo file sull'altro PC cambiando solo `mode` (e `[client] server`).

mode = "{mode}"            # "server" = PC con mouse e tastiera, "client" = PC controllato
secret = "{secret}"        # IDENTICO sui due PC
# name = "fisso"

[server]
listen = "0.0.0.0:{port}"
client_side = "right"      # dov'è il client rispetto al server: left | right | top | bottom
edge_switch = true         # passa al client spingendo il cursore oltre il bordo
hotkeys = [["ScrollLock"], ["RightCtrl", "RightShift"]]   # alternano server/client
pointer_speed = 1.0
pointer_accel = 0.6        # solo per server Linux (movimenti grezzi)
lock_while_dragging = true

[client]
server = "{server}"        # IP o nome del server, porta opzionale
# screen = [1920, 1080]    # forza la risoluzione se quella rilevata è sbagliata

[clipboard]
enabled = true
max_bytes = 8388608
poll_ms = 200
"#,
        port = DEFAULT_PORT
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_and_validates() {
        let t = template(Mode::Client, &"ab".repeat(32), "192.168.1.5");
        let c: Config = toml::from_str(&t).unwrap();
        c.validate().unwrap();
        assert_eq!(c.mode, Mode::Client);
        assert_eq!(c.server.client_side, Side::Right);
        assert_eq!(c.server_addr(), "192.168.1.5:24800");
        assert_eq!(c.hotkey_codes().unwrap(), vec![vec![70], vec![97, 54]]);
    }

    #[test]
    fn minimal_config_uses_defaults() {
        let c: Config =
            toml::from_str("mode = \"server\"\nsecret = \"0123456789abcdef0123\"").unwrap();
        c.validate().unwrap();
        assert!(c.clipboard.enabled);
        assert!(c.server.edge_switch);
    }

    #[test]
    fn save_and_reload() {
        let dir = std::env::temp_dir().join(format!("ponte-test-{}", std::process::id()));
        let path = dir.join("ponte.toml");
        let mut c: Config = toml::from_str(&template(Mode::Server, &"cd".repeat(32), "")).unwrap();
        c.server.client_side = Side::Top;
        c.name = Some("fisso".into());
        save(&path, &c).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(back.server.client_side, Side::Top);
        assert_eq!(back.name.as_deref(), Some("fisso"));
        assert_eq!(back.secret, c.secret);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bad_values_are_rejected() {
        let short: Config = toml::from_str("mode = \"server\"\nsecret = \"x\"").unwrap();
        assert!(short.validate().is_err());
        let mut c: Config =
            toml::from_str("mode = \"client\"\nsecret = \"0123456789abcdef0123\"").unwrap();
        assert!(c.validate().is_err(), "client senza server");
        c.client.server = "10.0.0.1:9000".into();
        c.validate().unwrap();
        assert_eq!(c.server_addr(), "10.0.0.1:9000");
        c.server.hotkeys = vec![vec!["Pippo".into()]];
        assert!(c.validate().is_err());
    }
}
