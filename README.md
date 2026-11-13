<p align="center"><img src="assets/ponte-256.png" width="128" alt="Ponte"></p>

<p align="center">
  <a href="https://github.com/vmatteo81/Deskflow/actions/workflows/ci.yml"><img src="https://github.com/vmatteo81/Deskflow/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="https://github.com/vmatteo81/Deskflow/releases/latest"><img src="https://img.shields.io/github/v/release/vmatteo81/Deskflow?label=download" alt="Download"></a>
</p>

# Ponte

Usa **un solo mouse e una sola tastiera su due PC** (Linux e Windows, in qualsiasi combinazione)
e **copia/incolla testo e immagini (screenshot) in entrambe le direzioni**. Porti il cursore oltre il bordo dello schermo
e continui a lavorare sull'altro computer.

- Scritto in **Rust**, thread nativi con I/O bloccante (nessun runtime async), TCP con Nagle
  disattivato, movimenti accorpati in coda: sulla LAN la latenza aggiunta è nell'ordine dei
  decimi di millisecondo.
- **Linux**: legge direttamente da `evdev` e inietta con `uinput`, quindi funziona sia su **X11**
  che su **Wayland** (GNOME, KDE, …).
- **Windows**: hook low-level di sistema + `SendInput`, consapevole del DPI (scaling 125/150%).
- **Cifrato**: ChaCha20-Poly1305 con chiavi di sessione derivate da un segreto condiviso.
- **Comanda la tastiera di origine**: il carattere che scrivi è quello del layout del PC con la
  tastiera (italiano, US, …), anche se l'altro PC ha un layout diverso. Scorciatoie (Ctrl+C,
  Alt+Tab, Win+E…), frecce e tasti funzione viaggiano come tasti fisici.

## Come si usa

C'è un **server** (il PC con mouse e tastiera collegati) e un **client** (il PC controllato).

### 1. Scarica e installa

**[⬇ Scarica l'ultima versione](https://github.com/vmatteo81/Deskflow/releases/latest)**

| Sistema | File | Installazione |
|---|---|---|
| Windows 10/11 x64 | `ponte-X.Y.Z-windows-x64.exe` | nessuna: eseguibile unico (mettilo in una cartella fissa) |
| Linux Debian/Ubuntu x64 | `ponte_X.Y.Z_amd64.deb` | `sudo apt install ./ponte_*_amd64.deb` |

Su Ubuntu il doppio clic sul `.deb` apre App Center, che installa la prima volta ma **non sa
aggiornare** i pacchetti scaricati a mano: usa il comando `apt` qui sopra.

**Aggiornamenti automatici:** dalla 0.4.0 Ponte controlla da solo le nuove versioni e mostra
**Aggiorna ora**. Su Windows sostituisce l'eseguibile e si riavvia; su Linux installa il `.deb`
chiedendo la password nella finestra di sistema. Il file scaricato è verificato con lo SHA-256
pubblicato nella Release. Da terminale: `ponte update`. Per disattivare il controllo:
`[update] auto_check = false`.

### 2. Configura (con la finestra)

Avvia Ponte (doppio clic su `ponte.exe`, oppure **Ponte** dal menu applicazioni su Linux).
Al primo avvio si apre la configurazione guidata:

1. **Sul PC con mouse e tastiera** scegli **Server**, clicca dove si trova l'altro PC
   (destra, sinistra, sopra, sotto) e premi **Salva e avvia**: compare un **codice di 6 cifre**.
2. **Sull'altro PC** scegli **Client**, premi **Cerca in rete**, seleziona il server
   (nome e IP) e digita le **6 cifre**. Premi **Abbina e avvia**.
3. Fatto: i due PC si scambiano da soli il segreto lungo che cifra il collegamento.
   La finestra mostra stato, nome e IP dei PC, latenza e registro attività;
   da **Impostazioni** si cambia tutto in seguito.

L'abbinamento usa SPAKE2: il codice non viaggia mai in rete e chi intercetta il traffico non può
indovinarlo provando le combinazioni. Il codice scade dopo 10 minuti e cambia dopo l'uso o dopo
3 tentativi sbagliati.

La ricerca in rete usa la porta **UDP 24801**; il collegamento vero la **TCP 24800**.

### 2-bis. Configura da terminale (in alternativa)

Sul **server**:

```sh
ponte init --mode server
ponte run        # nel log compare: «codice di abbinamento: 482 913»
```

Sul **client**:

```sh
ponte pair --server 192.168.1.10 --code 482913
ponte run
```

In alternativa si può copiare a mano il segreto lungo:
`ponte init --mode client --server 192.168.1.10 --secret <SEGRETO>`.

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
ponte          # apre la finestra (configurazione guidata al primo avvio)
ponte run      # senza finestra, secondo la `mode` del file di configurazione
```

**Avvio automatico:** di default Ponte parte all'accesso con la finestra ridotta a icona
(casella «Avvio automatico» nella finestra; Windows: chiave `Run` dell'utente, Linux:
`~/.config/autostart/ponte.desktop`). Se Ponte è già aperto, lanciarlo di nuovo mostra la finestra
esistente invece di avviare un secondo motore.

In alternativa su Linux, senza finestra, c'è il servizio utente (in quel caso disattiva la casella):

```sh
systemctl --user enable --now ponte
```

### Porte e firewall

- **TCP 24800** per il collegamento, **UDP 24801** per la ricerca in rete.
- Se la 24800 è occupata (spesso da Synergy, Barrier o Deskflow, che usano la stessa porta) il
  server passa da solo alla prima libera fino alla **24810**, e la ricerca in rete annuncia quella
  giusta. Il client, se non trova più il server, lo ricerca in rete per IP o per nome e si ricollega.
- **Firewall:** se serve, la finestra del server mostra **Consenti in rete locale**. Con un solo
  permesso di amministratore crea la regola che accetta connessioni **solo dalla rete locale**:
  - Windows: regola «Ponte (LAN)» sul programma, valida anche se la Wi-Fi è marcata come rete
    pubblica, e rimozione delle regole di blocco create premendo «Annulla» sull'avviso di sistema;
  - Linux: solo se `ufw` è attivo (su Ubuntu è spento di default), apre le porte alla sottorete locale.
- Il client non ha bisogno di regole: è lui ad aprire la connessione.

## Passare da un PC all'altro

| Azione | Come |
|---|---|
| Andare sul client | spingi il cursore oltre il bordo `client_side`, oppure la hotkey |
| Tornare sul server | esci dal bordo opposto del client, oppure la hotkey |
| Hotkey (default) | `ScrollLock` oppure `Ctrl destro + Shift destro` |

Mentre tieni premuto un tasto del mouse (trascinamento) lo schermo non cambia.

La ripetizione dei tasti tenuti premuti la decide il PC con la tastiera: sul client ogni tasto
(esclusi Shift, Ctrl, Alt, Win) viene premuto e rilasciato subito. Così un rilascio arrivato in
ritardo, per esempio su Wi-Fi, non fa più ripetere all'infinito una lettera.

## Appunti

Il testo copiato su un PC è subito disponibile per incollarlo sull'altro, in entrambe le direzioni
(controllo ogni 200 ms, configurabile). Anche le **immagini** (es. uno screenshot con Stamp o
Win+Shift+S) passano all'altro PC, come PNG fino a `max_bytes`. Si può disattivare tutto con
`[clipboard] enabled = false`.

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
  main.rs            CLI (gui, init, run, server, client, keys, devices)
  gui.rs             finestra: configurazione guidata e stato (egui)
  status.rs          stato condiviso motore → finestra, registro attività
  discovery.rs       ricerca automatica del server in LAN (UDP), IP locali
  pairing.rs         abbinamento con codice di 6 cifre (SPAKE2)
  update.rs          aggiornamenti automatici dalle Release GitHub
  firewall.rs        regole firewall per la rete locale (Windows, ufw)
  config.rs          file TOML e validazione
  protocol.rs        messaggi binari
  crypto.rs net.rs   handshake, cifratura, trasporto a bassa latenza
  keys.rs            keycode, nomi hotkey, scancode Windows
  clipboard.rs       sincronizzazione appunti (testo e immagini) senza eco
  autostart.rs       avvio automatico all'accesso
  instance.rs        istanza unica (rilanciare mostra la finestra esistente)
  server/engine.rs   logica di commutazione (testata senza hardware)
  client.rs          iniezione e bordo di ritorno
  platform/linux.rs  evdev, uinput, touchpad, X11
  platform/xkb.rs    layout di tastiera Linux (libxkbcommon)
  platform/windows.rs hook low-level, SendInput, DPI
assets/              logo (SVG sorgente, PNG, ICO)
packaging/           .desktop, regola udev, servizio systemd utente, manifest Windows
```
