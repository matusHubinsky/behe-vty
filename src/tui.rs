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
use cursive::theme::{
    BorderStyle, Color, ColorStyle, Palette, PaletteColor, Theme as CursiveTheme,
};
use cursive::traits::*;
use cursive::view::Margins;
use cursive::views::{Dialog, EditView, LinearLayout, PaddedView, ResizedView, ScrollView, SelectView, TextView};
use cursive::{Cursive, CursiveExt, Printer, Vec2};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::cisco;
use crate::commands::{command_matches, CommandTree, Vendor};
use crate::database::{Database, Device};
use crate::networking;
use crate::vault;

const INPUT_WIDTH: usize = 72;
const CURSOR_FPS: u32 = 24;

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

const SCHEMES: [Scheme; 5] = [
    Scheme {
        name: "Ubuntu",
        bg: Color::Rgb(0x30, 0x0a, 0x24),     // #300A24 aubergine background
        fg: Color::Rgb(0xff, 0xff, 0xff),     // white text
        prompt: Color::Rgb(0xe9, 0x54, 0x20), // #E95420 ubuntu orange (prompt/titles)
        ghost: Color::Rgb(0x8a, 0x6a, 0x7a),  // muted mauve (autocomplete ghost)
        input_bg: Color::Rgb(0x30, 0x0a, 0x24),
        cursor: Color::Rgb(0xff, 0xff, 0xff), // white blinking block
    },
    Scheme {
        name: "Dark Purple",
        bg: Color::Rgb(0x2a, 0x2a, 0x2a),
        fg: Color::Rgb(0xee, 0xee, 0xee),
        prompt: Color::Rgb(0x8a, 0x2b, 0xe2),
        ghost: Color::Rgb(0x75, 0x75, 0x75),
        input_bg: Color::Rgb(0x2a, 0x2a, 0x2a),
        cursor: Color::Rgb(0xd0, 0xd0, 0xd0),
    },
    Scheme {
        name: "Classic Green",
        bg: Color::Rgb(0x00, 0x00, 0x00),
        fg: Color::Rgb(0x33, 0xff, 0x33),
        prompt: Color::Rgb(0x00, 0xcc, 0x00),
        ghost: Color::Rgb(0x1f, 0x7a, 0x1f),
        input_bg: Color::Rgb(0x00, 0x00, 0x00),
        cursor: Color::Rgb(0x33, 0xff, 0x33),
    },
    Scheme {
        name: "Amber",
        bg: Color::Rgb(0x1a, 0x10, 0x00),
        fg: Color::Rgb(0xff, 0xb0, 0x00),
        prompt: Color::Rgb(0xff, 0x8c, 0x00),
        ghost: Color::Rgb(0x8a, 0x60, 0x10),
        input_bg: Color::Rgb(0x1a, 0x10, 0x00),
        cursor: Color::Rgb(0xff, 0xb0, 0x00),
    },
    Scheme {
        name: "Light",
        bg: Color::Rgb(0xfa, 0xfa, 0xfa),
        fg: Color::Rgb(0x22, 0x22, 0x22),
        prompt: Color::Rgb(0x5b, 0x21, 0xb6),
        ghost: Color::Rgb(0x9a, 0x9a, 0x9a),
        input_bg: Color::Rgb(0xe8, 0xe8, 0xe8),
        cursor: Color::Rgb(0x22, 0x22, 0x22),
    },
];

fn cursive_theme(scheme: &Scheme) -> CursiveTheme {
    let mut palette = Palette::default();
    palette[PaletteColor::Background] = scheme.bg;
    palette[PaletteColor::View] = scheme.bg;
    palette[PaletteColor::Primary] = scheme.fg;
    palette[PaletteColor::Secondary] = scheme.fg;
    palette[PaletteColor::Tertiary] = scheme.fg;
    palette[PaletteColor::TitlePrimary] = scheme.prompt;
    palette[PaletteColor::TitleSecondary] = scheme.prompt;
    palette[PaletteColor::Highlight] = scheme.prompt;
    palette[PaletteColor::HighlightInactive] = scheme.ghost;
    palette[PaletteColor::HighlightText] = scheme.bg;
    palette[PaletteColor::Shadow] = scheme.bg;

    CursiveTheme {
        shadow: false,
        borders: BorderStyle::Simple, // outline always on
        palette,
    }
}

/// Apply a scheme to the whole UI.
fn apply_scheme(s: &mut Cursive, index: usize) {
    let scheme = SCHEMES[index];

    s.set_theme(cursive_theme(&scheme));

    s.call_on_name("terminal_output", |v: &mut TextView| {
        v.set_style(ColorStyle::new(scheme.fg, scheme.bg));
    });
    s.call_on_name("prompt", |v: &mut TextView| {
        v.set_style(ColorStyle::new(scheme.prompt, scheme.bg));
    });
}

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

fn append_output(s: &mut Cursive, buffer: &Arc<Mutex<String>>, text: &str) {
    let snapshot = {
        let mut out = buffer.lock().unwrap();
        out.push_str(text);
        out.clone()
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
    output_buffer: Arc<Mutex<String>>,
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
        output_buffer: Arc<Mutex<String>>,
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
                        // The input line shows a static session label; remote
                        // output (incl. Cisco status messages) stays in the scrollback.
                        s.call_on_name("prompt", |v: &mut TextView| {
                            v.set_content(format!("{desc} "));
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
        let sc = SCHEMES[self.scheme.load(Ordering::Relaxed)];
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

            // UPDATED: TAB and right key works same for autocomplete
            Event::Key(Key::Right) | Event::Key(Key::Tab) => {
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

            // Ctrl+V: toggle multi-line input (Enter then inserts a newline).
            Event::CtrlChar('v') => {
                self.multiline = !self.multiline;
                self.update_ghost();
                EventResult::consumed()
            }

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
    device: String,
    username: String,
    password: String,
) {
    let master2 = Arc::clone(&master);
    let db2 = Arc::clone(&db);

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
                if let Ok(id) = db2.lock().unwrap().save_credential(&pass, &username, &password) {
                    let _ = db2.lock().unwrap().link_credential(&device, id);
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
    output_buffer: &Arc<Mutex<String>>,
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
                                        if let Ok(id) = db2.lock().unwrap().save_credential(&master, &user, &pass) {
                                            let _ = db2.lock().unwrap().link_credential(&name, id);
                                        }
                                    }
                                    (None, false) => set_master_dialog(
                                        s,
                                        Arc::clone(&master3),
                                        Arc::clone(&db2),
                                        name.clone(),
                                        user.clone(),
                                        pass.clone(),
                                    ),
                                    _ => {}
                                }
                                // Switch the autocomplete tree to the selected vendor.
                                *cmds2.write().unwrap() = CommandTree::load(vendor);
                                append_output(s, &out2, &format!("% {desc} connected — session active (type 'disconnect' to close)\n"));
                                s.call_on_name("prompt", |v: &mut TextView| {
                                    v.set_content(format!("{desc} "));
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
            })
            .button("Local simulator", |s| {
                s.pop_layer();
            })
            .button("Quit", |s| s.quit()),
    );
}

// ---------------------------------------------------------------- run

/// Bridges the session thread to the UI: drain new device output into the
/// scrollback, keep the prompt view on the device's current line, and drop
/// the session when the remote closes it.
fn make_notifier(
    sink: cursive::CbSink,
    output_buffer: Arc<Mutex<String>>,
    session: Arc<Mutex<Option<networking::RemoteSession>>>,
    terminal: Arc<Mutex<cisco::Terminal>>,
) -> networking::Notify {
    // Coalesce + throttle: at most one UI refresh in flight and at most ~20/s,
    // so a chatty telnet/ssh stream cannot flood the event loop with a full
    // scrollback re-render per chunk.
    let pending = Arc::new(AtomicBool::new(false));
    let last_sent = Arc::new(Mutex::new(std::time::Instant::now() - std::time::Duration::from_secs(1)));
    Arc::new(move || {
        if pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let closed = session
            .lock()
            .unwrap()
            .as_ref()
            .map(|sess| sess.is_closed())
            .unwrap_or(false);
        {
            let mut last = last_sent.lock().unwrap();
            let now = std::time::Instant::now();
            if !closed && now.duration_since(*last) < std::time::Duration::from_millis(50) {
                pending.store(false, Ordering::SeqCst);
                return;
            }
            *last = now;
        }
        let output_buffer = Arc::clone(&output_buffer);
        let session = Arc::clone(&session);
        let terminal = Arc::clone(&terminal);
        let pending = Arc::clone(&pending);
        let sink = sink.clone();
        let _ = sink.send(Box::new(move |s: &mut Cursive| {
            pending.store(false, Ordering::SeqCst);
            let mut text = String::new();
            let mut closed_status = None;
            {
                let mut guard = session.lock().unwrap();
                if let Some(sess) = guard.as_ref() {
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
                    out.push_str(&text);
                    out.clone()
                };
                s.call_on_name("terminal_output", |v: &mut TextView| {
                    v.set_content(snapshot);
                });
            }
            if let Some(status) = closed_status {
                append_output(
                    s,
                    &output_buffer,
                    &format!("% session closed by remote host ({status})\n"),
                );
                let prompt = terminal.lock().unwrap().prompt.clone();
                s.call_on_name("prompt", |v: &mut TextView| {
                    v.set_content(prompt);
                });
            }
        }));
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

    let output_buffer = Arc::new(Mutex::new(String::new()));
    let session: Arc<Mutex<Option<networking::RemoteSession>>> = Arc::new(Mutex::new(None));
    let notify = make_notifier(
        siv.cb_sink().clone(),
        Arc::clone(&output_buffer),
        Arc::clone(&session),
        Arc::clone(&terminal),
    );

    let text_view = TextView::new("██████╗ ███████╗██╗  ██╗███████╗    ██╗   ██╗████████╗██╗   ██╗
██╔══██╗██╔════╝██║  ██║██╔════╝    ██║   ██║╚══██╔══╝╚██╗ ██╔╝
██████╔╝█████╗  ███████║█████╗█████╗██║   ██║   ██║    ╚████╔╝ 
██╔══██╗██╔══╝  ██╔══██║██╔══╝╚════╝╚██╗ ██╔╝   ██║     ╚██╔╝  
██████╔╝███████╗██║  ██║███████╗     ╚████╔╝    ██║      ██║   
╚═════╝ ╚══════╝╚═╝  ╚═╝╚══════╝      ╚═══╝     ╚═╝      ╚═╝ 
*
*
*
*
*
*
")
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

    let initial_prompt = terminal.lock().unwrap().prompt.clone();

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
            .title("BEHE-VTY | TAB: complete | Ctrl+V: multiline | Ctrl+Z: send block")
            .padding(Margins::lrtb(1, 1, 1, 1)), // keep padding; no title bar = max rows
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

