// Copyright (c) 2026 - Matus Hubinsky

// Licensed under the EUPL-1.2-only
// You may not use this work except in compliance with the Licence.
// You may obtain a copy of the Licence at:

// https://joinup.ec.europa.eu/software/page/eupl

// Unless required by applicable law or agreed to in writing, software
// distributed under the Licence is distributed on an "AS IS" basis,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the Licence for the specific language governing permissions and
// limitations under the Licence.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::commands::Vendor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Serial,
    Ssh,
    Telnet,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_TIMEOUT: Duration = Duration::from_millis(50);
const TERM_TYPE: &str = "xterm";

impl Protocol {
    /// For serial this is the baud rate, for ssh/telnet the TCP port.
    pub fn default_port(&self) -> u16 {
        match self {
            Protocol::Serial => 9600,
            Protocol::Ssh => 22,
            Protocol::Telnet => 23,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Protocol::Serial => "serial",
            Protocol::Ssh => "ssh",
            Protocol::Telnet => "telnet",
        }
    }
}

/// Where a session connects to. This is what the connection dialog edits and
/// what gets saved into the `devices` table.
#[derive(Debug, Clone)]
pub enum Endpoint {
    Serial {
        path: String,
        baud: u32,
    },
    Remote {
        protocol: Protocol,
        host: String,
        port: u16,
        username: String,
        password: String,
        vendor: Vendor,
    },
}

impl Endpoint {
    /// Reachability check before connecting. Serial: try opening the port.
    /// Remote: resolve + TCP-connect to the service port (a real ICMP ping
    /// needs a raw socket / root, and for ssh/telnet the port is what matters).
    pub fn check(&self) -> io::Result<()> {
        match self {
            Endpoint::Serial { path, baud } => {
                open_console(path, *baud)?;
                Ok(())
            }
            Endpoint::Remote { host, port, .. } => check_reachable(host, *port),
        }
    }
}

/// Where the terminal session is attached.
pub enum Transport {
    Local,                                          // cisco::terminal() simulator
    Serial(Box<dyn serialport::SerialPort + Send>), // console cable
    Remote(RemoteSession),                          // live ssh / telnet session
}

/// Open a serial console port: 8N1, no flow control (Cisco console defaults).
pub fn open_console(path: &str, baud: u32) -> io::Result<Box<dyn serialport::SerialPort + Send>> {
    Ok(serialport::new(path, baud)
        .data_bits(serialport::DataBits::Eight)
        .parity(serialport::Parity::None)
        .stop_bits(serialport::StopBits::One)
        .flow_control(serialport::FlowControl::None)
        .timeout(Duration::from_millis(100))
        .open()?)
}

/// TCP "ping" of the service port: DNS resolve + connect with timeout.
/// A hostname may resolve to several addresses (e.g. ::1 before 127.0.0.1),
/// so try them all in order, like OpenSSH/PuTTY do.
pub fn check_reachable(host: &str, port: u16) -> io::Result<()> {
    let addrs: Vec<_> = (host, port).to_socket_addrs()?.collect();
    if addrs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("cannot resolve host '{host}'"),
        ));
    }
    let mut last_err = None;
    for addr in &addrs {
        match TcpStream::connect_timeout(addr, CONNECT_TIMEOUT) {
            Ok(_) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other(format!("cannot connect to {host}:{port}"))))
}

/// `host` or `host:port` (hostnames/IPv4 only — no IPv6 literals yet).
fn parse_host_port(rest: &str, default_port: u16) -> Option<(String, u16)> {
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    match rest.rsplit_once(':') {
        Some((h, p)) => match p.parse() {
            Ok(port) if !h.is_empty() => Some((h.to_string(), port)),
            _ => Some((rest.to_string(), default_port)),
        },
        None => Some((rest.to_string(), default_port)),
    }
}

/// Parse an in-terminal `ssh [user@]host[:port]` / `telnet [user@]host[:port]`
/// command into an endpoint. The SSH password comes from BEHE_PASSWORD when
/// not given interactively (the connection dialog passes its own field).
pub fn parse_remote_command(input: &str) -> Option<Endpoint> {
    let (cmd, rest) = input.split_once(' ')?;
    let protocol = match cmd {
        "ssh" => Protocol::Ssh,
        "telnet" => Protocol::Telnet,
        _ => return None,
    };
    let (userhost, port) = parse_host_port(rest, protocol.default_port())?;
    let (username, host) = match userhost.split_once('@') {
        Some((u, h)) if !h.is_empty() => (u.to_string(), h.to_string()),
        _ => (String::new(), userhost),
    };
    let password = std::env::var("BEHE_PASSWORD").unwrap_or_default();
    Some(Endpoint::Remote {
        protocol,
        host,
        port,
        username,
        password,
        vendor: Vendor::Cisco,
    })
}

/// Callback fired by the session thread whenever new output or a status
/// change is available.
pub type Notify = Arc<dyn Fn() + Send + Sync>;

/// Connect to a device described by `endpoint`, checking reachability first so
/// the UI fails fast with a clear message instead of hanging in the connect
/// handshake. Remote endpoints return a live session (PuTTY-style: the reader
/// thread streams device output, `send()` forwards keystrokes/lines).
pub fn connect(
    endpoint: &Endpoint,
    notify: Option<Notify>,
    size: (u16, u16),
) -> io::Result<Transport> {
    endpoint
        .check()
        .map_err(|e| io::Error::new(e.kind(), format!("{endpoint:?} unreachable: {e}")))?;
    match endpoint {
        Endpoint::Serial { .. } => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "serial sessions open via open_console()",
        )),
        Endpoint::Remote {
            protocol,
            host,
            port,
            username,
            password,
            ..
        } => {
            let notify = notify.unwrap_or_else(|| Arc::new(|| {}));
            Ok(Transport::Remote(RemoteSession::spawn(
                *protocol,
                host.clone(),
                *port,
                username.clone(),
                password.clone(),
                size.0,
                size.1,
                notify,
            )))
        }
    }
}

pub fn diagnose(host: &str, port: u16) -> String {
    match check_reachable(host, port) {
        Ok(()) => format!("% {host}:{port} reachable (tcp connect ok)"),
        Err(e) => format!("% {host}:{port} unreachable: {e}"),
    }
}

/// Terminal commands handled by networking instead of the cisco simulator.
/// Returns the reply to print, or None if `input` is not one of them.
/// (`ssh`/`telnet` are parsed by `parse_remote_command` so the session can be
/// attached to the UI.)
pub fn handle_command(input: &str) -> Option<String> {
    let (cmd, rest) = input.split_once(' ')?;
    match cmd {
        "diag" => {
            let (host, port) = parse_host_port(rest, 0)?;
            Some(diagnose(&host, port))
        }
        "console" => {
            let mut parts = rest.split_whitespace();
            let path = parts.next()?;
            let baud = parts.next().and_then(|b| b.parse().ok()).unwrap_or(9600);
            let ep = Endpoint::Serial {
                path: path.to_string(),
                baud,
            };
            Some(match ep.check() {
                Ok(()) => format!("% console {path} @ {baud} baud opened (session plumbing TODO)"),
                Err(e) => format!("% cannot open console {path}: {e}"),
            })
        }
        _ => None,
    }
}

// ----------------------------------------------------------------------
// Live remote session (ssh / telnet), PuTTY-style: a background thread owns
// the connection, renders device output into a scrollback-friendly string,
// and `send()` forwards input to the device.
// ----------------------------------------------------------------------

struct SessionInner {
    protocol: Protocol,
    host: String,
    port: u16,
    username: String,
    out: Mutex<String>,
    status: Mutex<String>,
    tx: Mutex<Option<Sender<Vec<u8>>>>,
    closed: AtomicBool,
}

#[derive(Clone)]
pub struct RemoteSession {
    inner: Arc<SessionInner>,
    handle: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl RemoteSession {
    pub fn spawn(
        protocol: Protocol,
        host: String,
        port: u16,
        username: String,
        password: String,
        cols: u16,
        rows: u16,
        notify: Notify,
    ) -> RemoteSession {
        let inner = Arc::new(SessionInner {
            protocol,
            host: host.clone(),
            port,
            username: username.clone(),
            out: Mutex::new(String::new()),
            status: Mutex::new("connecting...".to_string()),
            tx: Mutex::new(None),
            closed: AtomicBool::new(false),
        });
        let (tx, rx) = channel::<Vec<u8>>();
        *inner.tx.lock().unwrap() = Some(tx);
        let inner2 = Arc::clone(&inner);
        let handle = thread::spawn(move || match protocol {
            Protocol::Telnet => run_telnet(&host, port, cols, rows, inner2, rx, notify),
            Protocol::Ssh => run_ssh(
                &host, port, &username, &password, cols, rows, inner2, rx, notify,
            ),
            Protocol::Serial => unreachable!(),
        });
        RemoteSession {
            inner,
            handle: Arc::new(Mutex::new(Some(handle))),
        }
    }

    /// Forward bytes to the device (a submitted line should end with "\r").
    pub fn send(&self, data: &[u8]) {
        if let Some(tx) = self.inner.tx.lock().unwrap().as_ref() {
            let _ = tx.send(data.to_vec());
        }
    }

    /// Drain the sanitized device output produced since the last call.
    pub fn poll_output(&self) -> String {
        std::mem::take(&mut *self.inner.out.lock().unwrap())
    }

    pub fn status(&self) -> String {
        self.inner.status.lock().unwrap().clone()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    pub fn describe(&self) -> String {
        let user = if self.inner.username.is_empty() {
            String::new()
        } else {
            format!("{}@", self.inner.username)
        };
        format!(
            "{} {}{}:{}",
            self.inner.protocol.label(),
            user,
            self.inner.host,
            self.inner.port
        )
    }

    pub fn shutdown(&self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        self.inner.tx.lock().unwrap().take();
        if let Some(handle) = self.handle.lock().unwrap().take() {
            let _ = handle.join();
        }
    }
}

fn set_status(inner: &SessionInner, msg: impl Into<String>, notify: &Notify) {
    *inner.status.lock().unwrap() = msg.into();
    notify();
}

fn finish(inner: &SessionInner, notify: Notify) {
    inner.closed.store(true, Ordering::SeqCst);
    notify();
}

fn push_output(inner: &SessionInner, sanitizer: &mut AnsiSanitizer, bytes: &[u8], notify: &Notify) {
    let text = sanitizer.feed(bytes);
    if !text.is_empty() {
        inner.out.lock().unwrap().push_str(&text);
        notify();
    }
}

// ----------------------------------------------------------------------
// Telnet (RFC 854): option negotiation + IAC stripping, like PuTTY telnet
// ----------------------------------------------------------------------

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const SE: u8 = 240;

const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;
const OPT_TTYPE: u8 = 24;
const OPT_NAWS: u8 = 31;

const TTYPE_IS: u8 = 0;
const TTYPE_SEND: u8 = 1;

fn naws_bytes(cols: u16, rows: u16) -> Vec<u8> {
    vec![
        IAC,
        SB,
        OPT_NAWS,
        (cols >> 8) as u8,
        (cols & 0xff) as u8,
        (rows >> 8) as u8,
        (rows & 0xff) as u8,
        IAC,
        SE,
    ]
}

enum FilterState {
    Data,
    Iac,
    Command(u8),
    Subneg,
    SubnegIac,
}

pub struct TelnetFilter {
    state: FilterState,
    subneg: Vec<u8>,
}

impl TelnetFilter {
    pub fn new() -> Self {
        TelnetFilter {
            state: FilterState::Data,
            subneg: Vec::new(),
        }
    }

    /// Split incoming bytes into terminal data and negotiation replies.
    pub fn feed(&mut self, input: &[u8], cols: u16, rows: u16) -> (Vec<u8>, Vec<u8>) {
        let mut data = Vec::new();
        let mut resp = Vec::new();
        for &b in input {
            match self.state {
                FilterState::Data => {
                    if b == IAC {
                        self.state = FilterState::Iac;
                    } else {
                        data.push(b);
                    }
                }
                FilterState::Iac => match b {
                    IAC => {
                        data.push(IAC);
                        self.state = FilterState::Data;
                    }
                    WILL | WONT | DO | DONT => self.state = FilterState::Command(b),
                    SB => {
                        self.subneg.clear();
                        self.state = FilterState::Subneg;
                    }
                    _ => self.state = FilterState::Data,
                },
                FilterState::Command(cmd) => {
                    match cmd {
                        WILL => {
                            if b == OPT_ECHO || b == OPT_SGA {
                                resp.extend([IAC, DO, b]);
                            } else {
                                resp.extend([IAC, DONT, b]);
                            }
                        }
                        WONT => resp.extend([IAC, DONT, b]),
                        DO => {
                            if b == OPT_SGA || b == OPT_NAWS || b == OPT_TTYPE {
                                resp.extend([IAC, WILL, b]);
                                if b == OPT_NAWS {
                                    resp.extend(naws_bytes(cols, rows));
                                }
                            } else {
                                resp.extend([IAC, WONT, b]);
                            }
                        }
                        DONT => resp.extend([IAC, WONT, b]),
                        _ => unreachable!(),
                    }
                    self.state = FilterState::Data;
                }
                FilterState::Subneg => {
                    if b == IAC {
                        self.state = FilterState::SubnegIac;
                    } else {
                        self.subneg.push(b);
                    }
                }
                FilterState::SubnegIac => {
                    if b == SE {
                        if self.subneg.len() >= 2
                            && self.subneg[0] == OPT_TTYPE
                            && self.subneg[1] == TTYPE_SEND
                        {
                            resp.extend([IAC, SB, OPT_TTYPE, TTYPE_IS]);
                            resp.extend(TERM_TYPE.as_bytes());
                            resp.extend([IAC, SE]);
                        }
                        self.state = FilterState::Data;
                    } else if b == IAC {
                        self.subneg.push(IAC);
                        self.state = FilterState::Subneg;
                    } else {
                        self.state = FilterState::Data;
                    }
                }
            }
        }
        (data, resp)
    }
}

fn run_telnet(
    host: &str,
    port: u16,
    cols: u16,
    rows: u16,
    inner: Arc<SessionInner>,
    rx: Receiver<Vec<u8>>,
    notify: Notify,
) {
    set_status(&inner, format!("connecting to {host}:{port}..."), &notify);
    let stream = match TcpStream::connect((host, port)) {
        Ok(s) => s,
        Err(e) => {
            set_status(&inner, format!("connection failed: {e}"), &notify);
            finish(&inner, notify);
            return;
        }
    };
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut stream = stream;
    let mut filter = TelnetFilter::new();
    let mut sanitizer = AnsiSanitizer::new();
    let mut buf = [0u8; 8192];

    'outer: loop {
        if inner.closed.load(Ordering::SeqCst) {
            break;
        }
        match stream.read(&mut buf) {
            Ok(0) => {
                set_status(
                    &inner,
                    "connection closed by remote host".to_string(),
                    &notify,
                );
                break;
            }
            Ok(n) => {
                let (data, resp) = filter.feed(&buf[..n], cols, rows);
                if !resp.is_empty() && stream.write_all(&resp).is_err() {
                    set_status(
                        &inner,
                        "connection lost while negotiating".to_string(),
                        &notify,
                    );
                    break 'outer;
                }
                if !data.is_empty() {
                    push_output(&inner, &mut sanitizer, &data, &notify);
                }
            }
            Err(ref e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
            }
            Err(e) => {
                set_status(&inner, format!("read error: {e}"), &notify);
                break;
            }
        }
        loop {
            match rx.try_recv() {
                Ok(bytes) => {
                    if stream.write_all(&bytes).is_err() {
                        set_status(
                            &inner,
                            "connection closed by remote host".to_string(),
                            &notify,
                        );
                        break 'outer;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'outer,
            }
        }
    }
    finish(&inner, notify);
}

// ----------------------------------------------------------------------
// SSH via russh: password (or default key) auth, PTY + shell, like PuTTY -ssh
// ----------------------------------------------------------------------

struct SshHandler;

impl russh::client::Handler for SshHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Authenticate like the `ssh` CLI does: ssh-agent identities first, then the
/// default identity files (~/.ssh/id_*), then the password (dialog field or
/// BEHE_PASSWORD), then an empty password.
async fn authenticate(
    handle: &mut russh::client::Handle<SshHandler>,
    user: &str,
    password: &str,
) -> Result<(), String> {
    // 1. ssh-agent (SSH_AUTH_SOCK), same keys as the `ssh` command.
    if std::env::var_os("SSH_AUTH_SOCK").is_some() {
        if let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await {
            if let Ok(identities) = agent.request_identities().await {
                for identity in identities {
                    let result = handle
                        .authenticate_publickey_with(
                            user,
                            identity.public_key().into_owned(),
                            None,
                            &mut agent,
                        )
                        .await;
                    if let Ok(result) = result {
                        if result.success() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
    // 2. Default identity files, passphrase-less (like ~/.ssh/config defaults).
    if let Some(home) = dirs::home_dir() {
        for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
            let path = home.join(".ssh").join(name);
            if let Ok(key) = russh::keys::load_secret_key(&path, None) {
                let with_alg = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None);
                if let Ok(result) = handle.authenticate_publickey(user, with_alg).await {
                    if result.success() {
                        return Ok(());
                    }
                }
            }
        }
    }
    // 3. Password from the connection dialog or BEHE_PASSWORD, then empty.
    let password = if password.is_empty() {
        std::env::var("BEHE_PASSWORD").unwrap_or_default()
    } else {
        password.to_string()
    };
    if let Ok(result) = handle.authenticate_password(user, &password).await {
        if result.success() {
            return Ok(());
        }
    }
    if !password.is_empty() {
        if let Ok(result) = handle.authenticate_password(user, "").await {
            if result.success() {
                return Ok(());
            }
        }
    }
    Err("authentication failed (tried ssh-agent, ~/.ssh keys, password)".to_string())
}

fn run_ssh(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    cols: u16,
    rows: u16,
    inner: Arc<SessionInner>,
    rx: Receiver<Vec<u8>>,
    notify: Notify,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            set_status(&inner, format!("failed to start runtime: {e}"), &notify);
            finish(&inner, notify);
            return;
        }
    };
    rt.block_on(async move {
        ssh_session(
            host,
            port,
            username,
            password,
            cols,
            rows,
            Arc::clone(&inner),
            rx,
            &notify,
        )
        .await;
        finish(&inner, notify);
    });
}

async fn ssh_session(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    cols: u16,
    rows: u16,
    inner: Arc<SessionInner>,
    rx: Receiver<Vec<u8>>,
    notify: &Notify,
) {
    set_status(&inner, format!("connecting to {host}:{port}..."), notify);
    let config = Arc::new(russh::client::Config::default());
    let mut handle = match russh::client::connect(config, (host, port), SshHandler).await {
        Ok(h) => h,
        Err(e) => {
            set_status(&inner, format!("connection failed: {e}"), notify);
            return;
        }
    };

    let user = if username.is_empty() {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_default()
    } else {
        username.to_string()
    };
    set_status(&inner, format!("authenticating as {user}..."), notify);
    if let Err(e) = authenticate(&mut handle, &user, password).await {
        set_status(&inner, e, notify);
        return;
    }

    let mut channel = match handle.channel_open_session().await {
        Ok(c) => c,
        Err(e) => {
            set_status(&inner, format!("failed to open channel: {e}"), notify);
            return;
        }
    };
    let _ = channel
        .request_pty(true, TERM_TYPE, cols as u32, rows as u32, 0, 0, &[])
        .await;
    let _ = channel.request_shell(true).await;
    set_status(&inner, format!("connected to {user}@{host}:{port}"), notify);

    let (utx, mut wrx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let bridge = thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            if utx.send(bytes).is_err() {
                break;
            }
        }
    });

    let mut sanitizer = AnsiSanitizer::new();
    loop {
        tokio::select! {
            msg = channel.wait() => {
                match msg {
                    Some(russh::ChannelMsg::Data { data })
                    | Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                        push_output(&inner, &mut sanitizer, &data, notify);
                    }
                    Some(russh::ChannelMsg::Eof) | Some(russh::ChannelMsg::Close) | None => {
                        set_status(&inner, "connection closed by remote host".to_string(), notify);
                        break;
                    }
                    _ => {}
                }
            }
            bytes = wrx.recv() => {
                match bytes {
                    Some(bytes) => {
                        if channel.data_bytes(bytes).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
        if inner.closed.load(Ordering::SeqCst) {
            break;
        }
    }
    let _ = channel.eof().await;
    let _ = channel.close().await;
    drop(bridge);
}

// ----------------------------------------------------------------------
// ANSI/VT100 sanitizer: turns the device's escape sequences into plain text
// for the scrollback view (CR overwrites the current line, BS erases a char,
// CSI/OSC sequences are dropped).
// ----------------------------------------------------------------------

enum EscState {
    None,
    Esc,
    Csi,
    Osc,
    OscEsc,
    Charset,
}

pub struct AnsiSanitizer {
    out: String,
    pending: Vec<u8>, // incomplete UTF-8 bytes / escape sequence across reads
    esc: EscState,
    saw_cr: bool,
}

impl AnsiSanitizer {
    pub fn new() -> Self {
        AnsiSanitizer {
            out: String::new(),
            pending: Vec::new(),
            esc: EscState::None,
            saw_cr: false,
        }
    }

    fn flush_pending_utf8(&mut self) {
        loop {
            let Some(&first) = self.pending.first() else {
                return;
            };
            let need = match first {
                0x00..=0x7f => 1,
                0xc0..=0xdf => 2,
                0xe0..=0xef => 3,
                _ => 4,
            };
            if self.pending.len() < need {
                return;
            }
            let chunk: Vec<u8> = self.pending.drain(..need).collect();
            match std::str::from_utf8(&chunk) {
                Ok(s) => self.out.push_str(s),
                Err(_) => self.out.push('\u{fffd}'),
            }
        }
    }

    /// Drop the current (unfinished) line, emulating CR / EL.
    fn carriage_return(&mut self) {
        if let Some(pos) = self.out.rfind('\n') {
            self.out.truncate(pos + 1);
        } else {
            self.out.clear();
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> String {
        for &b in bytes {
            match self.esc {
                EscState::None => match b {
                    0x1b => {
                        self.flush_pending_utf8();
                        self.esc = EscState::Esc;
                    }
                    0x07 => {} // BEL
                    0x08 => {
                        self.flush_pending_utf8();
                        self.out.pop();
                    }
                    b'\r' => {
                        self.flush_pending_utf8();
                        self.saw_cr = true;
                    }
                    b'\n' => {
                        self.flush_pending_utf8();
                        self.out.push('\n');
                        self.saw_cr = false;
                    }
                    _ => {
                        if self.saw_cr {
                            self.carriage_return();
                            self.saw_cr = false;
                        }
                        self.pending.push(b);
                    }
                },
                EscState::Esc => match b {
                    b'[' => self.esc = EscState::Csi,
                    b']' => self.esc = EscState::Osc,
                    b'(' | b')' => self.esc = EscState::Charset,
                    _ => self.esc = EscState::None, // ESC M, ESC =, ... : drop
                },
                EscState::Csi => {
                    if (0x40..=0x7e).contains(&b) {
                        self.esc = EscState::None;
                    }
                }
                EscState::Osc => match b {
                    0x07 => self.esc = EscState::None,
                    0x1b => self.esc = EscState::OscEsc,
                    _ => {}
                },
                EscState::OscEsc => {
                    self.esc = if b == b'\\' {
                        EscState::None
                    } else {
                        EscState::Osc
                    };
                }
                EscState::Charset => self.esc = EscState::None,
            }
        }
        self.flush_pending_utf8();
        std::mem::take(&mut self.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn test_default_ports() {
        assert_eq!(Protocol::Ssh.default_port(), 22);
        assert_eq!(Protocol::Telnet.default_port(), 23);
        assert_eq!(Protocol::Serial.default_port(), 9600);
    }

    #[test]
    fn test_parse_host_port() {
        assert_eq!(parse_host_port("router1", 22), Some(("router1".into(), 22)));
        assert_eq!(
            parse_host_port("10.0.0.1:2323", 23),
            Some(("10.0.0.1".into(), 2323))
        );
        assert_eq!(parse_host_port("   ", 22), None);
    }

    #[test]
    fn test_parse_remote_command() {
        let ep = parse_remote_command("ssh admin@10.0.0.1:2222").unwrap();
        match ep {
            Endpoint::Remote {
                protocol,
                host,
                port,
                username,
                ..
            } => {
                assert_eq!(protocol, Protocol::Ssh);
                assert_eq!(host, "10.0.0.1");
                assert_eq!(port, 2222);
                assert_eq!(username, "admin");
            }
            _ => panic!("expected remote endpoint"),
        }
        let ep = parse_remote_command("telnet 10.0.0.2").unwrap();
        match ep {
            Endpoint::Remote {
                protocol,
                host,
                port,
                username,
                ..
            } => {
                assert_eq!(protocol, Protocol::Telnet);
                assert_eq!(host, "10.0.0.2");
                assert_eq!(port, 23);
                assert!(username.is_empty());
            }
            _ => panic!("expected remote endpoint"),
        }
        assert!(parse_remote_command("show version").is_none());
        assert!(parse_remote_command("ssh").is_none());
    }

    #[test]
    fn test_check_reachable_open_and_closed_port() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(check_reachable("127.0.0.1", port).is_ok());
        drop(listener);
        assert!(check_reachable("127.0.0.1", port).is_err());
    }

    #[test]
    fn test_check_reachable_bad_host() {
        assert!(check_reachable("256.256.256.256", 22).is_err());
    }

    #[test]
    fn test_check_reachable_hostname_tries_all_resolved_addresses() {
        // "localhost" typically resolves to ::1 first and 127.0.0.1 second;
        // the check must try every resolved address, not just the first one.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(check_reachable("localhost", port).is_ok());
        drop(listener);
        assert!(check_reachable("localhost", port).is_err());
    }

    #[test]
    fn test_connect_fails_fast_on_closed_port() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let ep = Endpoint::Remote {
            protocol: Protocol::Telnet,
            host: "127.0.0.1".into(),
            port,
            username: String::new(),
            password: String::new(),
            vendor: Vendor::Cisco,
        };
        match connect(&ep, None, (80, 24)) {
            Err(e) => assert!(e.to_string().contains("unreachable"), "{e}"),
            Ok(_) => panic!("expected unreachable error"),
        }
    }

    #[test]
    fn test_endpoint_check() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ep = Endpoint::Remote {
            protocol: Protocol::Ssh,
            host: "127.0.0.1".into(),
            port,
            username: String::new(),
            password: String::new(),
            vendor: Vendor::Cisco,
        };
        assert!(ep.check().is_ok());
        drop(listener);
        assert!(ep.check().is_err());

        let serial = Endpoint::Serial {
            path: "/dev/nonexistent-tty".into(),
            baud: 9600,
        };
        assert!(serial.check().is_err());
    }

    #[test]
    fn test_handle_command() {
        assert!(handle_command("show version").is_none());
        assert!(handle_command("ssh 10.0.0.1").is_none()); // handled via parse_remote_command

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let msg = handle_command(&format!("diag 127.0.0.1:{port}")).unwrap();
        assert!(msg.contains("reachable"), "{msg}");
        drop(listener);
        let msg = handle_command(&format!("diag 127.0.0.1:{port}")).unwrap();
        assert!(msg.contains("unreachable"), "{msg}");
    }

    // ------------------------------------------------------------ sanitizer

    #[test]
    fn sanitizer_passes_plain_text_and_crlf() {
        let mut s = AnsiSanitizer::new();
        assert_eq!(s.feed(b"router# show ver\r\n"), "router# show ver\n");
    }

    #[test]
    fn sanitizer_cr_overwrites_current_line() {
        let mut s = AnsiSanitizer::new();
        assert_eq!(s.feed(b"loading 50%\rloading 100%\n"), "loading 100%\n");
    }

    #[test]
    fn sanitizer_backspace_erases_char() {
        let mut s = AnsiSanitizer::new();
        assert_eq!(s.feed(b"show\x08w version\r\n"), "show version\n");
    }

    #[test]
    fn sanitizer_drops_csi_and_osc() {
        let mut s = AnsiSanitizer::new();
        assert_eq!(s.feed(b"\x1b[2J\x1b[H\x1b]0;title\x07hello\x1b[K"), "hello");
    }

    #[test]
    fn sanitizer_handles_split_escape_and_utf8() {
        let mut s = AnsiSanitizer::new();
        assert_eq!(s.feed(b"a\x1b"), "a");
        assert_eq!(s.feed(b"[31mred"), "red");
        let mut s = AnsiSanitizer::new();
        let (a, b) = "ě".as_bytes().split_at(1);
        assert_eq!(s.feed(a), "");
        assert_eq!(s.feed(b), "ě");
    }

    // ------------------------------------------------------------ telnet IAC

    #[test]
    fn telnet_filter_passes_plain_data() {
        let mut f = TelnetFilter::new();
        let (data, resp) = f.feed(b"router# ", 80, 24);
        assert_eq!(data, b"router# ");
        assert!(resp.is_empty());
    }

    #[test]
    fn telnet_filter_answers_will_echo_sga_and_do_sga_naws() {
        let mut f = TelnetFilter::new();
        let (data, resp) = f.feed(
            &[IAC, WILL, OPT_ECHO, IAC, WILL, OPT_SGA, IAC, DO, OPT_SGA],
            80,
            24,
        );
        assert!(data.is_empty());
        assert_eq!(
            resp,
            vec![IAC, DO, OPT_ECHO, IAC, DO, OPT_SGA, IAC, WILL, OPT_SGA]
        );

        let mut f = TelnetFilter::new();
        let (_, resp) = f.feed(&[IAC, DO, OPT_NAWS], 100, 40);
        assert_eq!(
            resp,
            vec![
                IAC, WILL, OPT_NAWS, IAC, SB, OPT_NAWS, 0, 100, 0, 40, IAC, SE
            ]
        );
    }

    #[test]
    fn telnet_filter_refuses_unknown_and_answers_ttype() {
        let mut f = TelnetFilter::new();
        let (_, resp) = f.feed(&[IAC, WILL, 99, IAC, DO, 99], 80, 24);
        assert_eq!(resp, vec![IAC, DONT, 99, IAC, WONT, 99]);

        let mut f = TelnetFilter::new();
        let (data, resp) = f.feed(&[IAC, SB, OPT_TTYPE, TTYPE_SEND, IAC, SE], 80, 24);
        assert!(data.is_empty());
        let mut expected = vec![IAC, SB, OPT_TTYPE, TTYPE_IS];
        expected.extend(TERM_TYPE.as_bytes());
        expected.extend([IAC, SE]);
        assert_eq!(resp, expected);
    }

    #[test]
    fn telnet_filter_unescapes_iac_iac_and_split_sequences() {
        let mut f = TelnetFilter::new();
        let (data, _) = f.feed(&[b'a', IAC, IAC, b'b'], 80, 24);
        assert_eq!(data, vec![b'a', IAC, b'b']);

        let mut f = TelnetFilter::new();
        let (d1, r1) = f.feed(&[b'h', IAC], 80, 24);
        assert_eq!(d1, b"h");
        assert!(r1.is_empty());
        let (d2, r2) = f.feed(&[WILL, OPT_ECHO], 80, 24);
        assert!(d2.is_empty());
        assert_eq!(r2, vec![IAC, DO, OPT_ECHO]);
    }

    // ------------------------------------------------------------ sessions

    fn wait_until(mut cond: impl FnMut() -> bool, timeout_ms: u64) {
        let mut waited = 0;
        while !cond() && waited < timeout_ms {
            thread::sleep(Duration::from_millis(20));
            waited += 20;
        }
    }

    #[test]
    fn telnet_session_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.write_all(b"Username: ").unwrap();
            let mut buf = [0u8; 256];
            let n = stream.read(&mut buf).unwrap();
            stream.write_all(b"\r\nPassword: ").unwrap();
            let n2 = stream.read(&mut buf).unwrap();
            stream
                .write_all(
                    format!("\r\nwelcome {}\r\n", String::from_utf8_lossy(&buf[..n2])).as_bytes(),
                )
                .unwrap();
            let _ = stream.read(&mut buf);
            let _ = n;
        });

        let session = RemoteSession::spawn(
            Protocol::Telnet,
            "127.0.0.1".into(),
            port,
            String::new(),
            String::new(),
            80,
            24,
            Arc::new(|| {}),
        );
        // poll_output drains, so accumulate everything the device sends.
        let mut acc = String::new();
        wait_until(
            || {
                acc.push_str(&session.poll_output());
                acc.contains("Username:")
            },
            5000,
        );
        assert!(acc.contains("Username:"), "got: {acc:?}");

        session.send(b"admin\r");
        wait_until(
            || {
                acc.push_str(&session.poll_output());
                acc.contains("Password:")
            },
            5000,
        );
        assert!(acc.contains("Password:"), "got: {acc:?}");

        session.send(b"secret\r");
        wait_until(
            || {
                acc.push_str(&session.poll_output());
                acc.contains("welcome secret")
            },
            5000,
        );
        assert!(acc.contains("welcome secret"), "got: {acc:?}");
        session.shutdown();
        server.join().unwrap();
        assert!(session.is_closed());
    }

    #[test]
    fn ssh_session_connection_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let session = RemoteSession::spawn(
            Protocol::Ssh,
            "127.0.0.1".into(),
            port,
            "admin".into(),
            "secret".into(),
            80,
            24,
            Arc::new(|| {}),
        );
        wait_until(|| session.is_closed(), 10000);
        assert!(session.is_closed());
        assert!(
            session.status().contains("connection failed"),
            "status: {}",
            session.status()
        );
        session.shutdown();
    }

    // ------------------------------------------- loopback SSH server (test)

    struct LoopbackServer;

    impl russh::server::Server for LoopbackServer {
        type Handler = LoopbackHandler;

        fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> LoopbackHandler {
            LoopbackHandler
        }
    }

    struct LoopbackHandler;

    impl russh::server::Handler for LoopbackHandler {
        type Error = russh::Error;

        async fn auth_password(&mut self, user: &str, password: &str) -> Result<russh::server::Auth, Self::Error> {
            if user == "admin" && password == "secret" {
                Ok(russh::server::Auth::Accept)
            } else {
                Ok(russh::server::Auth::reject())
            }
        }

        async fn channel_open_session(
            &mut self,
            _channel: russh::Channel<russh::server::Msg>,
            reply: russh::server::ChannelOpenHandle,
            _session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            reply.accept().await;
            Ok(())
        }

        async fn pty_request(
            &mut self,
            channel: russh::ChannelId,
            _term: &str,
            _col_width: u32,
            _row_height: u32,
            _pix_width: u32,
            _pix_height: u32,
            _modes: &[(russh::Pty, u32)],
            session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            session.channel_success(channel)?;
            session.data(channel, &b"router# "[..])?;
            Ok(())
        }

        async fn shell_request(
            &mut self,
            channel: russh::ChannelId,
            session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            session.channel_success(channel)?;
            Ok(())
        }

        async fn data(
            &mut self,
            channel: russh::ChannelId,
            data: &[u8],
            session: &mut russh::server::Session,
        ) -> Result<(), Self::Error> {
            session.data(channel, data.to_vec())?;
            if data == b"show version\r" {
                session.data(channel, &b"\r\nCisco IOS 15.5\r\nrouter# "[..])?;
            }
            Ok(())
        }
    }

    fn spawn_loopback_ssh_server() -> u16 {
        use russh::server::Server as _;
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
                let key = russh::keys::PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
                let config = Arc::new(russh::server::Config {
                    keys: vec![key],
                    ..Default::default()
                });
                let mut server = LoopbackServer;
                let running = server.run_on_socket(config, &listener);
                let _ = running.await;
            });
        });
        port
    }

    #[test]
    fn ssh_session_loopback_full_handshake() {
        let port = spawn_loopback_ssh_server();
        thread::sleep(Duration::from_millis(300));

        let session = RemoteSession::spawn(
            Protocol::Ssh,
            "127.0.0.1".into(),
            port,
            "admin".into(),
            "secret".into(),
            80,
            24,
            Arc::new(|| {}),
        );
        let mut acc = String::new();
        wait_until(
            || {
                acc.push_str(&session.poll_output());
                acc.contains("router#")
            },
            15000,
        );
        assert!(acc.contains("router#"), "no prompt, output: {acc:?}, status: {}", session.status());
        assert!(session.status().contains("connected"), "status: {}", session.status());

        session.send(b"show version\r");
        wait_until(
            || {
                acc.push_str(&session.poll_output());
                acc.contains("Cisco IOS 15.5")
            },
            15000,
        );
        assert!(acc.contains("show version"), "input not echoed: {acc:?}");
        assert!(acc.contains("Cisco IOS 15.5"), "no command reply: {acc:?}");
        session.shutdown();
        assert!(session.is_closed());
    }

    #[test]
    fn ssh_session_loopback_wrong_password() {
        let port = spawn_loopback_ssh_server();
        thread::sleep(Duration::from_millis(300));

        let session = RemoteSession::spawn(
            Protocol::Ssh,
            "127.0.0.1".into(),
            port,
            "admin".into(),
            "wrong".into(),
            80,
            24,
            Arc::new(|| {}),
        );
        wait_until(|| session.is_closed(), 15000);
        assert!(session.is_closed());
        assert!(
            session.status().contains("authentication failed"),
            "status: {}",
            session.status()
        );
        session.shutdown();
    }
}
