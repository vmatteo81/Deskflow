//! Firewall: verifica se le connessioni dalla rete locale possono arrivare al
//! server e, su richiesta, le autorizza (una volta, con i permessi di
//! amministratore). Le regole accettano solo la rete locale.
//!
//! - Windows: regola "Ponte (LAN)" sul programma, `remoteip=localsubnet`,
//!   valida per tutti i profili (anche reti marcate come "Pubbliche"). Prima
//!   rimuove le regole di blocco create da un "Annulla" sull'avviso di sistema.
//! - Linux: solo se `ufw` è attivo, apre le porte alle sottoreti locali.

use std::sync::Mutex;

use anyhow::Result;

/// Porte TCP che il server può usare (vedi `server::run`) e porta UDP della ricerca.
pub const TCP_PORTS: (u16, u16) = (24800, 24810);
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const UDP_PORT: u16 = crate::discovery::PORT;

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Unknown,
    /// Nessun firewall attivo o regola già presente.
    Ok,
    /// Serve l'autorizzazione.
    Missing,
    Applying,
    Failed(String),
}

static STATUS: Mutex<Status> = Mutex::new(Status::Unknown);

pub fn status() -> Status {
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set(s: Status) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = s;
    crate::status::update(|_| {});
}

/// Aggiorna lo stato in background.
pub fn refresh() {
    if status() == Status::Applying {
        return;
    }
    std::thread::spawn(|| {
        set(if is_allowed() {
            Status::Ok
        } else {
            Status::Missing
        })
    });
}

/// Chiede l'autorizzazione e crea le regole (in background).
pub fn allow_lan() {
    set(Status::Applying);
    std::thread::spawn(|| match apply() {
        Ok(()) => {
            log::info!("firewall: connessioni dalla rete locale consentite");
            set(if is_allowed() {
                Status::Ok
            } else {
                Status::Missing
            });
        }
        Err(e) => {
            log::warn!("firewall: {e:#}");
            set(Status::Failed(format!("{e:#}")));
        }
    });
}

// ------------------------------------------------------------------ Windows

#[cfg(windows)]
const RULE: &str = "Ponte (LAN)";

#[cfg(windows)]
fn hidden(cmd: &mut std::process::Command) -> &mut std::process::Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(windows)]
fn is_allowed() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let out = hidden(std::process::Command::new("netsh").args([
        "advfirewall",
        "firewall",
        "show",
        "rule",
        &format!("name={RULE}"),
        "verbose",
    ]))
    .output();
    match out {
        // L'output è localizzato: basta controllare che la regola punti a questo eseguibile.
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .to_lowercase()
            .contains(&exe.display().to_string().to_lowercase()),
        _ => false,
    }
}

#[cfg(windows)]
fn apply() -> Result<()> {
    use anyhow::bail;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE;

    let exe = std::env::current_exe()?.display().to_string();
    // cmd /c "a & b": cmd toglie solo le virgolette esterne.
    let params = format!(
        "/c \"netsh advfirewall firewall delete rule name=all program=\"{exe}\" & \
         netsh advfirewall firewall add rule name=\"{RULE}\" dir=in action=allow program=\"{exe}\" \
         enable=yes profile=any remoteip=localsubnet\""
    );
    let wide = |s: &str| {
        s.encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>()
    };
    let (verb, file, params) = (wide("runas"), wide("cmd.exe"), wide(&params));
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = SW_HIDE;
    // SAFETY: le stringhe restano vive fino al ritorno; l'handle viene chiuso.
    unsafe {
        if ShellExecuteExW(&mut info) == 0 {
            bail!("autorizzazione di amministratore negata");
        }
        if info.hProcess.is_null() {
            bail!("impossibile avviare netsh");
        }
        WaitForSingleObject(info.hProcess, INFINITE);
        let mut code = 1u32;
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
        if code != 0 {
            bail!("netsh ha restituito l'errore {code}");
        }
    }
    Ok(())
}

// -------------------------------------------------------------------- Linux

#[cfg(target_os = "linux")]
fn ufw_enabled() -> bool {
    std::fs::read_to_string("/etc/ufw/ufw.conf")
        .map(|s| {
            s.lines()
                .any(|l| l.trim().eq_ignore_ascii_case("ENABLED=yes"))
        })
        .unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn marker() -> std::path::PathBuf {
    crate::config::default_path().with_file_name("ufw-lan-ok")
}

#[cfg(target_os = "linux")]
fn is_allowed() -> bool {
    // Le regole di ufw sono leggibili solo da root: ricordiamo di averle create.
    !ufw_enabled() || marker().exists()
}

#[cfg(target_os = "linux")]
fn apply() -> Result<()> {
    use anyhow::{bail, Context};
    let nets = crate::discovery::local_subnets();
    if nets.is_empty() {
        bail!("nessuna rete locale rilevata");
    }
    let mut script = String::from("set -e\n");
    for n in &nets {
        script.push_str(&format!(
            "ufw allow from {n} to any port {}:{} proto tcp comment Ponte\n\
             ufw allow from {n} to any port {UDP_PORT} proto udp comment Ponte\n",
            TCP_PORTS.0, TCP_PORTS.1
        ));
    }
    let status = std::process::Command::new("pkexec")
        .args(["sh", "-c", &script])
        .status()
        .context("pkexec non disponibile")?;
    if !status.success() {
        bail!("autorizzazione negata o ufw non riuscito");
    }
    if let Some(dir) = marker().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(marker(), nets.join("\n"))?;
    Ok(())
}

#[cfg(not(any(windows, target_os = "linux")))]
fn is_allowed() -> bool {
    true
}

#[cfg(not(any(windows, target_os = "linux")))]
fn apply() -> Result<()> {
    Ok(())
}
