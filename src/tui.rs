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

use cursive::event::{Event, EventResult, Key, MouseButton, MouseEvent};
use cursive::style::Effect;
use cursive::theme::{Color, ColorStyle};
use cursive::traits::*;
use cursive::utils::markup::StyledString;
use cursive::view::{Margins, ViewWrapper};
use cursive::views::{Dialog, EditView, LinearLayout, NamedView, PaddedView, ResizedView, ScrollView, SelectView, TextView};
use cursive::{Cursive, CursiveExt, Printer, Vec2};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, RwLock};

use crate::cisco;
use crate::commands::{command_matches, CommandTree, Vendor};
use crate::database::{Database, Device};
use crate::networking;
use crate::vault;

const INPUT_WIDTH: usize = 72;
const CURSOR_FPS: u32 = 24;
const BASE_TITLE: &str = "BEHE-VTY";

/// Censorship of sensitive tokens is OFF by default and toggled with Ctrl+H
/// ("hide"): while on, `render_output` replaces logins/IPs/domains/ports with
/// the censorship block and the window title is masked too.
static CENSOR_ON: AtomicBool = AtomicBool::new(false);

/// Last session label shown in the window title, so the Ctrl+H toggle can
/// re-render the title with or without censorship.
static SESSION_DESC: Mutex<Option<String>> = Mutex::new(None);

fn censor_on() -> bool {
    CENSOR_ON.load(Ordering::SeqCst)
}

fn set_censor_on(v: bool) {
    CENSOR_ON.store(v, Ordering::SeqCst);
}
const LOGO: &str = "
██████╗ ███████╗██╗  ██╗███████╗    ██╗   ██╗████████╗██╗   ██╗
██╔══██╗██╔════╝██║  ██║██╔════╝    ██║   ██║╚══██╔══╝╚██╗ ██╔╝
██████╔╝█████╗  ███████║█████╗█████╗██║   ██║   ██║    ╚████╔╝ 
██╔══██╗██╔══╝  ██╔══██║██╔══╝╚════╝╚██╗ ██╔╝   ██║     ╚██╔╝  
██████╔╝███████╗██║  ██║███████╗     ╚████╔╝    ██║      ██║   
╚═════╝ ╚══════╝╚═╝  ╚═╝╚══════╝      ╚═══╝     ╚═╝      ╚═╝";

/// While a remote session is active the window title carries the session label
/// ("telnet lab.com:23 ...") and the input line shows the device's own prompt.
fn set_app_title(s: &mut Cursive, session: Option<&str>) {
    *SESSION_DESC.lock().unwrap() = session.map(|d| d.to_string());
    let title = match session {
        Some(desc) => {
            // The title bar shows the session label (host:port, user@host):
            // mask it only while the Ctrl+H censorship toggle is on.
            let shown = if censor_on() {
                censor_sensitive(desc)
            } else {
                desc.to_string()
            };
            format!("BEHE-VTY | {shown}")
        }
        None => BASE_TITLE.to_string(),
    };
    s.call_on_name("app_dialog", |d: &mut Dialog| d.set_title(title));
}

/// Ctrl+H ("hide"): toggle censorship of sensitive tokens in the scrollback
/// and the window title. The raw scrollback buffer is re-rendered on every
/// toggle, so revealing again loses nothing. Pre-event, so the input line
/// never consumes it (crossterm delivers 0x08 as CtrlChar('h'), distinct
/// from the 0x7F Backspace key).
fn bind_hide_key(siv: &mut Cursive, output_buffer: &OutputBuffer) {
    let output_buffer = Arc::clone(output_buffer);
    siv.set_on_pre_event(Event::CtrlChar('h'), move |s| {
        set_censor_on(!censor_on());
        let snapshot = output_buffer.lock().unwrap().display("");
        set_terminal_output(s, snapshot);
        let desc = SESSION_DESC.lock().unwrap().clone();
        set_app_title(s, desc.as_deref());
    });
}

// ---------------------------------------------------------------- colour schemes

#[derive(Clone, Copy)]
struct Scheme {
    name: &'static str,
    bg: Color,
    fg: Color,
    prompt: Color,
    ghost: Color,
    input_bg: Color,
    cursor: Color,
}

const THEMES: [(&str, &str); 5] = [
    ("Ubuntu",        include_str!("../themes/ubuntu.toml")),
    ("Dark Purple",   include_str!("../themes/dark_purple.toml")),
    ("Classic Green", include_str!("../themes/classic_green.toml")),
    ("Amber",         include_str!("../themes/amber.toml")),
    ("Light",         include_str!("../themes/light.toml")),
];

fn apply_scheme(s: &mut Cursive, index: usize) {
    let (name, toml) = THEMES[index % THEMES.len()];
    if let Err(e) = s.load_toml(toml) {
        eprintln!("Theme '{name}' failed to load: {e:?}");
        return;
    }

    s.call_on_name("terminal_output", |v: &mut TextView| {
        v.set_style(ColorStyle::primary());
    });
    s.call_on_name("prompt", |v: &mut TextView| {
        v.set_style(ColorStyle::title_primary());
    });
}

#[derive(serde::Deserialize)]
struct SchemeToml {
    colors: ColorsToml,
}

#[derive(serde::Deserialize)]
struct ColorsToml {
    background: Option<cursive::reexports::toml::Value>,
    view: Option<cursive::reexports::toml::Value>,
    primary: Option<cursive::reexports::toml::Value>,
    tertiary: Option<cursive::reexports::toml::Value>,
    title_primary: Option<cursive::reexports::toml::Value>,
    highlight: Option<cursive::reexports::toml::Value>,
}

fn toml_color(v: Option<&cursive::reexports::toml::Value>) -> Option<Color> {
    use cursive::reexports::toml::Value;
    match v {
        Some(Value::String(s)) => Color::parse(s),
        Some(Value::Integer(i)) => Some(Color::from_256colors(*i as u8)),
        _ => None,
    }
}

/// Parsed colours for each `THEMES` entry, same order; `name` comes from the tuple.
static SCHEMES: LazyLock<[Scheme; 5]> = LazyLock::new(|| {
    THEMES.map(|(name, src)| {
        let c: SchemeToml =
            cursive::reexports::toml::from_str(src).expect("embedded theme must parse");
        let col = &c.colors;
        Scheme {
            name,
            bg: toml_color(col.background.as_ref()).unwrap_or(Color::TerminalDefault),
            fg: toml_color(col.primary.as_ref()).unwrap_or(Color::TerminalDefault),
            prompt: toml_color(col.title_primary.as_ref()).unwrap_or(Color::TerminalDefault),
            ghost: toml_color(col.tertiary.as_ref()).unwrap_or(Color::TerminalDefault),
            input_bg: toml_color(col.view.as_ref()).unwrap_or(Color::TerminalDefault),
            cursor: toml_color(col.highlight.as_ref()).unwrap_or(Color::TerminalDefault),
        }
    })
});
// ---------------------------------------------------------------- history

struct History {
    lines: Vec<String>,
    index: usize,
}

impl History {
    fn from_lines(lines: Vec<String>) -> Self {
        let index = lines.len();
        Self { lines, index }
    }

    fn add_line(&mut self, line: String) {
        if !line.trim().is_empty() {
            self.lines.push(line);
        }
        self.index = self.lines.len();
    }

    fn current_line(&self) -> String {
        self.lines.get(self.index).cloned().unwrap_or_default()
    }

    fn previous(&mut self) {
        if self.index > 0 {
            self.index -= 1;
        }
    }

    fn next(&mut self) {
        if self.index < self.lines.len() {
            self.index += 1;
        }
    }
}

// ---------------------------------------------------------------- helpers

fn longest_common_prefix(commands: &[String]) -> String {
    let Some(first) = commands.first() else {
        return String::new();
    };

    let mut prefix = first.clone();

    for command in &commands[1..] {
        while !command.starts_with(&prefix) {
            prefix.pop();

            if prefix.is_empty() {
                return String::new();
            }
        }
    }

    prefix
}

/// The inline ghost for `input` against the suggested `command`: what still
/// has to be typed to reach `command`. Supports IOS-style abbreviations, so
/// `conf` against `configure terminal` yields `igure terminal` and `conf t`
/// yields `erminal`.
fn suggestion_tail(command: &str, input: &str) -> String {
    if let Some(rest) = command.strip_prefix(input) {
        return rest.to_string();
    }

    let input_tokens: Vec<&str> = input.split_whitespace().collect();
    let Some(last) = input_tokens.last() else {
        return String::new();
    };

    let command_tokens: Vec<&str> = command.split_whitespace().collect();
    match command_tokens.get(input_tokens.len() - 1) {
        Some(token) if token.starts_with(last) => {
            let mut tail = token[last.len()..].to_string();
            for t in &command_tokens[input_tokens.len()..] {
                tail.push(' ');
                tail.push_str(t);
            }
            tail
        }
        _ => String::new(),
    }
}

/// Orders autocomplete candidates: history-proven commands first (most used
/// first), then the YAML templates in file order. `usage` must already be
/// sorted by frequency.
fn rank_by_usage(yaml: Vec<String>, usage: &[(String, i64)], input: &str) -> Vec<String> {
    let mut ranked: Vec<String> = usage
        .iter()
        .map(|(command, _)| command.clone())
        .filter(|command| command_matches(input, command))
        .collect();

    for command in yaml {
        if !ranked.contains(&command) {
            ranked.push(command);
        }
    }

    ranked
}

/// Scrollback content. Complete lines are committed to `rich`; the trailing
/// partial line is held back in `tail`, so a prompt split across two chunks
/// is never rendered as an extra line.
struct Scrollback {
    rich: String,
    tail: String,
}

impl Scrollback {
    fn new() -> Self {
        Scrollback {
            rich: String::new(),
            tail: String::new(),
        }
    }

    /// Append raw text, committing only complete (newline-terminated) lines.
    fn push(&mut self, text: &str) {
        self.tail.push_str(text);
        let mut start = 0;
        while let Some(i) = self.tail[start..].find('\n') {
            let end = start + i + 1;
            let line = self.tail[start..end].to_string();
            push_line(&mut self.rich, &line);
            start = end;
        }
        self.tail.drain(..start);
    }

    /// Full visible content: committed lines plus any held-back partial line,
    /// plus `extra` (the device's current pending line).
    fn display(&self, extra: &str) -> String {
        let mut rich = self.rich.clone();
        if !self.tail.is_empty() {
            push_line(&mut rich, &self.tail);
        }
        if !extra.is_empty() {
            push_line(&mut rich, extra);
        }
        rich
    }
}

type OutputBuffer = Arc<Mutex<Scrollback>>;

/// Append one line to the scrollback. The prompt is detected as a prefix up to
/// and including the first '#' or '>' in the leading token, so a prompt glued
/// to its echo ("router#show ver") still gets the space after the prompt
/// restored when the device omitted it.
fn push_line(rich: &mut String, line: &str) {
    let token_end = line.find(char::is_whitespace).unwrap_or(line.len());
    let token = &line[..token_end];
    match token.find(['#', '>']) {
        Some(pos) if pos > 0 => {
            let rest = &line[pos + 1..];
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                rich.push_str(line);
            } else {
                // Prompt glued to the echoed command: restore the space.
                rich.push_str(&line[..pos + 1]);
                rich.push(' ');
                rich.push_str(rest);
            }
        }
        _ => rich.push_str(line),
    }
}

/// If `line` (the device's current, possibly incomplete line) is a bare prompt
/// ("router#", "switch> ", "router(config)#"), return that prompt token.
fn prompt_of_line(line: &str) -> Option<String> {
    let token_end = line.find(char::is_whitespace).unwrap_or(line.len());
    let token = &line[..token_end];
    let pos = token.find(['#', '>'])?;
    if pos == 0 {
        return None;
    }
    let rest = &line[pos + 1..];
    if rest.trim().is_empty() {
        Some(token[..pos + 1].to_string())
    } else {
        None
    }
}

fn append_output(s: &mut Cursive, buffer: &OutputBuffer, text: &str) {
    let snapshot = {
        let mut out = buffer.lock().unwrap();
        out.push(text);
        out.display("")
    };

    set_terminal_output(s, snapshot);
}

/// Copy the active mouse selection (scrollback first, then input line) to the
/// clipboard. Returns false when there is no selection.
fn copy_current_selection(s: &mut Cursive) -> bool {
    let scroll = s
        .call_on_name("terminal_scroll", |v: &mut ScrollMouse| v.selected_text())
        .unwrap_or_default();
    let input = s
        .call_on_name("input", |v: &mut InputView| v.selected_text())
        .unwrap_or_default();
    let text = if !scroll.is_empty() { scroll } else { input };
    if text.is_empty() {
        false
    } else {
        copy_to_clipboard(&text);
        true
    }
}

/// Replace the scrollback content (status-colored via `render_output`, plus
/// censored when the Ctrl+H hide toggle is on) and force the terminal view to
/// the bottom, so every Enter and every shell/vty update lands on the newest
/// line even if the user scrolled up before (a manual scroll resets the
/// scroll strategy to `KeepRow`, so re-assert `StickToBottom` here on every
/// update).
fn set_terminal_output(s: &mut Cursive, snapshot: String) {
    s.call_on_name("terminal_output", |v: &mut TextView| {
        v.set_content(render_output(&snapshot, None, censor_on()));
    });
    // The named view wraps the ScrollView around the named TextView, so the
    // type is ScrollMouse — with the wrong type the downcast silently fails
    // and call_on_name returns None (no scrolling).
    s.call_on_name("terminal_scroll", |v: &mut ScrollMouse| {
        let _ = v.inner.set_scroll_strategy(cursive::view::ScrollStrategy::StickToBottom);
        let _ = v.inner.scroll_to_bottom();
        v.apply_selection();
    });
}

// ---------------------------------------------------------------- clipboard

/// RFC 4648 base64, for the OSC 52 clipboard escape (avoids a new dependency).
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16
            | (*chunk.get(1).unwrap_or(&0) as u32) << 8
            | (*chunk.get(2).unwrap_or(&0) as u32);
        out.push(ALPHABET[(n >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(n >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 0x3f] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 0x3f] as char } else { '=' });
    }
    out
}

/// Copy text to the system clipboard with an OSC 52 escape sequence, which
/// works in most terminal emulators (xterm, kitty, iTerm2, Windows Terminal,
/// ...) without a clipboard library. Both the clipboard (`c`, for
/// Ctrl+Shift+V) and the primary selection (`p`, for middle-click paste) are
/// set. On unix the escape is written to /dev/tty — cursive's crossterm
/// backend renders there, while stdout may be redirected elsewhere.
fn copy_to_clipboard(text: &str) {
    if text.is_empty() {
        return;
    }
    use std::io::Write;
    let b64 = base64_encode(text.as_bytes());
    let seq = format!("\x1b]52;c;{b64}\x07\x1b]52;p;{b64}\x07");
    #[cfg(unix)]
    {
        if let Ok(mut tty) = std::fs::File::options().write(true).open("/dev/tty") {
            let _ = tty.write_all(seq.as_bytes());
            let _ = tty.flush();
        }
        // Best-effort fallback for terminals that ignore OSC 52 (VTE, tmux
        // without set-clipboard): feed the X11/Wayland clipboard tools when
        // they are installed. Check PATH first — spawning a missing binary
        // still forks a child, and the resulting SIGCHLD can interrupt the
        // session thread's blocking read (EINTR).
        let path_dirs = std::env::var_os("PATH").unwrap_or_default();
        let on_path = |prog: &str| {
            std::env::split_paths(&path_dirs).any(|d| d.join(prog).is_file())
        };
        for (prog, args) in [
            ("xclip", vec!["-selection", "clipboard"]),
            ("xsel", vec!["--clipboard", "--input"]),
            ("wl-copy", vec![]),
        ] {
            if !on_path(prog) {
                continue;
            }
            if let Ok(mut child) = std::process::Command::new(prog)
                .args(&args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
            }
        }
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

// ------------------------------------------------------- scrollback selection

/// Left-drag selection over the scrollback in content coordinates
/// (x = column, y = row): (anchor, current). `None` = no active selection.
type Selection = Option<(Vec2, Vec2)>;

type TermScroll = ScrollView<NamedView<TextView>>;

// ------------------------------------------------------- output censoring

/// Replacement for sensitive tokens (login, IP, domain, port) in the output.
const CENSOR: &str = "████";

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Length of an IPv4 address starting at `i`, or None. Requires a non-word
/// boundary on both sides so version strings like "15.5.1.2.3" don't match.
fn ipv4_len(chars: &[char], i: usize) -> Option<usize> {
    if i > 0 && (is_word_char(chars[i - 1]) || chars[i - 1] == '.') {
        return None;
    }
    let mut pos = i;
    for octet in 0..4 {
        let start = pos;
        while pos < chars.len() && chars[pos].is_ascii_digit() && pos - start < 3 {
            pos += 1;
        }
        if pos == start {
            return None;
        }
        if octet < 3 {
            if chars.get(pos) != Some(&'.') {
                return None;
            }
            pos += 1;
        }
    }
    if chars.get(pos).map(|c| is_word_char(*c) || *c == '.').unwrap_or(false) {
        return None;
    }
    Some(pos - i)
}

/// Length of a domain name starting at `i` (labels separated by dots, the
/// last label purely alphabetic and at least 2 chars, so "15.5" and "0/1.100"
/// are not treated as domains), or None.
fn domain_len(chars: &[char], i: usize) -> Option<usize> {
    if i > 0 && (is_word_char(chars[i - 1]) || chars[i - 1] == '.') {
        return None;
    }
    let mut pos = i;
    let mut last_dot = None;
    while pos < chars.len() && (is_word_char(chars[pos]) || chars[pos] == '.') {
        if chars[pos] == '.' {
            last_dot = Some(pos);
        }
        pos += 1;
    }
    // strip trailing dots
    while pos > i && chars[pos - 1] == '.' {
        pos -= 1;
    }
    let end = pos;
    if chars.get(end).map(|c| is_word_char(*c) || *c == '.').unwrap_or(false) {
        return None;
    }
    let Some(dot) = last_dot else { return None };
    let tld = &chars[dot + 1..end];
    if tld.len() >= 2 && tld.iter().all(|c| c.is_ascii_alphabetic()) && end > dot + 1 && dot > i {
        Some(end - i)
    } else {
        None
    }
}

/// Length of a login name (word chars) directly followed by '@', or None.
fn login_len(chars: &[char], i: usize) -> Option<usize> {
    if i > 0 && (is_word_char(chars[i - 1]) || chars[i - 1] == '@') {
        return None;
    }
    let mut pos = i;
    while pos < chars.len() && (is_word_char(chars[pos]) || chars[pos] == '.') {
        pos += 1;
    }
    if pos > i && chars.get(pos) == Some(&'@') {
        Some(pos - i)
    } else {
        None
    }
}

/// Length of an IPv6 literal starting at `i` (a run of hex digits and ':',
/// either compressed with "::" or full-form with exactly 8 groups of 1-4
/// hex digits), or None. Boundaries exclude hex/':'/'.' so timestamps like
/// "14:14:15" and MACs like "aa:bb:cc:dd:ee:ff" (6 groups) never match.
fn ipv6_len(chars: &[char], i: usize) -> Option<usize> {
    let hex = |c: char| c.is_ascii_hexdigit();
    if i > 0 && (hex(chars[i - 1]) || chars[i - 1] == ':' || chars[i - 1] == '.') {
        return None;
    }
    let mut pos = i;
    let mut has_double_colon = false;
    let mut groups = 1;
    let mut cur = 0usize;
    while pos < chars.len() && (hex(chars[pos]) || chars[pos] == ':') {
        if chars[pos] == ':' {
            if chars.get(pos + 1) == Some(&':') {
                has_double_colon = true;
                cur = 0;
                pos += 2;
                continue;
            }
            if cur == 0 || cur > 4 {
                return None;
            }
            groups += 1;
            cur = 0;
        } else {
            cur += 1;
            if cur > 4 {
                return None;
            }
        }
        pos += 1;
    }
    if pos == i || !(has_double_colon || (groups == 8 && cur >= 1)) {
        return None;
    }
    if chars.get(pos).map(|c| hex(*c) || *c == ':' || *c == '.').unwrap_or(false) {
        return None;
    }
    Some(pos - i)
}

/// If `chars[i..]` continues with ":digits" (a port suffix), return the end.
fn port_suffix_end(chars: &[char], i: usize) -> Option<usize> {
    if chars.get(i) != Some(&':') {
        return None;
    }
    let mut j = i + 1;
    while j < chars.len() && chars[j].is_ascii_digit() {
        j += 1;
    }
    (j > i + 1).then_some(j)
}

/// Byte index just after the ':' of a leading login prompt ("Username:",
/// "Password:", "login as:", ...), or None when the line is not a prompt.
/// Only a keyword at line start (optionally indented) counts, so banner text
/// ("User Access Verification") and `show running-config` lines ("username
/// admin secret ...", no colon) are left alone.
fn login_prompt_colon_end(body: &str) -> Option<usize> {
    let t = body.trim_start();
    let lead = body.len() - t.len();
    let lower = t.to_ascii_lowercase();
    for kw in ["username", "login as", "login", "password", "passcode"] {
        if let Some(rest) = lower.strip_prefix(kw) {
            if rest.starts_with(':') {
                return Some(lead + kw.len() + 1);
            }
        }
    }
    None
}

/// Replace sensitive tokens (IPv4/IPv6, host:port, domains, login@, a
/// "port N" keyword, and the value typed after a login prompt) with a
/// censorship block. Idempotent: blocks match nothing.
fn censor_sensitive(text: &str) -> String {
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        let body = line.strip_suffix('\n').unwrap_or(&line);
        match login_prompt_colon_end(body) {
            Some(end) => {
                let (head, value) = body.split_at(end);
                out.push_str(head);
                let ws = &value[..value.len() - value.trim_start().len()];
                if value.trim().is_empty() {
                    out.push_str(value); // prompt awaiting input: nothing to hide
                } else {
                    out.push_str(ws);
                    out.push_str(CENSOR);
                }
                if line.ends_with('\n') {
                    out.push('\n');
                }
            }
            None => out.push_str(&censor_scan(line)),
        }
    }
    out
}

/// Char-scan censorship of one line: IPv4/IPv6, host:port, domains, login@,
/// "port N".
fn censor_scan(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if let Some(len) = ipv4_len(&chars, i) {
            let end = port_suffix_end(&chars, i + len).unwrap_or(i + len);
            out.push_str(CENSOR);
            i = end;
            continue;
        }
        if let Some(len) = ipv6_len(&chars, i) {
            out.push_str(CENSOR);
            i += len;
            continue;
        }
        if let Some(len) = login_len(&chars, i) {
            out.push_str(CENSOR);
            out.push('@');
            i += len + 1;
            continue;
        }
        if let Some(len) = domain_len(&chars, i) {
            let end = port_suffix_end(&chars, i + len).unwrap_or(i + len);
            out.push_str(CENSOR);
            i = end;
            continue;
        }
        // "port 2323" as in telnet's "Trying 10.0.0.1, port 2323 ...".
        if i + 4 <= chars.len()
            && chars[i..i + 4].iter().map(|c| c.to_ascii_lowercase()).eq("port".chars())
            && (i == 0 || !is_word_char(chars[i - 1]))
            && !chars.get(i + 4).map(|c| is_word_char(*c)).unwrap_or(false)
        {
            let mut j = i + 4;
            while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t') {
                j += 1;
            }
            let digits = j;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            if j > digits {
                out.extend(chars[i..i + 4].iter());
                out.extend(chars[i + 4..digits].iter());
                out.push_str(CENSOR);
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

// ------------------------------------------------------- status messages

/// Cisco IOS syslog line: a "%FACILITY-SEVERITY-MNEMONIC:" token (facility and
/// mnemonic start with an uppercase letter, severity is one digit 0-7, ':'
/// after the mnemonic), optionally preceded by a timestamp or prompt, e.g.
/// "*Nov  3 14:14:15.001: %LINK-3-UPDOWN: Interface ... changed state to down".
fn is_status_line(line: &str) -> bool {
    let chars: Vec<char> = line.chars().collect();
    for i in 0..chars.len() {
        if chars[i] != '%' {
            continue;
        }
        // '%' glued to a word ("foo%SYS-5-...") is not a syslog token.
        if i > 0 && chars[i - 1].is_ascii_alphanumeric() {
            continue;
        }
        let mut p = i + 1;
        // FACILITY: starts with an uppercase letter, then uppercase/digit/'_'.
        if !chars.get(p).map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
            continue;
        }
        p += 1;
        while p < chars.len() && (chars[p].is_ascii_uppercase() || chars[p].is_ascii_digit() || chars[p] == '_') {
            p += 1;
        }
        if chars.get(p) != Some(&'-') {
            continue;
        }
        p += 1;
        // SEVERITY: exactly one digit 0-7.
        match chars.get(p) {
            Some(c) if c.is_ascii_digit() && *c <= '7' => p += 1,
            _ => continue,
        }
        if chars.get(p) != Some(&'-') {
            continue;
        }
        p += 1;
        // MNEMONIC: starts with an uppercase letter, then uppercase/digit/'_'.
        if !chars.get(p).map(|c| c.is_ascii_uppercase()).unwrap_or(false) {
            continue;
        }
        p += 1;
        while p < chars.len() && (chars[p].is_ascii_uppercase() || chars[p].is_ascii_digit() || chars[p] == '_') {
            p += 1;
        }
        // Real messages have ':' right after the mnemonic (or the line ends).
        match chars.get(p) {
            None | Some(':') | Some('\n') | Some('\r') => return true,
            _ => continue,
        }
    }
    false
}

/// Build the scrollback content as styled spans: Cisco status lines in the
/// theme highlight color, the selected rows (whole lines from anchor to
/// current row) in reverse video on top, and — only when `censor` is on
/// (the Ctrl+H hide toggle) — sensitive tokens replaced with the censorship
/// block.
fn render_output(src: &str, sel: Selection, censor: bool) -> StyledString {
    use cursive::style::Style;
    let mut out = StyledString::default();
    let range = sel.map(|(a, c)| a.y.min(c.y)..=a.y.max(c.y));
    for (i, line) in src.split_inclusive('\n').enumerate() {
        let censored = if censor {
            censor_sensitive(line)
        } else {
            line.to_string()
        };
        let mut style = Style::default();
        if is_status_line(line) {
            style = style.combine(ColorStyle::highlight());
        }
        if range.as_ref().map(|r| r.contains(&i)).unwrap_or(false) {
            style = style.combine(Effect::Reverse);
        }
        out.append_styled(censored, style);
    }
    out
}

/// Wraps the terminal ScrollView with mouse support: left-drag highlights a
/// range of the scrollback and copies it to the clipboard on release; the
/// wheel still scrolls; any interaction leaves keyboard focus on the input
/// line, so clicking the terminal window lets the user type immediately.
struct ScrollMouse {
    inner: TermScroll,
    selection: Selection,
}

impl ScrollMouse {
    fn new(inner: TermScroll) -> Self {
        ScrollMouse {
            inner,
            selection: None,
        }
    }

    /// Re-render the inner TextView with the active selection highlighted.
    fn apply_selection(&mut self) {
        let sel = self.selection;
        let _ = ViewWrapper::with_view_mut(self.inner.get_inner_mut(), |tv: &mut TextView| {
            let src = tv.get_content().source().to_string();
            tv.set_content(render_output(&src, sel, censor_on()));
        });
    }

    /// Text of the selected scrollback rows, for the clipboard copy on release.
    fn selected_text(&self) -> String {
        let Some((a, c)) = self.selection else {
            return String::new();
        };
        let lo = a.y.min(c.y);
        let hi = a.y.max(c.y);
        let src = ViewWrapper::with_view(self.inner.get_inner(), |tv: &TextView| {
            tv.get_content().source().to_string()
        })
        .unwrap_or_default();
        src.split_inclusive('\n')
            .enumerate()
            .filter(|(i, _)| (lo..=hi).contains(i))
            .map(|(_, l)| l)
            .collect()
    }

    fn refresh_selection(&self) -> EventResult {
        EventResult::with_cb(|s| {
            s.call_on_name("terminal_scroll", |v: &mut ScrollMouse| v.apply_selection());
        })
    }
}

impl ViewWrapper for ScrollMouse {
    type V = TermScroll;

    fn with_view<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&Self::V) -> R,
    {
        Some(f(&self.inner))
    }

    fn with_view_mut<F, R>(&mut self, f: F) -> Option<R>
    where
        F: FnOnce(&mut Self::V) -> R,
    {
        Some(f(&mut self.inner))
    }

    fn wrap_take_focus(
        &mut self,
        _source: cursive::direction::Direction,
    ) -> Result<EventResult, cursive::view::CannotFocus> {
        // Always take focus: clicks on the scrollback must reach this wrapper
        // even when the content does not overflow the viewport, where the
        // inner ScrollView would refuse focus (is_scrolling() == false) and
        // the click would be dispatched to the input line instead.
        Ok(EventResult::consumed())
    }

    fn wrap_on_event(&mut self, event: Event) -> EventResult {
        if let Event::Mouse {
            event: me,
            position,
            offset,
        } = event
        {
            match me {
                MouseEvent::Press(MouseButton::Left) => {
                    let local = Vec2::new(
                        position.x.saturating_sub(offset.x),
                        position.y.saturating_sub(offset.y),
                    );
                    let tl = self.inner.content_viewport().top_left();
                    let p = Vec2::new(tl.x + local.x, tl.y + local.y);
                    self.selection = Some((p, p));
                    return self.refresh_selection();
                }
                MouseEvent::Hold(MouseButton::Left) => {
                    if let Some((anchor, _)) = self.selection {
                        let local = Vec2::new(
                            position.x.saturating_sub(offset.x),
                            position.y.saturating_sub(offset.y),
                        );
                        let tl = self.inner.content_viewport().top_left();
                        let p = Vec2::new(tl.x + local.x, tl.y + local.y);
                        self.selection = Some((anchor, p));
                        return self.refresh_selection();
                    }
                }
                MouseEvent::Release(MouseButton::Left) => {
                    // Copy only a real drag; a plain click just clears the selection.
                    match self.selection {
                        Some((a, c)) if a != c => copy_to_clipboard(&self.selected_text()),
                        Some(_) => self.selection = None,
                        None => {}
                    }
                    // A click on the terminal window must leave the keyboard on
                    // the input line, so the user can type right away.
                    return EventResult::with_cb(|s| {
                        let _ = s.focus_name("input");
                    })
                    .and(self.refresh_selection());
                }
                MouseEvent::WheelUp | MouseEvent::WheelDown => {
                    // Let the ScrollView scroll, then put focus back on the input.
                    let r = self.inner.on_event(event);
                    return r.and(EventResult::with_cb(|s| {
                        let _ = s.focus_name("input");
                    }));
                }
                _ => {}
            }
        }
        self.inner.on_event(event)
    }
}

// ---------------------------------------------------------------- input view

/// Single-line input that draws an inline grey suggestion right after the
/// typed text.
struct InputView {
    content: String,
    cursor: usize, // byte index, always on a char boundary
    sel_anchor: Option<usize>, // left-drag selection start (byte index) in the input line
    ghost: String,
    ghost_command: String, // full command the ghost is a tail of
    multiline: bool, // Ctrl+V / pasted newline: Enter inserts a line, Ctrl+Z sends the block
    terminal: Arc<Mutex<cisco::Terminal>>,
    history: Arc<Mutex<History>>,
    output_buffer: OutputBuffer,
    scheme: Arc<AtomicUsize>,
    commands: Arc<RwLock<CommandTree>>,
    database: Arc<Mutex<Database>>,
    device: Arc<Mutex<String>>,
    vendor: Arc<Mutex<Vendor>>,
    session: Arc<Mutex<Option<networking::RemoteSession>>>,
    notify: networking::Notify,
}

impl InputView {
    fn new(
        terminal: Arc<Mutex<cisco::Terminal>>,
        history: Arc<Mutex<History>>,
        output_buffer: OutputBuffer,
        scheme: Arc<AtomicUsize>,
        commands: Arc<RwLock<CommandTree>>,
        database: Arc<Mutex<Database>>,
        device: Arc<Mutex<String>>,
        vendor: Arc<Mutex<Vendor>>,
        session: Arc<Mutex<Option<networking::RemoteSession>>>,
        notify: networking::Notify,
    ) -> Self {
        Self {
            content: String::new(),
            cursor: 0,
            sel_anchor: None,
            ghost: String::new(),
            ghost_command: String::new(),
            multiline: false,
            terminal,
            history,
            output_buffer,
            scheme,
            commands,
            database,
            device,
            vendor,
            session,
            notify,
        }
    }

    fn set_content(&mut self, text: String) {
        self.cursor = text.len();
        self.content = text;
        self.update_ghost();
    }

    /// Currently mouse-selected text in the input line ("" when none).
    fn selected_text(&self) -> String {
        match self.sel_anchor {
            Some(a) if a != self.cursor => {
                let (lo, hi) = if a < self.cursor { (a, self.cursor) } else { (self.cursor, a) };
                self.content[lo..hi].to_string()
            }
            _ => String::new(),
        }
    }

    /// Byte index of the character at (row, col) in the input buffer.
    fn byte_at(&self, row: usize, col: usize) -> usize {
        let mut start = 0;
        for (i, line) in self.content.split('\n').enumerate() {
            if i == row {
                let mut byte = start;
                for (ci, ch) in line.char_indices() {
                    if ci >= col {
                        break;
                    }
                    byte = start + ci + ch.len_utf8();
                }
                return byte.min(self.content.len());
            }
            start += line.len() + 1;
        }
        self.content.len()
    }

    fn matches(&self) -> Vec<String> {
        let (device, vendor) = {
            let device = self.device.lock().unwrap().clone();
            let vendor = *self.vendor.lock().unwrap();
            (device, vendor)
        };
        let usage = self
            .database
            .lock()
            .unwrap()
            .frequent_commands(&device, &vendor.label().to_lowercase(), 500)
            .unwrap_or_default();

        let yaml = {
            let term = self.terminal.lock().unwrap();
            self.commands
                .read()
                .unwrap()
                .autocomplete(&term.mode, &self.content)
        };

        rank_by_usage(yaml, &usage, &self.content)
    }

    fn update_ghost(&mut self) {
        // Only suggest on a single line, when the cursor is at the end of it.
        if self.multiline || self.content.is_empty() || self.cursor != self.content.len() {
            self.ghost.clear();
            self.ghost_command.clear();
            return;
        }

        let matches = self.matches();

        self.ghost_command = matches.first().cloned().unwrap_or_default();
        self.ghost = suggestion_tail(&self.ghost_command, &self.content);
    }

    fn accept_ghost(&mut self) -> bool {
        if self.ghost.is_empty() {
            return false;
        }

        self.ghost.clear();
        let command = std::mem::take(&mut self.ghost_command);
        self.set_content(command);
        true
    }

    /// Ctrl+G: complete fully if there is one match, otherwise extend to the
    /// longest common prefix and list the options.
    fn complete(&mut self) -> EventResult {
        let matches = self.matches();
        let buffer = Arc::clone(&self.output_buffer);

        match matches.as_slice() {
            [] => EventResult::with_cb(move |s| {
                append_output(s, &buffer, "% No matching commands\n");
            }),

            [command] => {
                self.set_content(command.clone());
                EventResult::consumed()
            }

            commands => {
                let prefix = longest_common_prefix(commands);

                if prefix.len() > self.content.len() {
                    self.set_content(prefix);
                }

                let listing = format!("{}\n", commands.join("\n"));

                EventResult::with_cb(move |s| {
                    append_output(s, &buffer, &listing);
                })
            }
        }
    }

    fn submit(&mut self) -> EventResult {
        let input = std::mem::take(&mut self.content);
        self.cursor = 0;
        self.ghost.clear();
        self.ghost_command.clear();
        let was_multiline = self.multiline;
        self.multiline = false;

        let lines: Vec<String> = input.split('\n').map(|l| l.to_string()).collect();
        if lines.len() > 1 || was_multiline {
            return self.submit_block(lines);
        }
        let input = lines.into_iter().next().unwrap_or_default();

        self.history.lock().unwrap().add_line(input.clone());
        let device = self.device.lock().unwrap().clone();
        let _ = self.database.lock().unwrap().add_history(&device, &input);

        // `ssh` / `telnet` commands open (or replace) a live remote session.
        if let Some(endpoint) = networking::parse_remote_command(&input) {
            let buffer = Arc::clone(&self.output_buffer);
            let session = Arc::clone(&self.session);
            let notify = Arc::clone(&self.notify);
            let device = Arc::clone(&self.device);
            let vendor = Arc::clone(&self.vendor);
            return EventResult::with_cb(move |s| {
                if let Some(old) = session.lock().unwrap().take() {
                    old.shutdown();
                }
                let size = s.screen_size();
                let cols = size.x.saturating_sub(4).max(20) as u16;
                let rows = size.y.saturating_sub(6).max(5) as u16;
                let label = match &endpoint {
                    networking::Endpoint::Remote {
                        protocol,
                        host,
                        port,
                        ..
                    } => {
                        format!("{} {}:{}", protocol.label(), host, port)
                    }
                    networking::Endpoint::Serial { path, .. } => format!("serial {path}"),
                };
                match networking::connect(&endpoint, Some(Arc::clone(&notify)), (cols, rows)) {
                    Ok(networking::Transport::Remote(sess)) => {
                        let desc = sess.describe();
                        *session.lock().unwrap() = Some(sess);
                        if let networking::Endpoint::Remote { host, vendor: v, .. } = &endpoint {
                            *device.lock().unwrap() = host.clone();
                            *vendor.lock().unwrap() = *v;
                        }
                        append_output(
                            s,
                            &buffer,
                            &format!("% {desc} connecting... (type 'disconnect' to close)\n"),
                        );
                        // The session label moves to the window title; the input
                        // line shows the device's own prompt once it arrives.
                        set_app_title(s, Some(&desc));
                        s.call_on_name("prompt", |v: &mut TextView| {
                            v.set_content(String::new());
                        });
                    }
                    Ok(_) => append_output(s, &buffer, &format!("% {label} opened\n")),
                    Err(e) => append_output(s, &buffer, &format!("% {label} failed: {e}\n")),
                }
            });
        }

        // `disconnect` closes the active remote session (PuTTY: close window).
        if input.trim() == "disconnect" {
            let buffer = Arc::clone(&self.output_buffer);
            let terminal = Arc::clone(&self.terminal);
            let closed = {
                let mut guard = self.session.lock().unwrap();
                guard.take().map(|s| {
                    s.shutdown();
                    true
                })
            };
            return EventResult::with_cb(move |s| {
                let msg = if closed.unwrap_or(false) {
                    "% session disconnected\n"
                } else {
                    "% no active remote session\n"
                };
                append_output(s, &buffer, msg);
                set_app_title(s, None);
                let prompt = terminal.lock().unwrap().prompt.clone();
                s.call_on_name("prompt", |v: &mut TextView| {
                    v.set_content(prompt);
                });
            });
        }

        // Connection / diagnostic commands are handled by networking,
        // not by the cisco simulator.
        if let Some(reply) = networking::handle_command(&input) {
            let prompt = self.terminal.lock().unwrap().prompt.clone();
            let buffer = Arc::clone(&self.output_buffer);
            return EventResult::with_cb(move |s| {
                append_output(s, &buffer, &format!("{prompt} {input}\n{reply}\n"));
            });
        }

        // With a live remote session the line goes to the device (Enter = CR),
        // like in PuTTY; the device echoes it and prints its own reply.
        if let Some(sess) = self.session.lock().unwrap().as_ref() {
            sess.send(format!("{input}\r").as_bytes());
            return EventResult::consumed();
        }

        let (old_prompt, new_prompt, reply, end): (String, String, String, bool) = {
            let mut term = self.terminal.lock().unwrap();
            let old_prompt = term.prompt.clone();

            term.input = input.clone();
            cisco::terminal(&mut term);

            (
                old_prompt,
                term.prompt.clone(),
                term.output.clone(),
                term.end,
            )
        };

        let mut text = format!("{} {}\n", old_prompt, input);

        if !reply.is_empty() && reply != input {
            text.push_str(&format!("{}\n", reply));
        }

        let buffer = Arc::clone(&self.output_buffer);

        EventResult::with_cb(move |s| {
            append_output(s, &buffer, &text);

            s.call_on_name("prompt", |v: &mut TextView| {
                v.set_content(new_prompt.clone());
            });

            if end {
                s.quit();
            }
        })
    }

    /// Multi-line buffer submitted (Ctrl+Z, or Enter while in multiline mode):
    /// paste the whole block at once. With a live remote session every line is
    /// sent with CR and the block ends with Ctrl+Z (0x1A), so Cisco accepts a
    /// pasted config and leaves config mode. Otherwise each line goes through
    /// the local simulator.
    fn submit_block(&mut self, lines: Vec<String>) -> EventResult {
        {
            let mut h = self.history.lock().unwrap();
            for l in &lines {
                h.add_line(l.clone());
            }
        }
        {
            let device = self.device.lock().unwrap().clone();
            let db = self.database.lock().unwrap();
            for l in &lines {
                let _ = db.add_history(&device, l);
            }
        }

        if let Some(sess) = self.session.lock().unwrap().as_ref() {
            for l in &lines {
                sess.send(format!("{l}\r").as_bytes());
            }
            sess.send(&[0x1a]);
            return EventResult::consumed();
        }

        let mut text = String::new();
        let mut end = false;
        let mut new_prompt = String::new();
        {
            let mut term = self.terminal.lock().unwrap();
            for l in &lines {
                let old_prompt = term.prompt.clone();
                term.input = l.clone();
                cisco::terminal(&mut term);

                text.push_str(&format!("{} {}\n", old_prompt, l));
                if !term.output.is_empty() && term.output != *l {
                    text.push_str(&format!("{}\n", term.output));
                }
                new_prompt = term.prompt.clone();
                if term.end {
                    end = true;
                    break;
                }
            }
        }

        let buffer = Arc::clone(&self.output_buffer);
        EventResult::with_cb(move |s| {
            append_output(s, &buffer, &text);
            s.call_on_name("prompt", |v: &mut TextView| {
                v.set_content(new_prompt.clone());
            });
            if end {
                s.quit();
            }
        })
    }

    fn history_move(&mut self, up: bool) {
        let line = {
            let mut h = self.history.lock().unwrap();

            if up {
                h.previous();
            } else {
                h.next();
            }

            h.current_line()
        };

        self.set_content(line);
    }
}

impl View for InputView {
    fn draw(&self, printer: &Printer) {
        let sc = SCHEMES[self.scheme.load(Ordering::Relaxed) % SCHEMES.len()];
        let normal = ColorStyle::new(sc.fg, sc.input_bg);
        let ghost = ColorStyle::new(sc.ghost, sc.input_bg);
        let cur = ColorStyle::new(sc.input_bg, sc.cursor);

        // Background across the whole (possibly multi-line) field.
        printer.with_color(normal, |p| {
            for y in 0..printer.size.y {
                p.print_hline((0, y), printer.size.x, " ");
            }
        });

        let lines: Vec<&str> = self.content.split('\n').collect();
        let height = printer.size.y.max(1);

        // Cursor row/column within the buffer.
        let before = &self.content[..self.cursor];
        let cursor_row = before.matches('\n').count();
        let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let cursor_col = before[line_start..].chars().count();

        // Vertical scroll so the cursor row stays visible.
        let row_offset = cursor_row.saturating_sub(height.saturating_sub(1));
        let width = printer.size.x.saturating_sub(1).max(1);

        // Active mouse selection in the input line, as a byte range.
        let sel = match self.sel_anchor {
            Some(a) if a != self.cursor => Some((a.min(self.cursor), a.max(self.cursor))),
            _ => None,
        };
        let sel_style = ColorStyle::new(sc.input_bg, sc.fg); // swapped = highlight

        let mut row_byte = 0; // byte index of the current row's first char
        for (row, line) in lines.iter().enumerate() {
            if row < row_offset {
                row_byte += line.len() + 1;
                continue;
            }
            let y = row - row_offset;
            if y >= height {
                break;
            }

            let chars: Vec<char> = line.chars().collect();
            // Horizontal scroll so the cursor column stays visible.
            let col_offset = if row == cursor_row {
                cursor_col.saturating_sub(width)
            } else {
                0
            };

            // Print the visible part, highlighting the selected byte range.
            let skip_bytes: usize = chars.iter().take(col_offset).map(|c| c.len_utf8()).sum();
            let mut x = 0usize;
            let mut run = String::new();
            let mut run_sel = false;
            let mut byte = row_byte + skip_bytes;
            for ch in chars.iter().skip(col_offset).take(width) {
                let is_sel = sel.map(|(lo, hi)| byte >= lo && byte < hi).unwrap_or(false);
                if is_sel != run_sel && !run.is_empty() {
                    let style = if run_sel { sel_style } else { normal };
                    printer.with_color(style, |p| p.print((x, y), &run));
                    x += run.chars().count();
                    run.clear();
                }
                run_sel = is_sel;
                run.push(*ch);
                byte += ch.len_utf8();
            }
            if !run.is_empty() {
                let style = if run_sel { sel_style } else { normal };
                printer.with_color(style, |p| p.print((x, y), &run));
            }
            row_byte += line.len() + 1;

            // Ghost text right after the typed text (single-line mode only).
            if row == cursor_row && !self.ghost.is_empty() {
                let typed_cols = chars.len().saturating_sub(col_offset);
                if typed_cols < width {
                    let room = width - typed_cols;
                    let g: String = self.ghost.chars().take(room).collect();
                    printer.with_color(ghost, |p| p.print((typed_cols, y), &g));
                }
            }

            // Cursor.
            if printer.focused && row == cursor_row {
                let blink_on = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() / 530 % 2 == 0)
                    .unwrap_or(true);

                if blink_on {
                    let col = cursor_col.saturating_sub(col_offset);
                    let under: String = if cursor_col < chars.len() {
                        chars[cursor_col].to_string()
                    } else if let Some(first) = self.ghost.chars().next() {
                        first.to_string()
                    } else {
                        " ".to_string()
                    };
                    printer.with_color(cur, |p| p.print((col, y), &under));
                }
            }
        }
    }

    fn required_size(&mut self, constraint: Vec2) -> Vec2 {
        let lines = self.content.matches('\n').count() + 1;
        let height = if constraint.y > 0 {
            lines.min(constraint.y)
        } else {
            lines
        };
        Vec2::new(INPUT_WIDTH, height.max(1))
    }

    fn take_focus(
        &mut self,
        _: cursive::direction::Direction,
    ) -> Result<EventResult, cursive::view::CannotFocus> {
        Ok(EventResult::consumed())
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        // Any keyboard edit ends an input-line mouse selection.
        if !matches!(event, Event::Mouse { .. }) {
            self.sel_anchor = None;
        }
        match event {
            Event::Mouse {
                event: MouseEvent::Press(MouseButton::Left),
                position,
                offset,
            } => {
                let local = Vec2::new(
                    position.x.saturating_sub(offset.x),
                    position.y.saturating_sub(offset.y),
                );
                self.cursor = self.byte_at(local.y, local.x);
                self.sel_anchor = Some(self.cursor);
                self.update_ghost();
                EventResult::consumed()
            }

            Event::Mouse {
                event: MouseEvent::Hold(MouseButton::Left),
                position,
                offset,
            } => {
                if self.sel_anchor.is_some() {
                    let local = Vec2::new(
                        position.x.saturating_sub(offset.x),
                        position.y.saturating_sub(offset.y),
                    );
                    self.cursor = self.byte_at(local.y, local.x);
                    self.update_ghost();
                }
                EventResult::consumed()
            }

            Event::Mouse {
                event: MouseEvent::Release(MouseButton::Left),
                ..
            } => {
                if let Some(a) = self.sel_anchor {
                    if a != self.cursor {
                        let (lo, hi) = if a < self.cursor { (a, self.cursor) } else { (self.cursor, a) };
                        copy_to_clipboard(&self.content[lo..hi]);
                    }
                }
                EventResult::consumed()
            }

            Event::Mouse {
                event: me @ (MouseEvent::WheelUp | MouseEvent::WheelDown),
                ..
            } => {
                // The input line has focus: forward wheel scrolling to the scrollback.
                EventResult::with_cb(move |s| {
                    s.call_on_name("terminal_scroll", |v: &mut ScrollMouse| {
                        let _ = v.inner.on_event(Event::Mouse {
                            event: me,
                            position: Vec2::zero(),
                            offset: Vec2::zero(),
                        });
                    });
                })
            }

            Event::Char('\n') => {
                // Pasted newline: switch to multi-line input.
                self.multiline = true;
                self.content.insert(self.cursor, '\n');
                self.cursor += 1;
                self.update_ghost();
                EventResult::consumed()
            }

            Event::Char(c) => {
                self.content.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                self.update_ghost();
                EventResult::consumed()
            }

            Event::Key(Key::Backspace) => {
                if self.cursor > 0 {
                    let prev = self.content[..self.cursor]
                        .char_indices()
                        .next_back()
                        .map(|(i, _)| i)
                        .unwrap_or(0);

                    self.content.remove(prev);
                    self.cursor = prev;
                }
                if !self.content.contains('\n') {
                    self.multiline = false;
                }

                self.update_ghost();
                EventResult::consumed()
            }

            Event::Key(Key::Del) => {
                if self.cursor < self.content.len() {
                    self.content.remove(self.cursor);
                }

                self.update_ghost();
                EventResult::consumed()
            }

            Event::Key(Key::Left) => {
                if let Some((i, _)) = self.content[..self.cursor].char_indices().next_back() {
                    self.cursor = i;
                }

                self.update_ghost();
                EventResult::consumed()
            }

            Event::Key(Key::Right) => {
                if self.cursor == self.content.len() {
                    // At end of line: accept the grey suggestion.
                    self.accept_ghost();
                } else if let Some(c) = self.content[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                    self.update_ghost();
                }

                EventResult::consumed()
            }

            Event::Key(Key::Home) => {
                self.cursor = 0;
                self.update_ghost();
                EventResult::consumed()
            }

            Event::Key(Key::End) => {
                if self.cursor == self.content.len() {
                    self.accept_ghost();
                } else {
                    self.cursor = self.content.len();
                    self.update_ghost();
                }

                EventResult::consumed()
            }

            Event::CtrlChar('u') => {
                self.content.drain(..self.cursor);
                self.cursor = 0;
                self.update_ghost();
                EventResult::consumed()
            }

            Event::CtrlChar('g') | Event::Key(Key::Tab) => self.complete(),

            // Ctrl+V: toggle multi-line input (Enter then inserts a newline).
            Event::CtrlChar('v') => {
                self.multiline = !self.multiline;
                self.update_ghost();
                EventResult::consumed()
            }

            // Ctrl+Z: send the whole buffer (a pasted multi-line block ends
            // with 0x1A on the device, so Cisco accepts the config paste).
            Event::CtrlChar('z') => self.submit(),

            Event::Key(Key::Up) => {
                self.history_move(true);
                EventResult::consumed()
            }

            Event::Key(Key::Down) => {
                self.history_move(false);
                EventResult::consumed()
            }

            Event::Key(Key::Enter) => {
                if self.multiline {
                    self.content.insert(self.cursor, '\n');
                    self.cursor += 1;
                    self.update_ghost();
                    EventResult::consumed()
                } else {
                    self.submit()
                }
            }

            _ => EventResult::Ignored,
        }
    }
}

// ---------------------------------------------------------------- connection dialog

/// PuTTY-style startup dialog: pick a saved session or fill in a new one.
/// Shown at startup when encrypted logins exist but no master password is
/// known yet: unlock them, or continue without saved passwords. The
/// connection dialog is already underneath, so dismissing just pops this
/// layer.
fn unlock_dialog(s: &mut Cursive, db: &Arc<Mutex<Database>>, master: &Arc<Mutex<Option<String>>>) {
    let db2 = Arc::clone(db);
    let master2 = Arc::clone(master);

    s.add_layer(
        Dialog::new()
            .title("Master password")
            .content(
                LinearLayout::vertical()
                    .child(TextView::new(
                        "Saved logins are encrypted.\nEnter the master password to unlock them.",
                    ))
                    .child(EditView::new().secret().fixed_width(24).with_name("master_pass"))
                    .child(TextView::new("").with_name("master_error")),
            )
            .button("Unlock", move |s| {
                let pass = s
                    .call_on_name("master_pass", |v: &mut EditView| v.get_content().to_string())
                    .unwrap_or_default();

                if pass.is_empty() {
                    return;
                }

                match db2.lock().unwrap().verify_master(&pass) {
                    Ok(true) => {
                        *master2.lock().unwrap() = Some(pass);
                        s.pop_layer();
                    }
                    Ok(false) => {
                        s.call_on_name("master_error", |v: &mut TextView| {
                            v.set_content("Wrong master password.")
                        });
                    }
                    Err(_) => {
                        s.call_on_name("master_error", |v: &mut TextView| {
                            v.set_content("Cannot read saved logins.")
                        });
                    }
                }
            })
            .button("Skip", |s| {
                s.pop_layer();
            }),
    );
}

/// Shown after a successful connect when a password was typed but no master
/// password is known: offer to set one and store the login encrypted.
fn set_master_dialog(
    s: &mut Cursive,
    master: Arc<Mutex<Option<String>>>,
    db: Arc<Mutex<Database>>,
    output_buffer: OutputBuffer,
    device: String,
    username: String,
    password: String,
) {
    let master2 = Arc::clone(&master);
    let db2 = Arc::clone(&db);
    let out2 = Arc::clone(&output_buffer);

    s.add_layer(
        Dialog::new()
            .title("Set master password")
            .content(
                LinearLayout::vertical()
                    .child(TextView::new(
                        "Choose a master password to save this login encrypted.\nLeave it empty to not save the login.",
                    ))
                    .child(EditView::new().secret().fixed_width(24).with_name("new_master")),
            )
            .button("Save", move |s| {
                let pass = s
                    .call_on_name("new_master", |v: &mut EditView| v.get_content().to_string())
                    .unwrap_or_default();
                s.pop_layer();

                if pass.is_empty() {
                    return;
                }

                *master2.lock().unwrap() = Some(pass.clone());
                match db2.lock().unwrap().save_credential(&pass, &username, &password) {
                    Ok(id) => {
                        let _ = db2.lock().unwrap().link_credential(&device, id);
                        append_output(s, &out2, &format!("% login saved for {device}\n"));
                    }
                    Err(_) => append_output(s, &out2, "% could not save login\n"),
                }
            })
            .button("Skip", |s| {
                s.pop_layer();
            }),
    );
}

fn connection_dialog(
    s: &mut Cursive,
    db: &Arc<Mutex<Database>>,
    output_buffer: &OutputBuffer,
    commands: &Arc<RwLock<CommandTree>>,
    device: &Arc<Mutex<String>>,
    vendor: &Arc<Mutex<Vendor>>,
    master: &Arc<Mutex<Option<String>>>,
    session: &Arc<Mutex<Option<networking::RemoteSession>>>,
    notify: &networking::Notify,
) {
    let saved = db.lock().unwrap().list_devices().unwrap_or_default();

    let mut sessions = SelectView::<Device>::new();
    for d in saved {
        // Saved sessions are grouped by host:port (the auto-saved name already
        // is "host:port"); legacy rows named after the bare host still show it.
        let label = if d.name == d.host {
            format!("{}:{}", d.host, d.port)
        } else {
            d.name.clone()
        };
        sessions.add_item(format!("{:<22} [{}]", label, d.protocol), d);
    }
    // Picking a saved session loads it into the form (like PuTTY); the form
    // fields are then the single source of truth for the "Open" button.
    let db_sel = Arc::clone(db);
    let master_sel = Arc::clone(master);
    sessions.set_on_select(move |s, d: &Device| {
        let _ = s.call_on_name("host", |v: &mut EditView| v.set_content(d.host.clone()));
        let _ = s.call_on_name("port", |v: &mut EditView| v.set_content(d.port.to_string()));
        let _ = s.call_on_name("user", |v: &mut EditView| v.set_content(d.username.clone()));
        // With a master password set, prefill the saved login for this device.
        if let Some(master) = master_sel.lock().unwrap().clone() {
            if let Ok(Some((user, pass))) = db_sel.lock().unwrap().credential_for_device(&master, &d.name) {
                let _ = s.call_on_name("user", |v: &mut EditView| v.set_content(user));
                let _ = s.call_on_name("pass", |v: &mut EditView| v.set_content(pass));
            }
        }
        let proto = match d.protocol.as_str() {
            "serial" => networking::Protocol::Serial,
            "telnet" => networking::Protocol::Telnet,
            _ => networking::Protocol::Ssh,
        };
        let _ = s.call_on_name("proto", |v: &mut SelectView<networking::Protocol>| {
            if let Some(i) = (0..v.len()).find(|&i| v.get_item(i).map(|(_, p)| *p == proto).unwrap_or(false)) {
                v.set_selection(i);
            }
        });
        let vendor = Vendor::parse(&d.vendor);
        let _ = s.call_on_name("vendor", |v: &mut SelectView<Vendor>| {
            if let Some(i) = (0..v.len()).find(|&i| v.get_item(i).map(|(_, x)| *x == vendor).unwrap_or(false)) {
                v.set_selection(i);
            }
        });
    });

    // Clicking a saved session (or pressing Enter on it) opens it directly;
    // the on_select prefill above has just filled the form with that session.
    sessions.set_on_submit({
        let db = Arc::clone(db);
        let output_buffer = Arc::clone(output_buffer);
        let commands = Arc::clone(commands);
        let device = Arc::clone(device);
        let vendor = Arc::clone(vendor);
        let master = Arc::clone(master);
        let session = Arc::clone(session);
        let notify = Arc::clone(notify);
        move |s: &mut Cursive, _d: &Device| {
            open_connection(
                s,
                db.clone(),
                output_buffer.clone(),
                commands.clone(),
                device.clone(),
                vendor.clone(),
                master.clone(),
                session.clone(),
                notify.clone(),
            )
        }
    });

    let mut proto = SelectView::<networking::Protocol>::new();
    proto.add_item("serial", networking::Protocol::Serial);
    proto.add_item("ssh", networking::Protocol::Ssh);
    proto.add_item("telnet", networking::Protocol::Telnet);
    proto.set_selection(1);

    let mut vendor_sel = SelectView::<Vendor>::new();
    for v in Vendor::all() {
        vendor_sel.add_item(v.label(), *v);
    }
    vendor_sel.set_selection(0);

    let form = LinearLayout::vertical()
        .child(TextView::new("Saved sessions:"))
        .child(ScrollView::new(sessions.with_name("sessions")).fixed_height(10))
        .child(
            LinearLayout::horizontal()
                .child(TextView::new("Protocol:  "))
                .child(proto.with_name("proto")),
        )
        .child(
            LinearLayout::horizontal()
                .child(TextView::new("Device:    "))
                .child(vendor_sel.with_name("vendor")),
        )
        .child(
            LinearLayout::horizontal()
                .child(TextView::new("Host/path: "))
                .child(EditView::new().with_name("host").fixed_width(48)),
        )
        .child(
            LinearLayout::horizontal()
                .child(TextView::new("Port/baud: "))
                .child(EditView::new().with_name("port").fixed_width(10)),
        )
        .child(
            LinearLayout::horizontal()
                .child(TextView::new("Username:  "))
                .child(EditView::new().with_name("user").fixed_width(24)),
        )
        .child(
            LinearLayout::horizontal()
                .child(TextView::new("Password:  "))
                .child(EditView::new().secret().with_name("pass").fixed_width(24)),
        );

    let db2 = Arc::clone(db);
    let out2 = Arc::clone(output_buffer);
    let cmds2 = Arc::clone(commands);
    let device2 = Arc::clone(device);
    let vendor2 = Arc::clone(vendor);
    let master3 = Arc::clone(master);
    let session2 = Arc::clone(session);
    let notify2 = Arc::clone(notify);

    s.add_layer(
        Dialog::around(form)
            .title("Open connection")
            .padding(Margins::lrtb(1, 1, 1, 1))
            .button("Open", move |s| {
                open_connection(
                    s,
                    db2.clone(),
                    out2.clone(),
                    cmds2.clone(),
                    device2.clone(),
                    vendor2.clone(),
                    master3.clone(),
                    session2.clone(),
                    notify2.clone(),
                )
            })
            .button("Local simulator", |s| {
                s.pop_layer();
            })
            .button("Quit", |s| s.quit()),
    );
}

/// Connect using the connection dialog's current form fields. Shared by the
/// "Open" button and by submitting a saved session in the list (click or
/// Enter) — the on_select prefill has just filled the form with that session.
fn open_connection(
    s: &mut Cursive,
    db2: Arc<Mutex<Database>>,
    out2: OutputBuffer,
    cmds2: Arc<RwLock<CommandTree>>,
    device2: Arc<Mutex<String>>,
    vendor2: Arc<Mutex<Vendor>>,
    master3: Arc<Mutex<Option<String>>>,
    session2: Arc<Mutex<Option<networking::RemoteSession>>>,
    notify2: networking::Notify,
) {
    let proto = s
        .call_on_name("proto", |v: &mut SelectView<networking::Protocol>| {
            v.selected_id().and_then(|i| v.get_item(i).map(|(_, p)| *p))
        })
        .flatten()
        .unwrap_or(networking::Protocol::Ssh);

    let host = s
        .call_on_name("host", |v: &mut EditView| v.get_content().to_string())
        .unwrap_or_default();
    let port_txt = s
        .call_on_name("port", |v: &mut EditView| v.get_content().to_string())
        .unwrap_or_default();
    let user = s
        .call_on_name("user", |v: &mut EditView| v.get_content().to_string())
        .unwrap_or_default();
    let pass = s
        .call_on_name("pass", |v: &mut EditView| v.get_content().to_string())
        .unwrap_or_default();

    let vendor = s
        .call_on_name("vendor", |v: &mut SelectView<Vendor>| {
            v.selected_id().and_then(|i| v.get_item(i).map(|(_, x)| *x))
        })
        .flatten()
        .unwrap_or(Vendor::Cisco);

    let port = port_txt.parse().unwrap_or(proto.default_port());
    // Key the saved session by host:port, not just the hostname, so the same
    // host on ssh (22) and telnet (23) stays two separate saved sessions.
    let name = if host.is_empty() {
        "adhoc".to_string()
    } else {
        format!("{host}:{port}")
    };

    let endpoint = match proto {
        networking::Protocol::Serial => networking::Endpoint::Serial {
            path: host.clone(),
            baud: u32::from(port),
        },
        p => networking::Endpoint::Remote {
            protocol: p,
            host: host.clone(),
            port,
            username: user.clone(),
            password: pass.clone(),
            vendor,
        },
    };

    if vendor == Vendor::Other {
        append_output(
            s,
            &out2,
            "% warning: unknown vendor \"other\" — no command templates, the shell may not work as intended\n",
        );
    }

    // The connection dialog is popped at the end of this handler;
    // the master-password dialog must be added only afterwards,
    // otherwise that final pop removes it instead.
    let mut ask_for_master = false;

    match &endpoint {
        networking::Endpoint::Remote { host, port, .. } => {
            let size = s.screen_size();
            let cols = size.x.saturating_sub(4).max(20) as u16;
            let rows = size.y.saturating_sub(6).max(5) as u16;
            match networking::connect(&endpoint, Some(Arc::clone(&notify2)), (cols, rows)) {
                Ok(networking::Transport::Remote(sess)) => {
                    let desc = sess.describe();
                    *session2.lock().unwrap() = Some(sess);
                    *device2.lock().unwrap() = name.clone();
                    *vendor2.lock().unwrap() = vendor;
                    let dev = Device {
                        name: name.clone(),
                        host: host.clone(),
                        port: *port,
                        protocol: proto.label().to_string(),
                        vendor: vendor.label().to_lowercase(),
                        username: user.clone(),
                    };
                    let _ = db2.lock().unwrap().save_device(&dev);
                    // Remember the login (encrypted) for next time.
                    match (master3.lock().unwrap().clone(), user.is_empty() || pass.is_empty()) {
                        (Some(master), false) => {
                            match db2.lock().unwrap().save_credential(&master, &user, &pass) {
                                Ok(id) => {
                                    let _ = db2.lock().unwrap().link_credential(&name, id);
                                    append_output(s, &out2, &format!("% login saved for {name}\n"));
                                }
                                Err(_) => append_output(s, &out2, "% could not save login\n"),
                            }
                        }
                        (None, false) => ask_for_master = true,
                        _ => {}
                    }
                    // Switch the autocomplete tree to the selected vendor.
                    *cmds2.write().unwrap() = CommandTree::load(vendor);
                    append_output(s, &out2, &format!("% {desc} connected — session active (type 'disconnect' to close)\n"));
                    set_app_title(s, Some(&desc));
                    s.call_on_name("prompt", |v: &mut TextView| {
                        v.set_content(String::new());
                    });
                }
                Ok(_) => append_output(s, &out2, &format!("% {host}:{port} opened\n")),
                Err(e) => append_output(s, &out2, &format!("% {host}:{port} failed: {e}\n")),
            }
        }
        networking::Endpoint::Serial { path, baud } => match networking::connect(&endpoint, None, (0, 0)) {
            Ok(networking::Transport::Serial(port)) => append_output(
                s,
                &out2,
                &format!("% console {} @ {baud} baud opened (serial session plumbing TODO)\n", port.name().unwrap_or_else(|| path.clone())),
            ),
            Ok(_) => append_output(s, &out2, &format!("% console {path} opened\n")),
            Err(e) => append_output(s, &out2, &format!("% cannot open console {path}: {e}\n")),
        },
    }
    s.pop_layer();
    if ask_for_master {
        set_master_dialog(
            s,
            Arc::clone(&master3),
            Arc::clone(&db2),
            Arc::clone(&out2),
            name.clone(),
            user.clone(),
            pass.clone(),
        );
    }
}

// ---------------------------------------------------------------- run

/// Min interval between two scrollback refreshes. Notifications arriving inside
/// the window are deferred to one trailing refresh, not dropped.
const REFRESH_THROTTLE: std::time::Duration = std::time::Duration::from_millis(50);

/// Queue one UI refresh: drain new device output into the scrollback, keep the
/// prompt view on the device's current line, and drop the session when the
/// remote closes it.
fn send_output_refresh(
    sink: &cursive::CbSink,
    output_buffer: &OutputBuffer,
    session: &Arc<Mutex<Option<networking::RemoteSession>>>,
    terminal: &Arc<Mutex<cisco::Terminal>>,
    pending: &Arc<AtomicBool>,
    last_sent: &Arc<Mutex<std::time::Instant>>,
) {
    *last_sent.lock().unwrap() = std::time::Instant::now();
    let output_buffer = Arc::clone(output_buffer);
    let session = Arc::clone(session);
    let terminal = Arc::clone(terminal);
    let pending = Arc::clone(pending);
    let sink = sink.clone();
    let _ = sink.send(Box::new(move |s: &mut Cursive| {
        pending.store(false, Ordering::SeqCst);
        let mut text = String::new();
        let mut line = String::new();
        let mut closed_status = None;
        {
            let mut guard = session.lock().unwrap();
            if let Some(sess) = guard.as_ref() {
                // Read the pending line before the output buffer (same lock
                // order as the session thread) to avoid an ABBA deadlock.
                line = sess.current_line();
                text = sess.poll_output();
                if sess.is_closed() {
                    closed_status = Some(sess.status());
                    guard.take();
                }
            }
        }
        // All remote output goes to the scrollback only — never to the
        // input line's prompt view (Cisco status messages would land there).
        if !text.is_empty() {
            let snapshot = {
                let mut out = output_buffer.lock().unwrap();
                out.push(&text);
                // Show the device's pending line (bare prompt) in the scrollback too.
                out.display(&line)
            };
            set_terminal_output(s, snapshot);
        } else if !line.is_empty() {
            // Only the pending line changed (bare prompt): re-render so the
            // scrollback shows it even before the first Enter.
            let snapshot = {
                let out = output_buffer.lock().unwrap();
                out.display(&line)
            };
            set_terminal_output(s, snapshot);
        }
        // Keep the input-line prompt on the device's current line, even when
        // only a bare prompt (no newline) arrived in this chunk.
        if let Some(prompt) = prompt_of_line(&line) {
            s.call_on_name("prompt", |v: &mut TextView| {
                v.set_content(prompt);
            });
        }
        if let Some(status) = closed_status {
            append_output(
                s,
                &output_buffer,
                &format!("% session closed by remote host ({status})\n"),
            );
            let prompt = terminal.lock().unwrap().prompt.clone();
            set_app_title(s, None);
            s.call_on_name("prompt", |v: &mut TextView| {
                v.set_content(prompt);
            });
        }
    }));
}

/// Bridges the session thread to the UI. At most one refresh is in flight and
/// refreshes are throttled to ~20/s, so a chatty telnet/ssh stream cannot flood
/// the event loop with a full scrollback re-render per chunk. A notification
/// landing inside the throttle window is deferred to one trailing refresh, so
/// device output is never stuck until the next keypress.
fn make_notifier(
    sink: cursive::CbSink,
    output_buffer: OutputBuffer,
    session: Arc<Mutex<Option<networking::RemoteSession>>>,
    terminal: Arc<Mutex<cisco::Terminal>>,
) -> networking::Notify {
    let pending = Arc::new(AtomicBool::new(false));
    let last_sent = Arc::new(Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1)));
    Arc::new(move || {
        if pending.swap(true, Ordering::SeqCst) {
            // A refresh is already scheduled or in flight; it drains all
            // buffered output when it runs, so coalescing loses nothing.
            return;
        }
        let closed = session
            .lock()
            .unwrap()
            .as_ref()
            .map(|sess| sess.is_closed())
            .unwrap_or(false);
        let remaining = {
            let last = last_sent.lock().unwrap();
            if closed {
                std::time::Duration::ZERO
            } else {
                REFRESH_THROTTLE.saturating_sub(last.elapsed())
            }
        };
        if !remaining.is_zero() {
            // Throttled: keep `pending` set and schedule one trailing refresh
            // for when the window expires; further notifications coalesce
            // into it meanwhile.
            let sink = sink.clone();
            let output_buffer = Arc::clone(&output_buffer);
            let session = Arc::clone(&session);
            let terminal = Arc::clone(&terminal);
            let pending = Arc::clone(&pending);
            let last_sent = Arc::clone(&last_sent);
            std::thread::spawn(move || {
                std::thread::sleep(remaining);
                send_output_refresh(&sink, &output_buffer, &session, &terminal, &pending, &last_sent);
            });
            return;
        }
        send_output_refresh(&sink, &output_buffer, &session, &terminal, &pending, &last_sent);
    })
}

pub fn run() {
    let mut siv = Cursive::default();

    // Replace the default Ctrl+C -> quit: with an active selection, Ctrl+C /
    // Ctrl+Shift+C (terminals deliver both as 0x03 = CtrlChar('c')) copies the
    // selection instead of closing the window; without a selection it quits
    // as before.
    siv.clear_all_global_callbacks();
    siv.set_on_pre_event(Event::WindowResize, |s| s.clear());
    siv.set_on_pre_event(Event::Exit, |s| s.quit());
    siv.set_on_pre_event(Event::CtrlChar('c'), |s| {
        if !copy_current_selection(s) {
            s.quit();
        }
    });

    let database = Arc::new(Mutex::new(Database::open().expect("cannot open database")));
    let commands = Arc::new(RwLock::new(CommandTree::load(Vendor::Cisco)));
    let device = Arc::new(Mutex::new("local".to_string()));
    let vendor = Arc::new(Mutex::new(Vendor::Cisco));
    let master = Arc::new(Mutex::new(vault::master_password()));

    let past = database
        .lock()
        .unwrap()
        .load_history("local", 1000)
        .unwrap_or_default();
    let history = Arc::new(Mutex::new(History::from_lines(past)));

    let scheme_index = Arc::new(AtomicUsize::new(0));
    let initial = SCHEMES[0];

    let terminal = Arc::new(Mutex::new(cisco::Terminal {
        mode: cisco::CiscoTerminalMode::UserExec,
        prompt: ">".to_string(),
        input: String::new(),
        output: String::new(),
        end: false,
    }));

    let output_buffer: OutputBuffer = Arc::new(Mutex::new(Scrollback::new()));
    bind_hide_key(&mut siv, &output_buffer);
    let session: Arc<Mutex<Option<networking::RemoteSession>>> = Arc::new(Mutex::new(None));
    let notify = make_notifier(
        siv.cb_sink().clone(),
        Arc::clone(&output_buffer),
        Arc::clone(&session),
        Arc::clone(&terminal),
    );

    let text_view = TextView::new("")
        .style(ColorStyle::new(initial.fg, initial.bg))
        .with_name("terminal_output");

    let terminal_output = ScrollMouse::new(
        ScrollView::new(text_view).scroll_strategy(cursive::view::ScrollStrategy::StickToBottom),
    )
    .with_name("terminal_scroll");

    let input_view = InputView::new(
        Arc::clone(&terminal),
        Arc::clone(&history),
        Arc::clone(&output_buffer),
        Arc::clone(&scheme_index),
        Arc::clone(&commands),
        Arc::clone(&database),
        Arc::clone(&device),
        Arc::clone(&vendor),
        Arc::clone(&session),
        Arc::clone(&notify),
    );

    let initial_prompt = LOGO;

    let prompt_view = TextView::new(initial_prompt)
        .style(ColorStyle::new(initial.prompt, initial.bg))
        .with_name("prompt");

    let padded_prompt_view = PaddedView::new(Margins::lrtb(0, 1, 0, 0), prompt_view);

    let input_line = LinearLayout::horizontal()
        .child(padded_prompt_view)
        .child(input_view.with_name("input"));

    // Elastic full-window layout: the root takes the whole screen (StackView
    // clamps the dialog to it, the Dialog subtracts its own border), the
    // output scrollback absorbs every row except the 1-row input line.
    // Resizing the terminal window is handled automatically by re-layout.
    let output_box = ResizedView::with_full_height(terminal_output);
    // No fixed height: the input grows when multi-line paste (Ctrl+V) is used.
    let input_box = input_line.full_width();
    let layout = LinearLayout::vertical().child(output_box).child(input_box);
    let root = ResizedView::with_full_screen(layout);

    siv.add_layer(
        Dialog::around(root)
            .title(BASE_TITLE)
            .padding(Margins::lrtb(1, 1, 1, 1)) // keep padding; no title bar = max rows
            .with_name("app_dialog"),
    );

    apply_scheme(&mut siv, 0);

    connection_dialog(
        &mut siv,
        &database,
        &output_buffer,
        &commands,
        &device,
        &vendor,
        &master,
        &session,
        &notify,
    );

    // Encrypted logins exist but no master password is known: ask for it.
    // The connection dialog stays underneath and appears when this pops.
    let needs_unlock = master.lock().unwrap().is_none()
        && database
            .lock()
            .unwrap()
            .has_credentials()
            .unwrap_or(false);
    if needs_unlock {
        unlock_dialog(&mut siv, &database, &master);
    }

    // Ctrl+T: cycle through the colour schemes.
    {
        let scheme_index = Arc::clone(&scheme_index);
        let output_buffer = Arc::clone(&output_buffer);

        siv.add_global_callback(Event::CtrlChar('t'), move |s| {
            let next = (scheme_index.load(Ordering::Relaxed) + 1) % SCHEMES.len();
            scheme_index.store(next, Ordering::Relaxed);

            apply_scheme(s, next);

            append_output(
                s,
                &output_buffer,
                &format!("% Theme: {}\n", SCHEMES[next].name),
            );
        });
    }

    // Any mouse event no widget consumed (click on the terminal window, dialog
    // padding, ...) puts keyboard focus back on the input line, so clicking the
    // window lets the user type immediately. Clicks on widgets are consumed by
    // the widget and never reach this callback.
    siv.set_on_post_event(cursive::event::EventTrigger::mouse(), |s| {
        let _ = s.focus_name("input");
    });

    siv.set_fps(CURSOR_FPS);
    siv.run();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_of_line_detects_bare_prompt() {
        assert_eq!(prompt_of_line("router# "), Some("router#".to_string()));
        assert_eq!(prompt_of_line("router(config)# "), Some("router(config)#".to_string()));
        assert_eq!(prompt_of_line("user@host> "), Some("user@host>".to_string()));
        assert_eq!(prompt_of_line("router# show ver\n"), None);
        assert_eq!(prompt_of_line("Cisco IOS 15.5\n"), None);
        assert_eq!(prompt_of_line(""), None);
    }

    #[test]
    fn scrollback_commits_only_complete_lines() {
        let mut sb = Scrollback::new();
        // A prompt/command split across two chunks must not be committed yet.
        sb.push("router# show ip int");
        assert_eq!(sb.rich, "");
        assert_eq!(sb.tail, "router# show ip int");
        sb.push("erface brief\nrouter# ");
        assert_eq!(sb.rich, "router# show ip interface brief\n");
        assert_eq!(sb.tail, "router# ");
        assert_eq!(sb.display(""), "router# show ip interface brief\nrouter# ");
        // The device's pending line renders immediately via display(extra).
        let sb2 = Scrollback::new();
        assert_eq!(sb2.display("router# "), "router# ");
        // A prompt glued to its echo (no space in the byte stream) gets the
        // space restored.
        let mut rich = String::new();
        push_line(&mut rich, "router#show ver\n");
        assert_eq!(rich, "router# show ver\n");
    }

    struct FixedBackend {
        size: Vec2,
    }

    impl cursive::backend::Backend for FixedBackend {
        fn poll_event(&mut self) -> Option<Event> {
            None
        }
        fn set_title(&mut self, _title: String) {}
        fn refresh(&mut self) {}
        fn has_colors(&self) -> bool {
            false
        }
        fn screen_size(&self) -> Vec2 {
            self.size
        }
        fn move_to(&self, _pos: Vec2) {}
        fn print(&self, _text: &str) {}
        fn clear(&self, _color: Color) {}
        fn set_color(&self, colors: cursive::theme::ColorPair) -> cursive::theme::ColorPair {
            colors
        }
        fn set_effect(&self, _effect: cursive::theme::Effect) {}
        fn unset_effect(&self, _effect: cursive::theme::Effect) {}
    }

    fn is_at_bottom(s: &mut Cursive) -> Option<bool> {
        s.call_on_name("terminal_scroll", |v: &mut ScrollMouse| v.inner.is_at_bottom())
    }

    /// Builds the same view tree as `run()` (scrollback over a 1-row input
    /// line inside a padded dialog) on a fixed-size backend, with the Ctrl+H
    /// hide keybind installed on the shared scrollback buffer.
    fn test_runner() -> (cursive::CursiveRunner<&'static mut Cursive>, OutputBuffer) {
        let siv = Box::leak(Box::new(Cursive::new()));
        let text_view = TextView::new("").with_name("terminal_output");
        let scroll = ScrollMouse::new(
            ScrollView::new(text_view).scroll_strategy(cursive::view::ScrollStrategy::StickToBottom),
        )
        .with_name("terminal_scroll");
        let layout = LinearLayout::vertical()
            .child(ResizedView::with_full_height(scroll))
            .child(EditView::new().with_name("input").full_width());
        siv.add_layer(
            Dialog::around(ResizedView::with_full_screen(layout))
                .padding(Margins::lrtb(1, 1, 1, 1))
                .with_name("app_dialog"),
        );
        let buffer: OutputBuffer = Arc::new(Mutex::new(Scrollback::new()));
        bind_hide_key(siv, &buffer);
        let mut runner = siv.runner(Box::new(FixedBackend {
            size: Vec2::new(80, 24),
        }));
        runner.refresh();
        (runner, buffer)
    }

    #[test]
    fn set_terminal_output_snaps_to_bottom_after_enter_and_after_scroll_up() {
        let (mut runner, _buffer) = test_runner();
        let lines = |n: usize| (0..n).map(|i| format!("line {i}\n")).collect::<String>();

        // Enter appends output taller than the viewport: must land at the bottom.
        set_terminal_output(&mut runner, lines(40));
        runner.refresh();
        assert_eq!(
            is_at_bottom(&mut runner),
            Some(true),
            "terminal_scroll must be found and at the bottom"
        );

        // User scrolls up: a manual scroll resets the strategy to KeepRow.
        runner.call_on_name("terminal_scroll", |v: &mut ScrollMouse| {
            let _ = v.inner.set_scroll_strategy(cursive::view::ScrollStrategy::KeepRow);
            let _ = v.inner.set_offset(Vec2::new(0, 0));
        });
        runner.refresh();
        assert_eq!(is_at_bottom(&mut runner), Some(false), "scrolled-up viewport should not be at the bottom");

        // Next Enter/output must snap back to the bottom instead of staying stuck.
        set_terminal_output(&mut runner, lines(80));
        runner.refresh();
        assert_eq!(is_at_bottom(&mut runner), Some(true), "viewport must return to the bottom on new output");
    }

    #[test]
    fn base64_encode_matches_rfc4648() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn render_output_censor_flag_controls_masking() {
        let src = "ping 10.0.0.1\n";
        // Censorship off (the default): raw text is shown.
        assert_eq!(render_output(src, None, false).source(), "ping 10.0.0.1\n");
        // Censorship on (Ctrl+H): sensitive tokens become the block.
        assert_eq!(render_output(src, None, true).source(), format!("ping {CENSOR}\n"));
        // Status-line coloring is independent of the censor flag.
        let status = "%LINK-3-UPDOWN: Interface GigabitEthernet0/1, changed state to down\n";
        for flag in [false, true] {
            let styled = render_output(status, None, flag);
            let spans: Vec<_> = styled.spans().collect();
            assert_eq!(spans.len(), 1);
            assert!(spans[0].attr.color != cursive::style::ColorStyle::inherit_parent());
        }
    }

    #[test]
    fn ctrl_h_toggles_censorship_in_scrollback_and_title() {
        let (mut runner, buffer) = test_runner();
        set_censor_on(false);

        // Seed the scrollback through the buffer, like append_output does.
        let snapshot = {
            let mut b = buffer.lock().unwrap();
            b.push("router# ping 10.0.0.1\n");
            b.display("")
        };
        set_terminal_output(&mut runner, snapshot);
        set_app_title(&mut runner, Some("telnet 10.0.0.1:23"));
        runner.refresh();

        let shown = |r: &mut cursive::CursiveRunner<&'static mut Cursive>| {
            r.call_on_name("terminal_output", |v: &mut TextView| {
                v.get_content().source().to_string()
            })
            .unwrap()
        };
        let title = |r: &mut cursive::CursiveRunner<&'static mut Cursive>| {
            r.call_on_name("app_dialog", |d: &mut Dialog| d.get_title().to_string())
                .unwrap_or_default()
        };

        // Default: raw — Enter alone no longer censors.
        assert_eq!(shown(&mut runner), "router# ping 10.0.0.1\n");
        assert!(title(&mut runner).contains("10.0.0.1"));

        // Ctrl+H hides: scrollback and window title are censored.
        runner.on_event(Event::CtrlChar('h'));
        runner.refresh();
        assert_eq!(shown(&mut runner), format!("router# ping {CENSOR}\n"));
        assert!(!title(&mut runner).contains("10.0.0.1"));
        assert!(title(&mut runner).contains(CENSOR));

        // Ctrl+H again reveals the raw buffer — nothing was destroyed.
        runner.on_event(Event::CtrlChar('h'));
        runner.refresh();
        assert_eq!(shown(&mut runner), "router# ping 10.0.0.1\n");
        assert!(title(&mut runner).contains("10.0.0.1"));

        set_censor_on(false);
    }

    #[test]
    fn render_output_marks_only_selected_rows() {
        let src = "line 0\nline 1\nline 2\nline 3\n";
        let styled = render_output(src, Some((Vec2::new(0, 1), Vec2::new(0, 2))), false);
        assert_eq!(styled.source(), src);
        use cursive::style::EffectStatus;
        let spans: Vec<_> = styled.spans().collect();
        assert_eq!(spans.len(), 4);
        let rev = |s: &cursive::utils::span::Span<'_, cursive::style::Style>| {
            s.attr.effects.statuses[Effect::Reverse] != EffectStatus::InheritParent
        };
        assert!(!rev(&spans[0]));
        assert!(rev(&spans[1]));
        assert!(rev(&spans[2]));
        assert!(!rev(&spans[3]));
        // No selection -> all plain.
        let plain = render_output(src, None, false);
        assert!(plain.spans().all(|s| !rev(&s)));
    }

    #[test]
    fn censor_sensitive_replaces_login_ip_domain_port() {
        assert_eq!(censor_sensitive("ping 10.0.0.1"), format!("ping {CENSOR}"));
        assert_eq!(censor_sensitive("telnet 10.0.0.1:2323"), format!("telnet {CENSOR}"));
        assert_eq!(censor_sensitive("ssh admin@10.0.0.1"), format!("ssh {CENSOR}@{CENSOR}"));
        assert_eq!(censor_sensitive("ping core-rtr.example.com"), format!("ping {CENSOR}"));
        assert_eq!(censor_sensitive("connect gw.example.com:22"), format!("connect {CENSOR}"));
        // Not sensitive: commands, prompts, versions, times, interfaces.
        assert_eq!(censor_sensitive("router# show version"), "router# show version");
        assert_eq!(censor_sensitive("Cisco IOS 15.5.1"), "Cisco IOS 15.5.1");
        assert_eq!(censor_sensitive("* Nov  3 14:14:14.123: x"), "* Nov  3 14:14:14.123: x");
        assert_eq!(censor_sensitive("interface GigabitEthernet0/1.100"), "interface GigabitEthernet0/1.100");
        // Idempotent: censoring twice changes nothing.
        let once = censor_sensitive("ping 10.0.0.1 from user@host.io:22");
        assert_eq!(censor_sensitive(&once), once);
    }

    #[test]
    fn censor_sensitive_handles_ipv6_and_port_keyword() {
        // Compressed IPv6 literals are IPs too.
        assert_eq!(censor_sensitive("ping 2001:db8::1"), format!("ping {CENSOR}"));
        assert_eq!(censor_sensitive("ssh admin@fe80::1"), format!("ssh {CENSOR}@{CENSOR}"));
        // "port N" as in telnet's "Trying 10.0.0.1, port 2323 ...".
        assert_eq!(censor_sensitive("telnet 10.0.0.1, port 2323 ..."), format!("telnet {CENSOR}, port {CENSOR} ..."));
        assert_eq!(censor_sensitive("connect gw.example.com, port 22"), format!("connect {CENSOR}, port {CENSOR}"));
        // Timestamps, MACs and words containing "port" stay untouched.
        assert_eq!(censor_sensitive("*Nov  3 14:14:15.001: %LINK-3-UPDOWN: x"), "*Nov  3 14:14:15.001: %LINK-3-UPDOWN: x");
        assert_eq!(censor_sensitive("mac-address aa:bb:cc:dd:ee:ff"), "mac-address aa:bb:cc:dd:ee:ff");
        assert_eq!(censor_sensitive("ip transport 22"), "ip transport 22");
        // Idempotent.
        let once = censor_sensitive("ping 2001:db8::1 via port 22");
        assert_eq!(censor_sensitive(&once), once);
    }

    #[test]
    fn censor_sensitive_handles_full_form_ipv6() {
        // Uncompressed IPv6 (8 groups of 1-4 hex digits, no "::") is an IP too.
        assert_eq!(
            censor_sensitive("ping 2001:0db8:85a3:0000:0000:8a2e:0370:7334"),
            format!("ping {CENSOR}")
        );
        assert_eq!(
            censor_sensitive("ssh admin@2001:0DB8:0000:0000:0000:0000:0000:0001"),
            format!("ssh {CENSOR}@{CENSOR}")
        );
        assert_eq!(censor_sensitive("ipv6 address 0:0:0:0:0:0:0:1/128"), format!("ipv6 address {CENSOR}/128"));
        // Still not IPs: MACs have 6 groups, syslog timestamps have 3.
        assert_eq!(censor_sensitive("mac aa:bb:cc:dd:ee:ff"), "mac aa:bb:cc:dd:ee:ff");
        assert_eq!(
            censor_sensitive("*Nov  3 14:14:15.001: %LINK-3-UPDOWN: x"),
            "*Nov  3 14:14:15.001: %LINK-3-UPDOWN: x"
        );
        // Idempotent.
        let once = censor_sensitive("ping 2001:0db8:85a3:0000:0000:8a2e:0370:7334");
        assert_eq!(censor_sensitive(&once), once);
    }

    #[test]
    fn censor_sensitive_censors_login_prompt_values() {
        // Real telnet/ssh login flow: the typed login/password after the prompt.
        assert_eq!(censor_sensitive("Username: admin\n"), format!("Username: {CENSOR}\n"));
        assert_eq!(censor_sensitive("Password: hunter2"), format!("Password: {CENSOR}"));
        assert_eq!(censor_sensitive("login: admin"), format!("login: {CENSOR}"));
        assert_eq!(censor_sensitive("Login as: admin"), format!("Login as: {CENSOR}"));
        assert_eq!(censor_sensitive("USERNAME: Admin"), format!("USERNAME: {CENSOR}"));
        // A prompt with no value typed yet stays untouched.
        assert_eq!(censor_sensitive("Password: "), "Password: ");
        assert_eq!(censor_sensitive("Username:"), "Username:");
        // Banner text and `show running-config` lines that merely start with
        // a keyword (no colon) are not login prompts.
        assert_eq!(censor_sensitive("User Access Verification"), "User Access Verification");
        assert_eq!(
            censor_sensitive("username admin privilege 15 secret 5 $1$xyz"),
            "username admin privilege 15 secret 5 $1$xyz"
        );
        // Multi-line banner + prompt: only the prompt line is censored.
        assert_eq!(
            censor_sensitive("User Access Verification\n\nUsername: admin\nPassword: x\n"),
            format!("User Access Verification\n\nUsername: {CENSOR}\nPassword: {CENSOR}\n")
        );
        // Idempotent.
        let once = censor_sensitive("Username: admin\nPassword: hunter2\n");
        assert_eq!(censor_sensitive(&once), once);
    }

    #[test]
    fn render_output_censors_login_prompt_and_colors_status_line() {
        let src = "Username: admin\n%LINK-3-UPDOWN: Interface GigabitEthernet0/1, changed state to down\n";
        let styled = render_output(src, None, true);
        assert_eq!(
            styled.source(),
            format!("Username: {CENSOR}\n%LINK-3-UPDOWN: Interface GigabitEthernet0/1, changed state to down\n")
        );
        let spans: Vec<_> = styled.spans().collect();
        assert_eq!(spans.len(), 2);
        let highlighted = |s: &cursive::utils::span::Span<'_, cursive::style::Style>| {
            s.attr.color != cursive::style::ColorStyle::inherit_parent()
        };
        assert!(!highlighted(&spans[0]));
        assert!(highlighted(&spans[1]));
    }

    #[test]
    fn status_lines_detected_and_colored() {
        assert!(is_status_line("* Nov  3 14:14:14.123: %LINEPROTO-5-UPDOWN: Line protocol changed"));
        assert!(is_status_line("%SYS-5-CONFIG_I: Configured from console"));
        assert!(is_status_line("router# %LINK-3-UPDOWN: Interface Ethernet0"));
        // The interface up/down example: state change messages.
        assert!(is_status_line("*Nov  3 14:14:15.001: %LINK-3-UPDOWN: Interface GigabitEthernet0/1, changed state to down"));
        assert!(is_status_line("%LINK-5-CHANGED: Interface GigabitEthernet0/1, changed state to up"));
        assert!(is_status_line("%LINEPROTO-5-UPDOWN: Line protocol on Interface Vlan1, changed state to up"));
        assert!(!is_status_line("router# show version"));
        assert!(!is_status_line("Cisco IOS 15.5"));
        assert!(!is_status_line("some output for: show version"));
        assert!(!is_status_line("% No matching commands"));
        assert!(!is_status_line("100% done"));

        // Status line gets the highlight color; a normal line does not.
        let src = "router# show ver\n%SYS-5-CONFIG_I: done\n";
        let styled = render_output(src, None, false);
        assert_eq!(styled.source(), src);
        let spans: Vec<_> = styled.spans().collect();
        assert_eq!(spans.len(), 2);
        let highlighted = |s: &cursive::utils::span::Span<'_, cursive::style::Style>| {
            s.attr.color != cursive::style::ColorStyle::inherit_parent()
        };
        assert!(!highlighted(&spans[0]));
        assert!(highlighted(&spans[1]));
        // With the censor flag on, censoring applies in the same pass.
        let censored = render_output("ping 10.0.0.1\n", None, true);
        assert_eq!(censored.source(), format!("ping {CENSOR}\n"));
    }

    #[test]
    fn status_lines_real_world_cisco_syslog() {
        // Interface state changes (the requested case) plus other common IOS lines.
        assert!(is_status_line("%LINK-3-UPDOWN: Interface GigabitEthernet0/1, changed state to down"));
        assert!(is_status_line("%LINEPROTO-5-UPDOWN: Line protocol on Interface Vlan1, changed state to up"));
        assert!(is_status_line("%LINK-5-CHANGED: Interface GigabitEthernet0/1, changed state to administratively down"));
        assert!(is_status_line("*Nov  3 14:14:15.001: %LINK-3-UPDOWN: Interface GigabitEthernet0/1, changed state to down"));
        assert!(is_status_line("%SYS-5-CONFIG_I: Configured from console by admin on vty0"));
        assert!(is_status_line("%SYS-5-RELOAD: Reload requested by admin"));
        assert!(is_status_line("%DUAL-5-NBRCHANGE: IP-EIGRP 1: Neighbor 10.1.1.1 (Serial0/0) is down"));
        assert!(is_status_line("%OSPF-5-ADJCHG: Process 1, Nbr 10.2.2.2 on GigabitEthernet0/0 from LOADING to FULL, Loading Done"));
        assert!(is_status_line("%BGP-5-ADJCHANGE: neighbor 10.3.3.3 Down BGP Notification sent"));
        assert!(is_status_line("%SEC-6-IPACCESSLOGS: list 10 denied 10.4.4.4 1 packet"));
        // Prompt glued directly to the message ('#' is a valid boundary).
        assert!(is_status_line("router#%LINK-3-UPDOWN: Interface Ethernet0, changed state to up"));
        // Severity boundaries 0 (emergencies) and 7 (debugging) are valid.
        assert!(is_status_line("%SYS-0-SHUTDOWN: The system is being shut down"));
        assert!(is_status_line("%LINK-7-UPDOWN: Interface Ethernet0 debug trace"));
    }

    #[test]
    fn status_lines_reject_non_syslog_percent() {
        // Severity is a single digit 0-7; 8+ does not exist in IOS.
        assert!(!is_status_line("%LINK-8-UPDOWN: Interface Ethernet0"));
        // '%' glued to a word is not a syslog token.
        assert!(!is_status_line("foo%LINK-3-UPDOWN: Interface Ethernet0"));
        // Facility and mnemonic must start with a letter.
        assert!(!is_status_line("%3-4-5: not a facility"));
        assert!(!is_status_line("%LINK-3-5UP: not a mnemonic"));
        // IOS error/help text is not a status line.
        assert!(!is_status_line("% No matching commands"));
        assert!(!is_status_line("% Invalid input detected at '^' marker"));
        assert!(!is_status_line("% Ambiguous command: \"show\""));
        assert!(!is_status_line("100% done"));
        assert!(!is_status_line("router# show version"));
    }

    #[test]
    fn render_output_colors_status_and_censors_ip_in_one_line() {
        let src = "%DUAL-5-NBRCHANGE: IP-EIGRP 1: Neighbor 10.1.1.1 (Serial0/0) is down\n";
        let styled = render_output(src, None, true);
        assert!(!styled.source().contains("10.1.1.1"));
        assert!(styled.source().contains(CENSOR));
        let spans: Vec<_> = styled.spans().collect();
        assert_eq!(spans.len(), 1);
        assert!(spans[0].attr.color != cursive::style::ColorStyle::inherit_parent());
    }

    #[test]
    fn mouse_drag_selects_scrollback_rows() {
        let (mut runner, _buffer) = test_runner();
        let lines = |n: usize| (0..n).map(|i| format!("line {i}\n")).collect::<String>();
        set_terminal_output(&mut runner, lines(40));
        runner.refresh();
        // Press in the middle of the output area, drag down 3 rows, release.
        runner.on_event(Event::Mouse {
            event: MouseEvent::Press(MouseButton::Left),
            position: Vec2::new(40, 10),
            offset: Vec2::zero(),
        });
        runner.on_event(Event::Mouse {
            event: MouseEvent::Hold(MouseButton::Left),
            position: Vec2::new(40, 13),
            offset: Vec2::zero(),
        });
        runner.on_event(Event::Mouse {
            event: MouseEvent::Release(MouseButton::Left),
            position: Vec2::new(40, 13),
            offset: Vec2::zero(),
        });

        let (sel, text) = runner
            .call_on_name("terminal_scroll", |v: &mut ScrollMouse| {
                (v.selection, v.selected_text())
            })
            .unwrap();
        let (a, c) = sel.expect("drag must leave an active selection");
        assert_eq!(c.y - a.y, 3, "drag must span 3 rows, got {a:?}..{c:?}");
        let expected: String = (a.y..=c.y).map(|i| format!("line {i}\n")).collect();
        assert_eq!(text, expected);

        // A plain click (press + release at the same spot) clears the selection
        // instead of copying a single row.
        runner.on_event(Event::Mouse {
            event: MouseEvent::Press(MouseButton::Left),
            position: Vec2::new(40, 10),
            offset: Vec2::zero(),
        });
        runner.on_event(Event::Mouse {
            event: MouseEvent::Release(MouseButton::Left),
            position: Vec2::new(40, 10),
            offset: Vec2::zero(),
        });
        let sel = runner
            .call_on_name("terminal_scroll", |v: &mut ScrollMouse| v.selection)
            .unwrap();
        assert_eq!(sel, None, "plain click must clear the selection");
    }
}

