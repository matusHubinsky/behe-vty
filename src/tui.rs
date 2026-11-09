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

use cursive::event::{Event, EventResult, Key};
use cursive::theme::{Color, ColorStyle};
use cursive::traits::*;
use cursive::view::Margins;
use cursive::views::{Dialog, EditView, LinearLayout, PaddedView, ResizedView, ScrollView, SelectView, TextView};
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
const BASE_TITLE: &str = "BEHE-VTY | TAB: complete";
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
    let title = match session {
        Some(desc) => {
            format!("BEHE-VTY | {desc}")
        }
        None => BASE_TITLE.to_string(),
    };
    s.call_on_name("app_dialog", |d: &mut Dialog| d.set_title(title));
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

    s.call_on_name("terminal_output", |v: &mut TextView| {
        v.set_content(snapshot);
    });
}

// ---------------------------------------------------------------- input view

/// Single-line input that draws an inline grey suggestion right after the
/// typed text.
struct InputView {
    content: String,
    cursor: usize, // byte index, always on a char boundary
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

        for (row, line) in lines.iter().enumerate() {
            if row < row_offset {
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

            let visible: String = chars.iter().skip(col_offset).take(width).collect();
            printer.with_color(normal, |p| p.print((0, y), &visible));

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
        match event {
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
        sessions.add_item(format!("{:<14} {}:{}", d.name, d.host, d.port), d);
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
    let name = if host.is_empty() { "adhoc".to_string() } else { host.clone() };

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
            s.call_on_name("terminal_output", |v: &mut TextView| {
                v.set_content(snapshot);
            });
        } else if !line.is_empty() {
            // Only the pending line changed (bare prompt): re-render so the
            // scrollback shows it even before the first Enter.
            let snapshot = {
                let out = output_buffer.lock().unwrap();
                out.display(&line)
            };
            s.call_on_name("terminal_output", |v: &mut TextView| {
                v.set_content(snapshot);
            });
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

    let terminal_output =
        ScrollView::new(text_view).scroll_strategy(cursive::view::ScrollStrategy::StickToBottom);

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
}