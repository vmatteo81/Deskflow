//! Avvio automatico all'accesso dell'utente, con la finestra ridotta.
//! Windows: chiave `HKCU\…\Run`. Linux: `~/.config/autostart/ponte.desktop`
//! (standard XDG, vale per GNOME, KDE, XFCE…).

use anyhow::{Context, Result};

/// Argomenti con cui viene avviato Ponte all'accesso.
pub const ARGS: &str = "gui --minimized";

fn exe() -> Result<std::path::PathBuf> {
    std::env::current_exe().context("percorso dell'eseguibile sconosciuto")
}

/// Attiva o disattiva l'avvio automatico (idempotente).
pub fn set(enabled: bool) -> Result<()> {
    imp::set(enabled)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use std::path::PathBuf;

    fn path() -> Option<PathBuf> {
        Some(dirs::config_dir()?.join("autostart").join("ponte.desktop"))
    }

    pub fn entry(exe: &str) -> String {
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Ponte\n\
             Comment=Mouse, tastiera e appunti condivisi\n\
             Exec=\"{exe}\" {ARGS}\n\
             Icon=ponte\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n\
             X-GNOME-Autostart-Delay=2\n"
        )
    }

    pub fn set(enabled: bool) -> Result<()> {
        let path = path().context("cartella di configurazione sconosciuta")?;
        if !enabled {
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("impossibile rimuovere {}", path.display()))?;
            }
            return Ok(());
        }
        let exe = exe()?;
        let text = entry(&exe.to_string_lossy());
        if std::fs::read_to_string(&path).ok().as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, text)
            .with_context(|| format!("impossibile scrivere {}", path.display()))?;
        log::info!("avvio automatico attivato ({})", path.display());
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };

    const KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "Ponte";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn current() -> Option<String> {
        let (key, name) = (wide(KEY), wide(NAME));
        let mut buf = vec![0u16; 2048];
        let mut len = (buf.len() * 2) as u32;
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr() as *mut _,
                &mut len,
            )
        };
        if rc != 0 {
            return None;
        }
        let n = (len as usize / 2).saturating_sub(1);
        Some(String::from_utf16_lossy(&buf[..n]))
    }

    pub fn set(enabled: bool) -> Result<()> {
        let (key, name) = (wide(KEY), wide(NAME));
        if !enabled {
            if current().is_some() {
                unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), name.as_ptr()) };
            }
            return Ok(());
        }
        let cmd = format!("\"{}\" {ARGS}", exe()?.display());
        if current().as_deref() == Some(cmd.as_str()) {
            return Ok(());
        }
        let data = wide(&cmd);
        let rc = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                key.as_ptr(),
                name.as_ptr(),
                REG_SZ,
                data.as_ptr() as *const _,
                (data.len() * 2) as u32,
            )
        };
        if rc != 0 {
            anyhow::bail!("impossibile scrivere la chiave Run del registro (errore {rc})");
        }
        log::info!("avvio automatico attivato");
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use super::*;
    pub fn set(_enabled: bool) -> Result<()> {
        let _ = exe;
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    #[test]
    fn desktop_entry_quotes_the_path() {
        let e = super::imp::entry("/opt/Ponte App/ponte");
        assert!(e.contains("Exec=\"/opt/Ponte App/ponte\" gui --minimized\n"));
        assert!(e.starts_with("[Desktop Entry]\n"));
    }
}
