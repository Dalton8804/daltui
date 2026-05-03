use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::Window;
use crate::pty::PtySession;

pub fn render_claude_pane(
    win: &Window,
    frame: &mut Frame,
    area: Rect,
    focused: bool,
    normal: Style,
    active: Style,
) {
    let border_style = if focused { active } else { normal };
    let hint = if focused { " S↑↓/PgUp/PgDn scroll  ^W pane  ^Q quit " } else { "" };
    let title = if focused { format!(" {} * ", win.name) } else { format!(" {} ", win.name) };
    render_pty_pane(
        win.claude.as_ref(),
        frame,
        area,
        focused,
        title,
        hint,
        border_style,
        "Failed to start claude CLI.\nEnsure `claude` is on PATH.",
        win.claude_scroll,
    );
}

pub fn render_terminal_pane(
    win: &Window,
    frame: &mut Frame,
    area: Rect,
    focused: bool,
    normal: Style,
    active: Style,
) {
    let border_style = if focused { active } else { normal };
    let hint = if focused { " ^W pane  ^Q quit " } else { "" };
    let title = if focused { " Terminal * ".to_string() } else { " Terminal ".to_string() };
    render_pty_pane(
        win.terminal.as_ref(),
        frame,
        area,
        focused,
        title,
        hint,
        border_style,
        "Failed to start shell.\nCheck $SHELL.",
        0,
    );
}

fn render_pty_pane(
    session: Option<&PtySession>,
    frame: &mut Frame,
    area: Rect,
    focused: bool,
    title: String,
    hint: &str,
    border_style: Style,
    fail_msg: &str,
    scroll_offset: usize,
) {
    let block = Block::bordered()
        .border_style(border_style)
        .title(title)
        .title_bottom(Line::from(hint).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(session) = session else {
        frame.render_widget(
            Paragraph::new(fail_msg.to_string()).style(Style::default().fg(Color::Red)),
            inner,
        );
        return;
    };

    let Ok(mut parser) = session.parser.lock() else {
        return;
    };

    parser.set_scrollback(scroll_offset);

    let screen = parser.screen();
    let (screen_rows, screen_cols) = screen.size();

    let lines: Vec<Line> = (0..screen_rows)
        .map(|row| {
            let mut spans: Vec<Span> = Vec::new();
            let mut run_text = String::new();
            let mut run_style = Style::default();
            for col in 0..screen_cols {
                let (text, style) = match screen.cell(row, col) {
                    None => (" ".to_string(), Style::default()),
                    Some(cell) => {
                        let c = cell.contents();
                        (if c.is_empty() { " ".to_string() } else { c }, cell_to_style(cell))
                    }
                };
                if style == run_style {
                    run_text.push_str(&text);
                } else {
                    if !run_text.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut run_text), run_style));
                    }
                    run_text = text;
                    run_style = style;
                }
            }
            if !run_text.is_empty() {
                spans.push(Span::styled(run_text, run_style));
            }
            Line::from(spans)
        })
        .collect();

    let cursor = if focused && scroll_offset == 0 {
        Some(screen.cursor_position())
    } else {
        None
    };

    parser.set_scrollback(0);
    drop(parser);

    frame.render_widget(Paragraph::new(lines), inner);

    if let Some((crow, ccol)) = cursor {
        let cx = inner.x.saturating_add(ccol);
        let cy = inner.y.saturating_add(crow);
        if cx < inner.x + inner.width && cy < inner.y + inner.height {
            frame.set_cursor_position((cx, cy));
        }
    }
}

fn cell_to_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = vt100_color(cell.fgcolor()) {
        style = style.fg(fg);
    }
    if let Some(bg) = vt100_color(cell.bgcolor()) {
        style = style.bg(bg);
    }
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }
    style
}

fn vt100_color(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(match i {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::White,
            8 => Color::DarkGray,
            9 => Color::LightRed,
            10 => Color::LightGreen,
            11 => Color::LightYellow,
            12 => Color::LightBlue,
            13 => Color::LightMagenta,
            14 => Color::LightCyan,
            15 => Color::Gray,
            n => Color::Indexed(n),
        }),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}
