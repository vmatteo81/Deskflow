<p align="center"><img src="assets/ponte-256.png" width="128" alt="Ponte"></p>

<p align="center">
  <a href="https://github.com/vmatteo81/Deskflow/actions/workflows/ci.yml"><img src="https://github.com/vmatteo81/Deskflow/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/vmatteo81/Deskflow/releases/latest"><img src="https://img.shields.io/github/v/release/vmatteo81/Deskflow?label=download" alt="Download"></a>
</p>

# Ponte

Usa **un solo mouse e una sola tastiera su due PC** (Linux e Windows, in qualsiasi combinazione)
e **copia/incolla testo in entrambe le direzioni**. Porti il cursore oltre il bordo dello schermo
e continui a lavorare sull'altro computer.

- Scritto in **Rust**, thread nativi con I/O bloccante (nessun runtime async), TCP con Nagle
  disattivato, movimenti accorpati in coda: sulla LAN la latenza aggiunta è nell'ordine dei
  decimi di millisecondo.
- **Linux**: legge direttamente da `evdev` e inietta con `uinput`, quindi funziona sia su **X11**
  che su **Wayland** (GNOME, KDE, …).
- **Windows**: hook low-level di sistema + `SendInput`, consapevole del DPI (scaling 125/150%).
- **Cifrato**: ChaCha20-Poly1305 con chiavi di sessione derivate da un segreto condiviso.
- Tasti trasmessi come **tasti fisici**: il layout (italiano, US, …) è quello del PC che riceve.

## Come si usa

C'è un **server** (il PC con mouse e tastiera collegati) e un **client** (il PC controllato).

### 1. Scarica e installa

**[⬇ Scarica l'ultima versione](https://github.com/vmatteo81/Deskflow/releases/latest)**

| Sistema | File | Installazione |
|---|---|---|
| Windows 10/11 x64 | `ponte-X.Y.Z-windows-x64.exe` | nessuna: eseguibile unico |
| Linux Debian/Ubuntu x64 | `ponte_X.Y.Z_amd64.deb` | `sudo apt install ./ponte_*_amd64.deb` |

### 2. Configura

Sul **server**:

```sh
ponte init --mode server
```

Stampa il segreto generato. Sul **client**:

```sh
ponte init --mode client --server 192.168.1.10 --secret <SEGRETO-STAMPATO-DAL-SERVER>
```

Su Windows apri un terminale nella cartella dell'eseguibile e usa `.\ponte.exe init ...`.
La configurazione sta in `~/.config/ponte/ponte.toml` (Linux) o `%APPDATA%\ponte\ponte.toml`
(Windows). `ponte config-path` mostra il percorso.

Nel file del server imposta **dove sta il client** rispetto al server:

```toml
[server]
client_side = "right"   # left | right | top | bottom
```

### 3. Permessi su Linux

- **Client Linux**: il pacchetto installa una regola udev che dà accesso a `/dev/uinput`
  all'utente della sessione. Basta uscire e rientrare (o riavviare).
- **Server Linux**: serve leggere tastiera e mouse:
  `sudo usermod -aG input $USER`, poi esci e rientra.

### 4. Avvia

```sh
ponte          # usa la `mode` del file di configurazione
```

Su Windows basta il doppio clic su `ponte.exe`. Su Linux, per l'avvio automatico con la sessione:

```sh
systemctl --user enable --now ponte
```

Porta usata: **TCP 24800** (aprila nel firewall del server, es. `sudo ufw allow 24800/tcp`).

## Passare da un PC all'altro

| Azione | Come |
|---|---|
| Andare sul client | spingi il cursore oltre il bordo `client_side`, oppure la hotkey |
| Tornare sul server | esci dal bordo opposto del client, oppure la hotkey |
| Hotkey (default) | `ScrollLock` oppure `Ctrl destro + Shift destro` |

Mentre tieni premuto un tasto del mouse (trascinamento) lo schermo non cambia.

## Appunti

Il testo copiato su un PC è subito disponibile per incollarlo sull'altro, in entrambe le direzioni
(controllo ogni 200 ms, configurabile). Si può disattivare con `[clipboard] enabled = false`.

## Configurazione completa

```toml
mode = "server"                 # "server" | "client"
secret = "…"                    # identico sui due PC (ponte init lo genera)
# name = "fisso"                # nome mostrato all'altro PC

[server]
listen = "0.0.0.0:24800"
client_side = "right"
edge_switch = true              # passaggio spingendo oltre il bordo
hotkeys = [["ScrollLock"], ["RightCtrl", "RightShift"]]
pointer_speed = 1.0             # velocità del puntatore sul client
pointer_accel = 0.6             # accelerazione (solo server Linux)
lock_while_dragging = true

[client]
server = "192.168.1.10"         # porta opzionale: "host:24800"
# screen = [3840, 1080]         # forza la risoluzione totale se rilevata male

[clipboard]
enabled = true
max_bytes = 8388608
poll_ms = 200
```

Per vedere i nomi dei tasti usabili nelle hotkey: `ponte keys`.
Per vedere quali dispositivi di input vengono usati (Linux): `ponte devices`.
Log dettagliati: `ponte -v` (debug) o `ponte -vv` (trace, include la latenza di rete).

## Limiti noti

- **Server su Wayland**: Wayland non permette a un programma di sapere dove si trova il cursore,
  quindi per **andare** sul client si usa la **hotkey**. Il **ritorno** passando dal bordo funziona
  sempre. Con un server X11 o Windows funziona anche il passaggio dal bordo.
- **Client Windows**: Windows non permette a un programma normale di comandare finestre avviate
  come amministratore (UAC). Se serve, avvia `ponte.exe` come amministratore.
- Sul client Linux il puntatore virtuale è assoluto: copre tutto il desktop. Con più monitor
  disposti in modo non orizzontale imposta `[client] screen` con la risoluzione totale.
- Touchpad del server Linux durante il controllo remoto: movimento, tap (1/2/3 dita = sinistro,
  destro, centrale), clic fisico e scorrimento a due dita. Le gesture avanzate restano locali.
- Combinazioni intercettate dal sistema (`Ctrl+Alt+Canc` su Windows) non vengono inoltrate.

## Come funziona

```
 SERVER                                             CLIENT
 evdev / hook Windows                               uinput / SendInput
        │                                                   ▲
   Engine (bordi, hotkey,                          ClientState (cursore virtuale,
   tasti a metà passaggio)                          bordo di ritorno, rilascio tasti)
        │                                                   │
   coda → writer (accorpa i movimenti, 1 syscall) ──TCP──▶ reader (inietta subito)
        ▲                                                   │
        └──────────── Leave / Clipboard / Pong ◀────────────┘
```

- **Protocollo** (`src/protocol.rs`): messaggi binari di pochi byte (`Move` = 5 byte + 20 di
  frame cifrato). I tasti viaggiano come keycode evdev, convertiti da/verso gli scancode
  Windows (`src/keys.rs`).
- **Sicurezza** (`src/crypto.rs`, `src/net.rs`): ogni lato invia un nonce casuale; HKDF-SHA256
  deriva una chiave per direzione; ogni frame è autenticato e numerato (niente replay).
  Un segreto sbagliato fa fallire la connessione subito.
- **Nessun tasto bloccato**: un tasto premuto prima del passaggio viene rilasciato dove è stato
  premuto; a ogni passaggio o disconnessione il client rilascia tutto ciò che è ancora premuto;
  se la rete cade il server torna subito locale.
- **Keepalive**: ping ogni secondo, connessione considerata morta dopo 5 s; il client si
  riconnette da solo.

## Compilare dai sorgenti

Su Ubuntu/Debian:

```sh
sudo apt install build-essential mingw-w64
curl https://sh.rustup.rs -sSf | sh
rustup target add x86_64-pc-windows-gnu
cargo install cargo-deb

make test        # test automatici
make release     # dist/ponte_X_amd64.deb + dist/ponte-X-windows-x64.exe
make logo        # rigenera PNG/ICO da assets/logo.svg
```

### Pubblicare una nuova versione

1. Aggiorna `version` in `Cargo.toml` (es. `0.2.0`) e fai commit.
2. `git tag v0.2.0 && git push origin main v0.2.0`
3. GitHub Actions compila `.deb` ed `.exe` e li pubblica nella pagina **Releases**
   (con `SHA256SUMS.txt`). A ogni push la CI esegue i test su Linux e su Windows.

## Struttura

```
src/
  main.rs            CLI (init, run, server, client, keys, devices)
  config.rs          file TOML e validazione
  protocol.rs        messaggi binari
  crypto.rs net.rs   handshake, cifratura, trasporto a bassa latenza
  keys.rs            keycode, nomi hotkey, scancode Windows
  clipboard.rs       sincronizzazione appunti senza eco
  server/engine.rs   logica di commutazione (testata senza hardware)
  client.rs          iniezione e bordo di ritorno
  platform/linux.rs  evdev, uinput, touchpad, X11
  platform/windows.rs hook low-level, SendInput, DPI
assets/              logo (SVG sorgente, PNG, ICO)
packaging/           .desktop, regola udev, servizio systemd utente, manifest Windows
```
