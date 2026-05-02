use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Clear, Paragraph, Tabs};
use ratatui::Frame;

use crate::app::{App, InputMode, Pane};
use crate::ui::git_pane::render_git_pane;
use crate::ui::pty_pane::{render_claude_pane, render_terminal_pane};

pub fn render(app: &App, frame: &mut Frame) {
    let [winbar_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(frame.area());

    render_window_bar(app, frame, winbar_area);

    let win = app.win();
    let normal = Style::default().fg(Color::Blue);
    let active = Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD);

    if win.fullscreen {
        render_git_pane(
            win,
            frame,
            content_area,
            win.focused == Pane::Git,
            normal,
            active,
            &app.open_paths(),
        );
        return;
    }

    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .areas(content_area);
    let [git_area, term_area] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(right);

    render_claude_pane(win, frame, left, win.focused == Pane::Claude, normal, active);
    render_git_pane(
        win,
        frame,
        git_area,
        win.focused == Pane::Git,
        normal,
        active,
        &app.open_paths(),
    );
    render_terminal_pane(win, frame, term_area, win.focused == Pane::Terminal, normal, active);

    if app.input_mode.is_some() {
        render_input_prompt(app, frame);
    }
}

fn render_window_bar(app: &App, frame: &mut Frame, area: Rect) {
    let names: Vec<String> = app.windows.iter().map(|w| format!(" {} ", w.name)).collect();
    let tabs = Tabs::new(names)
        .select(app.active)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
        .divider("│");
    frame.render_widget(tabs, area);

    let title = Paragraph::new("datui")
        .style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center);
    frame.render_widget(title, area);
}

fn render_input_prompt(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let w = 54u16.min(area.width.saturating_sub(4));
    let h = 5u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let dialog = Rect { x, y, width: w, height: h };

    frame.render_widget(Clear, dialog);

    match &app.input_mode {
        Some(InputMode::NewWorktree) => {
            let block = Block::bordered()
                .border_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
                .title(" New Worktree ");
            let inner = block.inner(dialog);
            frame.render_widget(block, dialog);
            let [label_area, input_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
            frame.render_widget(
                Paragraph::new("Branch name (worktree created at ../name):"),
                label_area,
            );
            frame.render_widget(
                Paragraph::new(format!("{}_", app.input_buf))
                    .style(Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                input_area,
            );
        }
        Some(InputMode::ConfirmDelete(_, name)) => {
            let block = Block::bordered()
                .border_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                .title(" Delete Worktree ");
            let inner = block.inner(dialog);
            frame.render_widget(block, dialog);
            let [label_area, confirm_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
            frame.render_widget(
                Paragraph::new(format!("Remove worktree '{name}'?")),
                label_area,
            );
            frame.render_widget(
                Paragraph::new("Press y to confirm, n or Esc to cancel")
                    .style(Style::default().fg(Color::DarkGray)),
                confirm_area,
            );
        }
        Some(InputMode::ConfirmDeleteBranch(name)) => {
            let block = Block::bordered()
                .border_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
                .title(" Delete Branch ");
            let inner = block.inner(dialog);
            frame.render_widget(block, dialog);
            let [label_area, confirm_area] =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
            frame.render_widget(
                Paragraph::new(format!("Delete branch '{name}'?")),
                label_area,
            );
            frame.render_widget(
                Paragraph::new("Press y to confirm, n or Esc to cancel")
                    .style(Style::default().fg(Color::DarkGray)),
                confirm_area,
            );
        }
        None => {}
    }
}
