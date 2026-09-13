// Windows opens a console behind a program unless it says otherwise. Debug
// builds keep it, so the dev hooks and panics still have somewhere to print.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> iced::Result {
    mimirdlp::gui::run()
}
