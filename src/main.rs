use std::process::Command;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Claude,
    GitDif,
    GitWorktree,
}

impl Pane {
    fn next(self) -> Self {
        match self {
            Pane::Claude => Pane::GitDif,
            Pane::GitDif => Pane::GitWorktree,
            Pane::GitWorktree => Pane::Claude,
        }
    }

    fn prev(self) -> Self {
        match self {
            Pane::Claude => Pane::GitWorktree,
            Pane::GitDif => Pane::Claude,
            Pane::GitWorktree => Pane::GitDif,
        }
    }
}

struct App {
    focused: Pane,
    should_quit: bool,
    fullscreen: bool,
    git_log: String,
    git_worktrees: String,
}

impl App {
    fn new() -> Self {
        let mut app = Self {
            focused: Pane::Claude,
            should_quit: false,
            fullscreen: false,
            git_log: String::new(),
            git_worktrees: String::new(),
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.git_log = run_command("git", &["log", "--oneline", "--graph", "--decorate", "-50"]);
        self.git_worktrees = run_command("git", &["worktree", "list"]);
    }
}

fn run_command(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            if out.is_empty() && !err.is_empty() { err } else { out }
        })
        .unwrap_or_else(|e| format!("error: {e}"))
}

fn main() -> std::io::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
    let mut app = App::new();
    while !app.should_quit {
        terminal.draw(|frame| render(&app, frame))?;
        handle_events(&mut app)?;
    }
    Ok(())
}

fn handle_events(app: &mut App) -> std::io::Result<()> {
    match event::read()? {
        Event::Key(KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers,
            ..
        }) => match code {
            KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
            KeyCode::Char('f') if modifiers.contains(KeyModifiers::CONTROL) => app.fullscreen = !app.fullscreen,
            KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) => app.refresh(),
            KeyCode::BackTab => app.focused = app.focused.prev(),
            KeyCode::Tab => app.focused = app.focused.next(),
            _ => {}
        },
        _ => {}
    }
    Ok(())
}

fn render(app: &App, frame: &mut Frame) {
    let normal_style = Style::default().fg(Color::Blue);
    let focused_style = Style::default()
        .fg(Color::Blue)
        .add_modifier(Modifier::BOLD);

    if app.fullscreen {
        let title = match app.focused {
            Pane::Claude => "Claude Code",
            Pane::GitDif => "Git Log",
            Pane::GitWorktree => "Git Worktrees",
        };
        let area = frame.area();
        let block = pane_block(title, true, normal_style, focused_style);
        match app.focused {
            Pane::GitDif => frame.render_widget(
                Paragraph::new(app.git_log.as_str()).block(block),
                area,
            ),
            Pane::GitWorktree => frame.render_widget(
                Paragraph::new(app.git_worktrees.as_str()).block(block),
                area,
            ),
            Pane::Claude => frame.render_widget(block, area),
        }
        return;
    }

    let [left_area, right_area] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .areas(frame.area());

    let [top_right_area, bottom_right_area] = Layout::vertical([
        Constraint::Percentage(50),
        Constraint::Percentage(50),
    ])
    .areas(right_area);

    frame.render_widget(
        pane_block("Claude Code", app.focused == Pane::Claude, normal_style, focused_style),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(app.git_log.as_str())
            .block(pane_block("Git Log", app.focused == Pane::GitDif, normal_style, focused_style)),
        top_right_area,
    );
    frame.render_widget(
        Paragraph::new(app.git_worktrees.as_str())
            .block(pane_block("Git Worktrees", app.focused == Pane::GitWorktree, normal_style, focused_style)),
        bottom_right_area,
    );
}

fn pane_block(title: &str, focused: bool, normal: Style, active: Style) -> Block<'_> {
    let border_style = if focused { active } else { normal };
    let label = if focused {
        format!(" {} * ", title)
    } else {
        format!(" {} ", title)
    };
    Block::bordered().title(label).border_style(border_style)
}
