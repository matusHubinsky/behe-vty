use cursive::views::{TextView, EditView, LinearLayout, ResizedView, ScrollView, Dialog, PaddedView};
use cursive::traits::*;
use cursive::theme::{Color, ColorStyle, BaseColor};
use cursive::{Cursive, CursiveExt};
use cursive::view::Margins;
use cursive::event::Key;
use std::rc::Rc;
use std::cell::RefCell;

use crate::cisco;

struct History {
    lines: Vec<String>,
    index: usize,
}

// TODO: history should ingnore "\n" and ""
impl History {
    // Create a new History instance
    fn new() -> Self {
        History {
            lines: Vec::new(),
            index: 0,
        }
    }

    // Add a new line to the history
    fn add_line(&mut self, line: String) {
        self.lines.push(line);
        self.index = self.lines.len(); // Move index to the end
    }

    // Get the current line from history
    fn current_line(&self) -> String {
        if self.index < self.lines.len() {
            self.lines[self.index].clone()
        } else {
            String::new()
        }
    }

    // Move to the previous line in history
    fn previous(&mut self) {
        if self.index > 0 {
            self.index -= 1;
        }
    }

    // Move to the next line in history
    fn next(&mut self) {
        if self.index < self.lines.len() {
            self.index += 1;
        }
    }
}

pub fn run() {
    let mut siv = Cursive::default();

    let terminal = Rc::new(RefCell::new(cisco::Terminal {
        mode: cisco::CiscoTerminalMode::UserExec,
        prompt: "router>".to_string(),
        input: "".to_string(),
        output: "".to_string(),
        end: false,
    }));
    
    siv.load_toml(include_str!(".././theme.toml")).unwrap();

    let output_buffer = Rc::new(RefCell::new(String::from("")));
    let history = Rc::new(RefCell::new(History::new()));

    let text_view = TextView::new(output_buffer.borrow().as_str())
        .style(ColorStyle::new(
            Color::Light(BaseColor::White),
            Color::Rgb(0x2a, 0x2a, 0x2a),
        ))
        .with_name("terminal_output");

    let terminal_output = ScrollView::new(text_view)
        .scroll_strategy(cursive::view::ScrollStrategy::StickToBottom)
        .full_width();

    let input_field = EditView::new()
        .style(ColorStyle::new(
            Color::Rgb(0x2a, 0x2a, 0x2a),
            Color::Light(BaseColor::White),
        ))
        .on_submit({
            let output_buffer = output_buffer.clone();
            let terminal = Rc::clone(&terminal); // Clone the Rc to capture by reference
            let history = Rc::clone(&history);
            move |s, input| {
                let mut output = output_buffer.borrow_mut();
                let mut term = terminal.borrow_mut();

                term.input = input.to_string();
                cisco::terminal(&mut term);
                
                if term.end == true {
                    s.quit();
                } 

                output.push_str(&format!("{} {}\n", term.prompt, term.output));
                s.call_on_name("terminal_output", |view: &mut TextView| {
                    view.set_content(output.clone());
                });
                s.call_on_name("input", |view: &mut EditView| {
                    view.set_content("");
                });
                s.call_on_name("prompt", |view: &mut TextView| {
                    view.set_content(term.prompt.clone());
                });

                // Add the input to history
                history.borrow_mut().add_line(input.to_string());
            }
        })
        .with_name("input")
        .fixed_width(72);
    let prompt_view = TextView::new(terminal.borrow().prompt.clone())
        .style(ColorStyle::new(
            Color::Rgb(0x8a, 0x2b, 0xe2),
            Color::Rgb(0x2a, 0x2a, 0x2a),
        ))
        .with_name("prompt");

    let padded_prompt_view = PaddedView::new(Margins::lrtb(0, 1, 0, 0), prompt_view); // Add padding to the right of the prompt

    let input_line = LinearLayout::horizontal()
        .child(padded_prompt_view)
        .child(input_field);

    let layout = LinearLayout::vertical()
        .child(ResizedView::with_fixed_size((80, 18), terminal_output))
        .child(ResizedView::with_fixed_size((80, 2), input_line));

    siv.add_layer(
        Dialog::around(ResizedView::with_fixed_size((80, 20), layout))
            .title("Cisco Router Terminal")
            .padding(Margins::lrtb(1, 1, 1, 1)),
    );

    // Clone history for the second callback
    let history_clone = Rc::clone(&history);

    // Add event handler for history navigation
    siv.add_global_callback(Key::Up, move |s| {
        let mut history = history.borrow_mut();
        history.previous();
        let current_line = history.current_line();
        s.call_on_name("input", |view: &mut EditView| {
            view.set_content(current_line);
        });
    });

    siv.add_global_callback(Key::Down, move |s| {
        let mut history = history_clone.borrow_mut();
        history.next();
        let current_line = history.current_line();
        s.call_on_name("input", |view: &mut EditView| {
            view.set_content(current_line);
        });
    });

    siv.run();
}