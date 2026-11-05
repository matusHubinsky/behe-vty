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
use cursive::view::{Margins, SizeConstraint};
use cursive::views::{Dialog, LinearLayout, PaddedView, ResizedView, ScrollView, TextView};
use cursive::{Cursive, CursiveExt, Printer, Vec2};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::cisco;
use crate::commands::CommandTree;
use crate::database::Database;
use crate::network;

const INPUT_WIDTH: usize = 72;

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

const SCHEMES: [Scheme; 4] = [
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
        borders: BorderStyle::Simple,
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
    terminal: Arc<Mutex<cisco::Terminal>>,
    history: Arc<Mutex<History>>,
    output_buffer: Arc<Mutex<String>>,
    scheme: Arc<AtomicUsize>,
    commands: Arc<CommandTree>,
    database: Arc<Mutex<Database>>,
}

impl InputView {
    fn new(
        terminal: Arc<Mutex<cisco::Terminal>>,
        history: Arc<Mutex<History>>,
        output_buffer: Arc<Mutex<String>>,
        scheme: Arc<AtomicUsize>,
        commands: Arc<CommandTree>,
        database: Arc<Mutex<Database>>,
    ) -> Self {
        Self {
            content: String::new(),
            cursor: 0,
            ghost: String::new(),
            terminal,
            history,
            output_buffer,
            scheme,
            commands,
            database,
        }
    }

    fn set_content(&mut self, text: String) {
        self.cursor = text.len();
        self.content = text;
        self.update_ghost();
    }

    fn matches(&self) -> Vec<String> {
        let term = self.terminal.lock().unwrap();
        self.commands.autocomplete(&term.mode, &self.content)
    }

    fn update_ghost(&mut self) {
        // Only suggest when the cursor is at the end of the line.
        if self.content.is_empty() || self.cursor != self.content.len() {
            self.ghost.clear();
            return;
        }

        let matches = self.matches();

        self.ghost = if matches.len() == 1 {
            matches[0]
                .strip_prefix(self.content.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            // Several candidates: show the shared remainder, if any.
            let prefix = longest_common_prefix(&matches);
            prefix
                .strip_prefix(self.content.as_str())
                .unwrap_or("")
                .to_string()
        };
    }

    fn accept_ghost(&mut self) -> bool {
        if self.ghost.is_empty() {
            return false;
        }

        let ghost = std::mem::take(&mut self.ghost);
        self.content.push_str(&ghost);
        self.cursor = self.content.len();
        self.update_ghost();
        true
    }

    /// TAB: complete fully if there is one match, otherwise extend to the
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

        self.history.lock().unwrap().add_line(input.clone());
        let _ = self.database.lock().unwrap().add_history("local", &input);

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

        // Background across the whole field.
        printer.with_color(normal, |p| {
            p.print_hline((0, 0), printer.size.x, " ");
        });

        // Horizontal scroll so the cursor stays visible.
        let chars: Vec<char> = self.content.chars().collect();
        let cursor_col = self.content[..self.cursor].chars().count();
        let width = printer.size.x.saturating_sub(1).max(1);
        let offset = cursor_col.saturating_sub(width);

        let visible: String = chars.iter().skip(offset).take(width).collect();
        printer.with_color(normal, |p| p.print((0, 0), &visible));

        // Ghost text right after the typed text.
        let typed_cols = chars.len().saturating_sub(offset);

        if !self.ghost.is_empty() && typed_cols < width {
            let room = width - typed_cols;
            let g: String = self.ghost.chars().take(room).collect();
            printer.with_color(ghost, |p| p.print((typed_cols, 0), &g));
        }

        // Cursor.
        if printer.focused {
            let col = cursor_col - offset;

            let under: String = if cursor_col < chars.len() {
                chars[cursor_col].to_string()
            } else if let Some(first) = self.ghost.chars().next() {
                // Cursor sits on the first ghost character.
                first.to_string()
            } else {
                " ".to_string()
            };

            printer.with_color(cur, |p| p.print((col, 0), &under));
        }
    }

    fn required_size(&mut self, _constraint: Vec2) -> Vec2 {
        Vec2::new(INPUT_WIDTH, 1)
    }

    fn take_focus(
        &mut self,
        _: cursive::direction::Direction,
    ) -> Result<EventResult, cursive::view::CannotFocus> {
        Ok(EventResult::consumed())
    }

    fn on_event(&mut self, event: Event) -> EventResult {
        match event {
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

            Event::Key(Key::Up) => {
                self.history_move(true);
                EventResult::consumed()
            }

            Event::Key(Key::Down) => {
                self.history_move(false);
                EventResult::consumed()
            }

            Event::Key(Key::Enter) => self.submit(),

            _ => EventResult::Ignored,
        }
    }
}

/// Dialog chrome: 2 border cols + 2 padding cols; 2 border rows + 2 padding
/// rows + 1 title row. Clamps to sane minimums (screen_size() is (0,0) before
/// the first layout phase).
fn content_size(screen: Vec2) -> (usize, usize) {
    (
        screen.x.saturating_sub(4).max(40),
        screen.y.saturating_sub(5).max(10),
    )
}

/// Recompute the fixed sizes of the root/output/input boxes from the window.
fn apply_window_size(s: &mut Cursive) {
    let (w, h) = content_size(s.screen_size());
    let out_h = h.saturating_sub(2).max(1);
    s.call_on_name("root", move |v: &mut ResizedView<LinearLayout>| {
        v.set_constraints(SizeConstraint::Fixed(w), SizeConstraint::Fixed(h));
    });
    s.call_on_name("output_box", move |v: &mut ResizedView<ScrollView<TextView>>| {
        v.set_constraints(SizeConstraint::Fixed(w), SizeConstraint::Fixed(out_h));
    });
    s.call_on_name("input_box", move |v: &mut ResizedView<LinearLayout>| {
        v.set_constraints(SizeConstraint::Fixed(w), SizeConstraint::Fixed(2));
    });
}

// ---------------------------------------------------------------- run

pub fn run() {
    let mut siv = Cursive::default();

    let database = Arc::new(Mutex::new(
        Database::open().expect("cannot open database"),
    ));
    let commands = Arc::new(CommandTree::load());

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
        prompt: "router>".to_string(),
        input: String::new(),
        output: String::new(),
        end: false,
    }));

    let output_buffer = Arc::new(Mutex::new(String::new()));

    let text_view = TextView::new("")
        .style(ColorStyle::new(initial.fg, initial.bg))
        .with_name("terminal_output");

    let terminal_output = ScrollView::new(text_view)
        .scroll_strategy(cursive::view::ScrollStrategy::StickToBottom)
        .full_width();

    let input_view = InputView::new(
        Arc::clone(&terminal),
        Arc::clone(&history),
        Arc::clone(&output_buffer),
        Arc::clone(&scheme_index),
        Arc::clone(&commands),
        Arc::clone(&database),
    );

    let initial_prompt = terminal.lock().unwrap().prompt.clone();

    let prompt_view = TextView::new(initial_prompt)
        .style(ColorStyle::new(initial.prompt, initial.bg))
        .with_name("prompt");

    let padded_prompt_view = PaddedView::new(Margins::lrtb(0, 1, 0, 0), prompt_view);

    let input_line = LinearLayout::horizontal()
        .child(padded_prompt_view)
        .child(input_view.with_name("input"));

    let layout = LinearLayout::vertical()
        .child(ResizedView::with_fixed_size((80, 18), terminal_output))
        .child(ResizedView::with_fixed_size((80, 2), input_line));

    siv.add_layer(
        Dialog::around(ResizedView::with_fixed_size((80, 20), layout))
            .title("behe-vty | TAB: complete | Ctrl+T: theme")
            .padding(Margins::lrtb(1, 1, 1, 1)),
    );

    apply_scheme(&mut siv, 0);

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

    siv.add_global_callback(Event::WindowResize, |s| apply_window_size(s));
    apply_window_size(&mut siv);

    siv.run();
}