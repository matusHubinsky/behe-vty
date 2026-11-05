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


use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;
use crate::commands::Vendor;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Serial,
    Ssh,
    Telnet,
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

impl Protocol {
    /// For serial this is the baud rate, for ssh/telnet the TCP port.
    pub fn default_port(&self) -> u16 {
        match self {
            Protocol::Serial => 9600,
            Protocol::Ssh => 22,
            Protocol::Telnet => 23,
        }
    }
    pub fn label(&self) -> &'static str {          // ← new
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
    Serial { path: String, baud: u32 },
    Remote { protocol: Protocol, host: String, port: u16, username: String, vendor: Vendor},
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
    Remote,                                         // russh / telnet session (TODO)
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
pub fn check_reachable(host: &str, port: u16) -> io::Result<()> {
    let addr = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("cannot resolve host '{host}'")))?;
    TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    Ok(())
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

/// Connect to a remote device, checking reachability first so the UI fails
/// fast with a clear message instead of hanging in the connect handshake.
/// Connect to a device described by `endpoint` (which carries the vendor for
/// prompt/command handling), checking reachability first so the UI fails fast.
pub fn connect(endpoint: &Endpoint) -> io::Result<Transport> {
    endpoint
        .check()
        .map_err(|e| io::Error::new(e.kind(), format!("{endpoint:?} unreachable: {e}")))?;
    match endpoint {
        Endpoint::Serial { .. } => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "serial sessions open via open_console()",
        )),
        Endpoint::Remote { protocol, .. } => match protocol {
            Protocol::Ssh => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "ssh session not implemented yet (wire russh here)",
            )),
            Protocol::Telnet => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "telnet session not implemented yet",
            )),
            Protocol::Serial => unreachable!(),
        },
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
pub fn handle_command(input: &str) -> Option<String> {
    let (cmd, rest) = input.split_once(' ')?;
    match cmd {
        "diag" => {
            let (host, port) = parse_host_port(rest, 0)?;
            Some(diagnose(&host, port))
        }
        "telnet" | "ssh" => {
            let protocol = if cmd == "ssh" { Protocol::Ssh } else { Protocol::Telnet };
            let (host, port) = parse_host_port(rest, protocol.default_port())?;
            let ep = Endpoint::Remote {
                protocol,
                host: host.clone(),
                port,
                username: String::new(),
                vendor: Vendor::Cisco,   // ← required now
            };
            Some(match connect(&ep) {
                Ok(_) => format!("% {host}:{port} connected (session plumbing TODO)"),
                Err(e) => format!("% {host}:{port} not connected: {e}"),
            })
        }
        "console" => {
            let mut parts = rest.split_whitespace();
            let path = parts.next()?;
            let baud = parts.next().and_then(|b| b.parse().ok()).unwrap_or(9600);
            let ep = Endpoint::Serial { path: path.to_string(), baud };
            Some(match ep.check() {
                Ok(()) => format!("% console {path} @ {baud} baud opened (session plumbing TODO)"),
                Err(e) => format!("% cannot open console {path}: {e}"),
            })
        }
        _ => None,
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
    fn test_connect_fails_fast_on_closed_port() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let ep = Endpoint::Remote { protocol: Protocol::Telnet, host: "127.0.0.1".into(), port, username: String::new(), vendor: Vendor::Cisco };
        match connect(&ep) {
            Err(e) => assert!(e.to_string().contains("unreachable"), "{e}"),
            Ok(_) => panic!("expected unreachable error"),
        }
    }

    #[test]
    fn test_endpoint_check() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ep = Endpoint::Remote { protocol: Protocol::Ssh, host: "127.0.0.1".into(), port, username: String::new() };
        assert!(ep.check().is_ok());
        drop(listener);
        assert!(ep.check().is_err());

        let serial = Endpoint::Serial { path: "/dev/nonexistent-tty".into(), baud: 9600 };
        assert!(serial.check().is_err());
    }

    #[test]
    fn test_handle_command() {
        assert!(handle_command("show version").is_none());

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let msg = handle_command(&format!("diag 127.0.0.1:{port}")).unwrap();
        assert!(msg.contains("reachable"), "{msg}");
        drop(listener);
        let msg = handle_command(&format!("diag 127.0.0.1:{port}")).unwrap();
        assert!(msg.contains("unreachable"), "{msg}");
    }
}