use std::collections::HashSet;
use std::path::PathBuf;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Tabs, Wrap,
};
use ratatui::Frame;

use crate::app::{GitTab, Window};
use crate::git::{FileDiff, SbsKind, SbsRow};

pub fn render_git_pane(
    win: &Window,
    frame: &mut Frame,
    area: Rect,
    focused: bool,
    normal: Style,
    active: Style,
    open_paths: &HashSet<PathBuf>,
) {
    let border_style = if focused { active } else { normal };
    let hint = if focused {
        match (win.git_tab, win.fullscreen) {
            (GitTab::Diff, true) => {
                " ↑↓/j/k scroll  e explorer  ^F exit fullscreen  ^R refresh "
            }
            (GitTab::Diff, false) => {
                " ←→ tabs  ↑↓ file  j/k scroll  e explorer  ^F fullscreen  ^R refresh "
            }
            (GitTab::Worktrees, _) => {
                " ←→ tabs  ↑↓ select  Enter open  ^T new  d delete  ^R refresh "
            }
            (GitTab::Branches, _) => " ←→ tabs  ↑↓ select  d delete  ^R refresh ",
            _ => " ←→ tabs  ↑↓ scroll  ^R refresh ",
        }
    } else {
        ""
    };

    let block = Block::bordered()
        .border_style(border_style)
        .title_bottom(Line::from(hint).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [tabs_area, sep_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)])
            .areas(inner);

    let tab_style = if focused {
        Style::default().fg(Color::Blue)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let tabs = Tabs::new(GitTab::ALL.iter().map(|t| t.title()).collect::<Vec<_>>())
        .select(win.git_tab.index())
        .style(tab_style)
        .highlight_style(
            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
        )
        .divider("│");
    frame.render_widget(tabs, tabs_area);
    frame.render_widget(
        Block::default().borders(Borders::TOP).border_style(border_style),
        sep_area,
    );

    match win.git_tab {
        GitTab::Diff => render_diff_tab(win, frame, content_area, focused, border_style),
        GitTab::Worktrees => {
            render_worktrees_tab(win, frame, content_area, border_style, open_paths)
        }
        GitTab::Branches => render_branches_tab(win, frame, content_area),
        GitTab::Log => render_scrollable_text(
            win.simple_tab_content(),
            win.current_scroll(),
            frame,
            content_area,
            border_style,
        ),
    }
}

fn render_worktrees_tab(
    win: &Window,
    frame: &mut Frame,
    area: Rect,
    _border_style: Style,
    open_paths: &HashSet<PathBuf>,
) {
    if win.git_worktrees.is_empty() {
        frame.render_widget(Paragraph::new("No worktrees found."), area);
        return;
    }

    let items: Vec<ListItem> = win
        .git_worktrees
        .iter()
        .map(|wt| {
            let label = if open_paths.contains(&wt.path) {
                format!("{} [open]", wt.name)
            } else {
                wt.name.clone()
            };
            ListItem::new(label)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
        .highlight_symbol("▶ ");
    let mut state = ListState::default().with_selected(Some(win.worktree_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_branches_tab(win: &Window, frame: &mut Frame, area: Rect) {
    if win.git_branches.is_empty() {
        frame.render_widget(Paragraph::new("No branches found."), area);
        return;
    }

    let items: Vec<ListItem> = win
        .git_branches
        .iter()
        .map(|b| {
            let marker = if b.is_current { "* " } else { "  " };
            let style = if b.is_current {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(format!("{}{}", marker, b.name)).style(style)
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )
        .highlight_symbol("▶ ");
    let mut state = ListState::default().with_selected(Some(win.branch_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_diff_tab(
    win: &Window,
    frame: &mut Frame,
    area: Rect,
    focused: bool,
    border_style: Style,
) {
    if win.file_diffs.is_empty() {
        frame.render_widget(
            Paragraph::new(if focused { "No changes in working tree." } else { "" }),
            area,
        );
        return;
    }

    let diff_area = if win.diff_show_list {
        let [list_area, div_area, content_area] = Layout::horizontal([
            Constraint::Percentage(20),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);

        let items: Vec<ListItem> =
            win.file_diffs.iter().map(|f| ListItem::new(f.filename.as_str())).collect();
        let list = List::new(items)
            .highlight_style(
                Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::REVERSED),
            )
            .highlight_symbol("▶ ");
        let mut list_state = ListState::default().with_selected(Some(win.diff_file_idx));
        frame.render_stateful_widget(list, list_area, &mut list_state);
        frame.render_widget(
            Block::default().borders(Borders::LEFT).border_style(border_style),
            div_area,
        );
        content_area
    } else {
        area
    };

    if let Some(fd) = win.file_diffs.get(win.diff_file_idx) {
        if win.fullscreen {
            render_sbs(fd, win.diff_content_scroll, frame, diff_area, border_style);
        } else {
            render_unified_diff(&fd.content, win.diff_content_scroll, frame, diff_area, border_style);
        }
    }
}

fn render_unified_diff(
    content: &str,
    scroll: u16,
    frame: &mut Frame,
    area: Rect,
    border_style: Style,
) {
    let lines: Vec<Line> = content.lines().map(color_diff_line).collect();
    let line_count = lines.len();
    let [text_area, sb_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)).wrap(Wrap { trim: false }),
        text_area,
    );
    let mut sb = ScrollbarState::new(line_count.saturating_sub(text_area.height as usize))
        .position(scroll as usize);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight).style(border_style),
        sb_area,
        &mut sb,
    );
}

fn render_sbs(
    fd: &FileDiff,
    scroll: u16,
    frame: &mut Frame,
    area: Rect,
    border_style: Style,
) {
    let rows = &fd.sbs;
    let total = rows.len();
    let max_no =
        rows.iter().flat_map(|r| [r.left_no, r.right_no]).flatten().max().unwrap_or(1);
    let no_w = max_no.to_string().len();

    let [left_area, div_area, right_area, sb_area] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Length(1),
        Constraint::Percentage(50),
        Constraint::Length(1),
    ])
    .areas(area);

    frame.render_widget(
        Block::default().borders(Borders::LEFT).border_style(border_style),
        div_area,
    );

    let height = left_area.height as usize;
    let start = scroll as usize;
    let end = (start + height).min(total);
    let (mut ll, mut rl) = (Vec::with_capacity(height), Vec::with_capacity(height));
    for row in rows.get(start..end).unwrap_or(&[]) {
        let (l, r) = sbs_row_to_lines(row, no_w);
        ll.push(l);
        rl.push(r);
    }
    frame.render_widget(Paragraph::new(ll), left_area);
    frame.render_widget(Paragraph::new(rl), right_area);

    let mut sb =
        ScrollbarState::new(total.saturating_sub(height)).position(scroll as usize);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight).style(border_style),
        sb_area,
        &mut sb,
    );
}

fn sbs_row_to_lines(row: &SbsRow, no_w: usize) -> (Line<'static>, Line<'static>) {
    let no_s = Style::default().fg(Color::DarkGray);
    let fmt = |n: Option<usize>| {
        format!("{:>no_w$} ", n.map(|v| v.to_string()).unwrap_or_default())
    };
    match row.kind {
        SbsKind::Header => {
            let s = Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD);
            (
                Line::from(Span::styled(row.left.clone(), s)),
                Line::from(Span::styled(row.right.clone(), s)),
            )
        }
        SbsKind::Context => (
            Line::from(vec![
                Span::styled(fmt(row.left_no), no_s),
                Span::raw(row.left.clone()),
            ]),
            Line::from(vec![
                Span::styled(fmt(row.right_no), no_s),
                Span::raw(row.right.clone()),
            ]),
        ),
        SbsKind::Removed => (
            Line::from(vec![
                Span::styled(fmt(row.left_no), no_s),
                Span::styled(row.left.clone(), Style::default().fg(Color::Red)),
            ]),
            Line::from(Span::raw("")),
        ),
        SbsKind::Added => (
            Line::from(Span::raw("")),
            Line::from(vec![
                Span::styled(fmt(row.right_no), no_s),
                Span::styled(row.right.clone(), Style::default().fg(Color::Green)),
            ]),
        ),
        SbsKind::Changed => (
            Line::from(vec![
                Span::styled(fmt(row.left_no), no_s),
                Span::styled(row.left.clone(), Style::default().fg(Color::Red)),
            ]),
            Line::from(vec![
                Span::styled(fmt(row.right_no), no_s),
                Span::styled(row.right.clone(), Style::default().fg(Color::Green)),
            ]),
        ),
    }
}

fn render_scrollable_text(
    content: &str,
    scroll: u16,
    frame: &mut Frame,
    area: Rect,
    border_style: Style,
) {
    let line_count = content.lines().count();
    let [text_area, sb_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(content).scroll((scroll, 0)).wrap(Wrap { trim: false }),
        text_area,
    );
    let mut sb = ScrollbarState::new(line_count.saturating_sub(text_area.height as usize))
        .position(scroll as usize);
    frame.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight).style(border_style),
        sb_area,
        &mut sb,
    );
}

fn color_diff_line(line: &str) -> Line<'_> {
    let style = if line.starts_with('+') && !line.starts_with("+++") {
        Style::default().fg(Color::Green)
    } else if line.starts_with('-') && !line.starts_with("---") {
        Style::default().fg(Color::Red)
    } else if line.starts_with("@@") {
        Style::default().fg(Color::Cyan)
    } else if line.starts_with("diff ") || line.starts_with("index ") {
        Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(Span::styled(line, style))
}
