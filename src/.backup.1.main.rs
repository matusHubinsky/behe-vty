use cursive::views::{TextView, EditView, LinearLayout, ResizedView, ScrollView};
use cursive::traits::*;
use cursive::{Cursive, CursiveExt};
use std::rc::Rc;
use std::cell::RefCell;

fn main() {
    let mut siv = Cursive::default();

    // Shared buffer for terminal output
    let output_buffer = Rc::new(RefCell::new(String::from("Cisco Terminal Emulator\n")));

    // TextView to show terminal output
    let text_view = TextView::new(output_buffer.borrow().as_str())
        .with_name("terminal_output");

    // Scrollable output area
    let terminal_output = ScrollView::new(text_view)
        .scroll_strategy(cursive::view::ScrollStrategy::StickToBottom)
        .full_width();

    // Input field with fixed width so user can type
    let input_field = EditView::new()
        .on_submit({
            let output_buffer = output_buffer.clone();
            move |s, input| {
                let mut output = output_buffer.borrow_mut();
                output.push_str(&format!("Router#{}\n", input));

                // Simulate response
                if input.trim() == "enable" {
                    output.push_str("Entered privileged EXEC mode.\n");
                }

                s.call_on_name("terminal_output", |view: &mut TextView| {
                    view.set_content(output.clone());
                });

                s.call_on_name("input", |view: &mut EditView| {
                    view.set_content("");
                });
            }
        })
        .with_name("input")
        .fixed_width(72); // 80 total - 8 for "Router#" prompt

    // Combine "Router#" + input on same line
    let input_line = LinearLayout::horizontal()
        .child(TextView::new("Router#"))
        .child(input_field);

    // Terminal layout: output + input line
    let layout = LinearLayout::vertical()
        .child(ResizedView::with_fixed_size((80, 18), terminal_output))
        .child(ResizedView::with_fixed_size((80, 1), input_line));

    // Add the terminal window as fixed 80x20
    siv.add_layer(ResizedView::with_fixed_size((80, 20), layout));

    siv.run();
}

