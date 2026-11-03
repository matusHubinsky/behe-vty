use cursive::views::{Dialog, TextView, EditView, LinearLayout};
use cursive::traits::*;
use cursive::{Cursive, CursiveExt};
use std::rc::Rc;
use std::cell::RefCell;

fn main() {
    let mut siv = Cursive::default();

    let output_buffer = Rc::new(RefCell::new(String::new()));

    let text_view = TextView::new("Cisco Terminal\n")
        .with_name("terminal_output")
        .scrollable();

    let input = EditView::new()
        .on_submit({
            let output_buffer = output_buffer.clone();
            move |s, input| {
                let mut output = output_buffer.borrow_mut();
                output.push_str(&format!("> {}\n", input));
                s.call_on_name("terminal_output", |view: &mut TextView| {
                    view.set_content(output.clone());
                });
                s.call_on_name("input", |view: &mut EditView| {
                    view.set_content("");
                });
            }
        })
        .with_name("input")
        .fixed_width(50);

    let layout = LinearLayout::vertical()
        .child(text_view)
        .child(input);

    siv.add_layer(Dialog::around(layout)
        .title("Terminal Emulator")
        .button("Quit", |s| s.quit()));

    siv.run();
}

