//! Ricerca automatica del server sulla rete locale (UDP broadcast).
//! Il server risponde solo con nome e porta: nessun dato sensibile, e la
//! connessione vera resta comunque protetta dal segreto.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

pub const PORT: u16 = 24801;
const ASK: &[u8] = b"PONTE?1";
const ANSWER: &str = "PONTE!1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub name: String,
    /// "ip:porta" pronto per la configurazione del client.
    pub addr: String,
}

/// Avvia il risponditore sul server.
pub fn spawn_responder(name: String, tcp_port: u16) {
    let sock = match UdpSocket::bind((Ipv4Addr::UNSPECIFIED, PORT)) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("ricerca automatica non disponibile (porta UDP {PORT}: {e})");
            return;
        }
    };
    let reply = format!("{ANSWER}\n{name}\n{tcp_port}");
    let _ = std::thread::Builder::new()
        .name("ponte-discovery".into())
        .spawn(move || {
            let mut buf = [0u8; 64];
            loop {
                match sock.recv_from(&mut buf) {
                    Ok((n, from)) if &buf[..n] == ASK => {
                        let _ = sock.send_to(reply.as_bytes(), from);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        log::debug!("discovery: {e}");
                        std::thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        });
}

fn parse(data: &[u8], from: SocketAddr) -> Option<Found> {
    let text = std::str::from_utf8(data).ok()?;
    let mut it = text.lines();
    if it.next()? != ANSWER {
        return None;
    }
    let name = it.next()?.to_string();
    let port: u16 = it.next()?.parse().ok()?;
    Some(Found {
        name,
        addr: format!("{}:{port}", from.ip()),
    })
}

/// Cerca i server per `timeout` e restituisce quelli trovati.
pub fn scan(timeout: Duration) -> Vec<Found> {
    let mut found = Vec::new();
    let Ok(sock) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
        return found;
    };
    let _ = sock.set_broadcast(true);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(100)));
    let targets = [Ipv4Addr::BROADCAST, Ipv4Addr::LOCALHOST];
    let deadline = Instant::now() + timeout;
    let mut next_send = Instant::now();
    let mut buf = [0u8; 512];
    while Instant::now() < deadline {
        if Instant::now() >= next_send {
            for t in targets {
                let _ = sock.send_to(ASK, (t, PORT));
            }
            next_send = Instant::now() + Duration::from_millis(400);
        }
        if let Ok((n, from)) = sock.recv_from(&mut buf) {
            if let Some(f) = parse(&buf[..n], from) {
                if !found.contains(&f) {
                    found.push(f);
                }
            }
        }
    }
    found
}

/// IP della route predefinita (nessun pacchetto viene inviato).
fn default_route_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(8, 8, 8, 8), 53)).ok()?;
    match s.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(ip) if !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// Interfacce virtuali (Docker, macchine virtuali, WSL) che non servono in LAN.
fn is_virtual(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "docker",
        "br-",
        "veth",
        "virbr",
        "vboxnet",
        "vmnet",
        "vethernet",
        "virtualbox",
        "vmware",
        "hyper-v",
        "wsl",
    ]
    .iter()
    .any(|p| n.starts_with(p) || n.contains(p))
}

/// Tutti gli IPv4 utili di questo PC, prima quello della route predefinita.
pub fn local_ips() -> Vec<String> {
    let primary = default_route_ip();
    let mut ips: Vec<Ipv4Addr> = primary.into_iter().collect();
    if let Ok(list) = if_addrs::get_if_addrs() {
        for itf in list {
            if itf.is_loopback()
                || itf.is_link_local()
                || !itf.is_oper_up()
                || is_virtual(&itf.name)
            {
                continue;
            }
            if let if_addrs::IfAddr::V4(v4) = itf.addr {
                if !ips.contains(&v4.ip) {
                    ips.push(v4.ip);
                }
            }
        }
    }
    ips.iter().map(|ip| ip.to_string()).collect()
}

/// IP principale con cui questo PC è raggiungibile in LAN.
pub fn local_ip() -> Option<String> {
    local_ips().into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_answers() {
        let from: SocketAddr = "192.168.1.7:24801".parse().unwrap();
        assert_eq!(
            parse(b"PONTE!1\nfisso\n24800", from),
            Some(Found {
                name: "fisso".into(),
                addr: "192.168.1.7:24800".into()
            })
        );
        assert_eq!(parse(b"ALTRO\nx\n1", from), None);
        assert_eq!(parse(b"PONTE!1\nx\nporta", from), None);
    }

    #[test]
    fn local_ips_are_usable() {
        for ip in local_ips() {
            let ip: Ipv4Addr = ip.parse().unwrap();
            assert!(!ip.is_loopback() && !ip.is_unspecified());
        }
        assert!(is_virtual("docker0") && is_virtual("vEthernet (WSL)") && !is_virtual("wlo1"));
    }

    #[test]
    fn finds_local_server() {
        spawn_responder("prova".into(), 4321);
        let found = scan(Duration::from_millis(900));
        assert!(
            found
                .iter()
                .any(|f| f.name == "prova" && f.addr.ends_with(":4321")),
            "trovati: {found:?}"
        );
    }
}
