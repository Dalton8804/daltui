mod app;
mod events;
mod git;
mod pty;
mod ui;
mod util;

fn main() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = app::App::run(&mut terminal);
    ratatui::restore();
    result
}
