use std::process::Command;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Tabs, Wrap,
};
use ratatui::Frame;

// ── Pane / tab state ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Claude,
    Git,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitTab {
    Log,
    Worktrees,
    Branches,
    Diff,
}

impl GitTab {
    const ALL: [GitTab; 4] = [GitTab::Diff, GitTab::Worktrees, GitTab::Branches, GitTab::Log];

    fn title(self) -> &'static str {
        match self {
            GitTab::Diff => " Diff ",
            GitTab::Worktrees => " Worktrees ",
            GitTab::Branches => " Branches ",
            GitTab::Log => " Log ",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap()
    }

    fn next(self) -> Self { Self::ALL[(self.index() + 1) % Self::ALL.len()] }
    fn prev(self) -> Self { Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()] }
}

// ── Diff parsing ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum SbsKind { Context, Removed, Added, Changed, Header }

#[derive(Clone)]
struct SbsRow {
    left_no: Option<usize>,
    left: String,
    right_no: Option<usize>,
    right: String,
    kind: SbsKind,
}

struct FileDiff {
    filename: String,
    content: String,
    sbs: Vec<SbsRow>,
}

fn parse_hunk_start(line: &str) -> Option<(usize, usize)> {
    let s = line.strip_prefix("@@ ")?;
    let mut parts = s.split_whitespace();
    let old_start: usize = parts.next()?.strip_prefix('-')?.split(',').next()?.parse().ok()?;
    let new_start: usize = parts.next()?.strip_prefix('+')?.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

fn flush_sbs(rows: &mut Vec<SbsRow>, removed: &mut Vec<(usize, String)>, added: &mut Vec<(usize, String)>) {
    let n = removed.len().max(added.len());
    for i in 0..n {
        let kind = match (i < removed.len(), i < added.len()) {
            (true, true) => SbsKind::Changed,
            (true, false) => SbsKind::Removed,
            _ => SbsKind::Added,
        };
        let (l_no, l) = removed.get(i).cloned().unwrap_or_default();
        let (r_no, r) = added.get(i).cloned().unwrap_or_default();
        rows.push(SbsRow {
            left_no: if i < removed.len() { Some(l_no) } else { None },
            left: l,
            right_no: if i < added.len() { Some(r_no) } else { None },
            right: r,
            kind,
        });
    }
    removed.clear();
    added.clear();
}

fn parse_side_by_side(content: &str) -> Vec<SbsRow> {
    let mut rows: Vec<SbsRow> = Vec::new();
    let mut left_no: usize = 1;
    let mut right_no: usize = 1;
    let mut pending_removed: Vec<(usize, String)> = Vec::new();
    let mut pending_added: Vec<(usize, String)> = Vec::new();

    for line in content.lines() {
        if line.starts_with("@@") {
            flush_sbs(&mut rows, &mut pending_removed, &mut pending_added);
            if let Some((l, r)) = parse_hunk_start(line) {
                left_no = l;
                right_no = r;
            }
            rows.push(SbsRow { left_no: None, left: line.to_string(), right_no: None, right: String::new(), kind: SbsKind::Header });
        } else if line.starts_with("diff ") || line.starts_with("index ") || line.starts_with("--- ") || line.starts_with("+++ ") {
            flush_sbs(&mut rows, &mut pending_removed, &mut pending_added);
            rows.push(SbsRow { left_no: None, left: line.to_string(), right_no: None, right: String::new(), kind: SbsKind::Header });
        } else if let Some(rest) = line.strip_prefix('-') {
            pending_removed.push((left_no, rest.to_string()));
            left_no += 1;
        } else if let Some(rest) = line.strip_prefix('+') {
            pending_added.push((right_no, rest.to_string()));
            right_no += 1;
        } else {
            flush_sbs(&mut rows, &mut pending_removed, &mut pending_added);
            let text = line.strip_prefix(' ').unwrap_or(line);
            rows.push(SbsRow {
                left_no: Some(left_no),
                left: text.to_string(),
                right_no: Some(right_no),
                right: text.to_string(),
                kind: SbsKind::Context,
            });
            left_no += 1;
            right_no += 1;
        }
    }
    flush_sbs(&mut rows, &mut pending_removed, &mut pending_added);
    rows
}

fn parse_diff(raw: &str) -> Vec<FileDiff> {
    let mut result: Vec<FileDiff> = Vec::new();
    let mut content = String::new();
    let mut filename = String::new();

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if !filename.is_empty() {
                let sbs = parse_side_by_side(&content);
                result.push(FileDiff { filename: filename.clone(), content: content.clone(), sbs });
            }
            filename = rest.split_whitespace().nth(1).unwrap_or(rest).trim_start_matches("b/").to_string();
            content = line.to_string();
        } else {
            content.push('\n');
            content.push_str(line);
        }
    }
    if !filename.is_empty() {
        let sbs = parse_side_by_side(&content);
        result.push(FileDiff { filename, content, sbs });
    }
    result
}

// ── App state ─────────────────────────────────────────────────────────────

struct App {
    focused: Pane,
    git_tab: GitTab,
    scroll_offsets: [u16; 3],
    file_diffs: Vec<FileDiff>,
    diff_file_idx: usize,
    diff_content_scroll: u16,
    diff_show_list: bool,
    should_quit: bool,
    fullscreen: bool,
    git_log: String,
    git_worktrees: String,
    git_branches: String,
}

impl App {
    fn new() -> Self {
        let mut app = Self {
            focused: Pane::Claude,
            git_tab: GitTab::Diff,
            scroll_offsets: [0; 3],
            file_diffs: Vec::new(),
            diff_file_idx: 0,
            diff_content_scroll: 0,
            diff_show_list: true,
            should_quit: false,
            fullscreen: false,
            git_log: String::new(),
            git_worktrees: String::new(),
            git_branches: String::new(),
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.git_log = run_command("git", &["log", "--oneline", "--graph", "--decorate", "--color=never", "-100"]);
        self.git_worktrees = run_command("git", &["worktree", "list", "--porcelain"]);
        self.git_branches = run_command("git", &["branch", "-a", "-vv"]);
        self.file_diffs = parse_diff(&run_command("git", &["diff"]));
        self.scroll_offsets = [0; 3];
        self.diff_file_idx = 0;
        self.diff_content_scroll = 0;
    }

    fn simple_tab_content(&self) -> &str {
        match self.git_tab {
            GitTab::Log => &self.git_log,
            GitTab::Worktrees => &self.git_worktrees,
            GitTab::Branches => &self.git_branches,
            GitTab::Diff => "",
        }
    }

    fn current_scroll(&self) -> u16 {
        match self.git_tab {
            GitTab::Diff => self.diff_content_scroll,
            t => self.scroll_offsets[t.index()],
        }
    }

    fn scroll_down(&mut self, amount: u16) {
        if self.git_tab == GitTab::Diff {
            let max = self.file_diffs.get(self.diff_file_idx)
                .map(|f| if self.fullscreen { f.sbs.len() } else { f.content.lines().count() }
                    .saturating_sub(1) as u16)
                .unwrap_or(0);
            self.diff_content_scroll = (self.diff_content_scroll + amount).min(max);
        } else {
            let i = self.git_tab.index();
            let max = self.simple_tab_content().lines().count().saturating_sub(1) as u16;
            self.scroll_offsets[i] = (self.scroll_offsets[i] + amount).min(max);
        }
    }

    fn scroll_up(&mut self, amount: u16) {
        if self.git_tab == GitTab::Diff {
            self.diff_content_scroll = self.diff_content_scroll.saturating_sub(amount);
        } else {
            let i = self.git_tab.index();
            self.scroll_offsets[i] = self.scroll_offsets[i].saturating_sub(amount);
        }
    }

    fn diff_select_next(&mut self) {
        if !self.file_diffs.is_empty() {
            self.diff_file_idx = (self.diff_file_idx + 1).min(self.file_diffs.len() - 1);
            self.diff_content_scroll = 0;
        }
    }

    fn diff_select_prev(&mut self) {
        self.diff_file_idx = self.diff_file_idx.saturating_sub(1);
        self.diff_content_scroll = 0;
    }
}

// ── Shell helper ──────────────────────────────────────────────────────────

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

// ── Entry point ───────────────────────────────────────────────────────────

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

// ── Events ────────────────────────────────────────────────────────────────

fn handle_events(app: &mut App) -> std::io::Result<()> {
    match event::read()? {
        Event::Key(KeyEvent { code, kind: KeyEventKind::Press, modifiers, .. }) => match code {
            KeyCode::Char('q') if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => app.should_quit = true,
            KeyCode::Char('f') if modifiers.contains(KeyModifiers::CONTROL) => {
                app.fullscreen = !app.fullscreen;
                app.diff_content_scroll = 0;
            }
            KeyCode::Char('r') if modifiers.contains(KeyModifiers::CONTROL) => app.refresh(),
            KeyCode::Char('e') if app.focused == Pane::Git && app.git_tab == GitTab::Diff => {
                app.diff_show_list = !app.diff_show_list;
            }
            KeyCode::Tab => app.focused = if app.focused == Pane::Claude { Pane::Git } else { Pane::Claude },
            KeyCode::BackTab => app.focused = if app.focused == Pane::Claude { Pane::Git } else { Pane::Claude },
            KeyCode::Right if app.focused == Pane::Git => app.git_tab = app.git_tab.next(),
            KeyCode::Left if app.focused == Pane::Git => app.git_tab = app.git_tab.prev(),
            KeyCode::Up if app.focused == Pane::Git && app.git_tab == GitTab::Diff && app.diff_show_list => app.diff_select_prev(),
            KeyCode::Down if app.focused == Pane::Git && app.git_tab == GitTab::Diff && app.diff_show_list => app.diff_select_next(),
            KeyCode::Up if app.focused == Pane::Git => app.scroll_up(1),
            KeyCode::Down if app.focused == Pane::Git => app.scroll_down(1),
            KeyCode::Char('k') if app.focused == Pane::Git => app.scroll_up(20),
            KeyCode::Char('j') if app.focused == Pane::Git => app.scroll_down(20),
            _ => {}
        },
        _ => {}
    }
    Ok(())
}

// ── Rendering ─────────────────────────────────────────────────────────────

fn render(app: &App, frame: &mut Frame) {
    let normal = Style::default().fg(Color::Blue);
    let focused = Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD);

    if app.fullscreen {
        let area = frame.area();
        match app.focused {
            Pane::Claude => frame.render_widget(pane_block("Claude Code", true, normal, focused), area),
            Pane::Git => render_git_pane(app, frame, area, true, normal, focused),
        }
        return;
    }

    let [left, right] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .areas(frame.area());

    frame.render_widget(pane_block("Claude Code", app.focused == Pane::Claude, normal, focused), left);
    render_git_pane(app, frame, right, app.focused == Pane::Git, normal, focused);
}

fn render_git_pane(app: &App, frame: &mut Frame, area: Rect, focused: bool, normal: Style, active: Style) {
    let border_style = if focused { active } else { normal };
    let hint = if focused {
        match (app.git_tab, app.fullscreen) {
            (GitTab::Diff, true) => " ↑↓/j/k scroll  e explorer  ^F exit fullscreen  ^R refresh ",
            (GitTab::Diff, false) => " ←→ tabs  ↑↓ file  j/k scroll  e explorer  ^F fullscreen  ^R refresh ",
            _ => " ←→ tabs  ↑↓ scroll  ^R refresh ",
        }
    } else {
        ""
    };

    let block = Block::bordered()
        .border_style(border_style)
        .title_bottom(ratatui::text::Line::from(hint).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [tabs_area, sep_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)])
            .areas(inner);

    let tab_style = if focused { Style::default().fg(Color::Blue) } else { Style::default().fg(Color::DarkGray) };
    let tabs = Tabs::new(GitTab::ALL.iter().map(|t| t.title()).collect::<Vec<_>>())
        .select(app.git_tab.index())
        .style(tab_style)
        .highlight_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::UNDERLINED))
        .divider("│");
    frame.render_widget(tabs, tabs_area);
    frame.render_widget(Block::default().borders(Borders::TOP).border_style(border_style), sep_area);

    if app.git_tab == GitTab::Diff {
        render_diff_tab(app, frame, content_area, focused, border_style);
    } else {
        render_scrollable_text(app.simple_tab_content(), app.current_scroll(), frame, content_area, border_style);
    }
}

fn render_diff_tab(app: &App, frame: &mut Frame, area: Rect, focused: bool, border_style: Style) {
    if app.file_diffs.is_empty() {
        frame.render_widget(Paragraph::new(if focused { "No changes in working tree." } else { "" }), area);
        return;
    }

    // Determine content area: strip file list if visible
    let diff_area = if app.diff_show_list {
        let [list_area, div_area, content_area] = Layout::horizontal([
            Constraint::Percentage(20),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .areas(area);

        let items: Vec<ListItem> = app.file_diffs.iter().map(|f| ListItem::new(f.filename.as_str())).collect();
        let list = List::new(items)
            .highlight_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::REVERSED))
            .highlight_symbol("▶ ");
        let mut list_state = ListState::default().with_selected(Some(app.diff_file_idx));
        frame.render_stateful_widget(list, list_area, &mut list_state);
        frame.render_widget(Block::default().borders(Borders::LEFT).border_style(border_style), div_area);

        content_area
    } else {
        area
    };

    if let Some(fd) = app.file_diffs.get(app.diff_file_idx) {
        if app.fullscreen {
            render_sbs(fd, app.diff_content_scroll, frame, diff_area, border_style);
        } else {
            render_unified_diff(&fd.content, app.diff_content_scroll, frame, diff_area, border_style);
        }
    }
}

fn render_unified_diff(content: &str, scroll: u16, frame: &mut Frame, area: Rect, border_style: Style) {
    let lines: Vec<Line> = content.lines().map(color_diff_line).collect();
    let line_count = lines.len();
    let [text_area, sb_area] = Layout::horizontal([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)).wrap(Wrap { trim: false }), text_area);
    let mut sb = ScrollbarState::new(line_count.saturating_sub(text_area.height as usize)).position(scroll as usize);
    frame.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight).style(border_style), sb_area, &mut sb);
}

fn render_sbs(fd: &FileDiff, scroll: u16, frame: &mut Frame, area: Rect, border_style: Style) {
    let rows = &fd.sbs;
    let total = rows.len();

    // compute line-number display width
    let max_no = rows.iter().flat_map(|r| [r.left_no, r.right_no]).flatten().max().unwrap_or(1);
    let no_w = max_no.to_string().len();

    let [left_area, div_area, right_area, sb_area] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Length(1),
        Constraint::Percentage(50),
        Constraint::Length(1),
    ])
    .areas(area);

    // divider
    frame.render_widget(Block::default().borders(Borders::LEFT).border_style(border_style), div_area);

    let height = left_area.height as usize;
    let start = scroll as usize;
    let end = (start + height).min(total);

    let mut left_lines: Vec<Line> = Vec::with_capacity(height);
    let mut right_lines: Vec<Line> = Vec::with_capacity(height);

    for row in rows.get(start..end).unwrap_or(&[]) {
        let (l_line, r_line) = sbs_row_to_lines(row, no_w);
        left_lines.push(l_line);
        right_lines.push(r_line);
    }

    frame.render_widget(Paragraph::new(left_lines), left_area);
    frame.render_widget(Paragraph::new(right_lines), right_area);

    let mut sb = ScrollbarState::new(total.saturating_sub(height)).position(scroll as usize);
    frame.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight).style(border_style), sb_area, &mut sb);
}

fn sbs_row_to_lines(row: &SbsRow, no_w: usize) -> (Line<'static>, Line<'static>) {
    match row.kind {
        SbsKind::Header => {
            let s = Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD);
            let left = Line::from(Span::styled(row.left.clone(), s));
            let right = Line::from(Span::styled(row.right.clone(), s));
            (left, right)
        }
        SbsKind::Context => {
            let no_style = Style::default().fg(Color::DarkGray);
            let l_no = format!("{:>no_w$} ", row.left_no.map(|n| n.to_string()).unwrap_or_default());
            let r_no = format!("{:>no_w$} ", row.right_no.map(|n| n.to_string()).unwrap_or_default());
            let left = Line::from(vec![Span::styled(l_no, no_style), Span::raw(row.left.clone())]);
            let right = Line::from(vec![Span::styled(r_no, no_style), Span::raw(row.right.clone())]);
            (left, right)
        }
        SbsKind::Removed => {
            let no_style = Style::default().fg(Color::DarkGray);
            let s = Style::default().fg(Color::Red);
            let l_no = format!("{:>no_w$} ", row.left_no.map(|n| n.to_string()).unwrap_or_default());
            let left = Line::from(vec![Span::styled(l_no, no_style), Span::styled(row.left.clone(), s)]);
            let right = Line::from(Span::raw(""));
            (left, right)
        }
        SbsKind::Added => {
            let no_style = Style::default().fg(Color::DarkGray);
            let s = Style::default().fg(Color::Green);
            let r_no = format!("{:>no_w$} ", row.right_no.map(|n| n.to_string()).unwrap_or_default());
            let left = Line::from(Span::raw(""));
            let right = Line::from(vec![Span::styled(r_no, no_style), Span::styled(row.right.clone(), s)]);
            (left, right)
        }
        SbsKind::Changed => {
            let no_style = Style::default().fg(Color::DarkGray);
            let l_no = format!("{:>no_w$} ", row.left_no.map(|n| n.to_string()).unwrap_or_default());
            let r_no = format!("{:>no_w$} ", row.right_no.map(|n| n.to_string()).unwrap_or_default());
            let left = Line::from(vec![
                Span::styled(l_no, no_style),
                Span::styled(row.left.clone(), Style::default().fg(Color::Red)),
            ]);
            let right = Line::from(vec![
                Span::styled(r_no, no_style),
                Span::styled(row.right.clone(), Style::default().fg(Color::Green)),
            ]);
            (left, right)
        }
    }
}

fn render_scrollable_text(content: &str, scroll: u16, frame: &mut Frame, area: Rect, border_style: Style) {
    let line_count = content.lines().count();
    let [text_area, sb_area] = Layout::horizontal([Constraint::Min(0), Constraint::Length(1)]).areas(area);
    frame.render_widget(Paragraph::new(content).scroll((scroll, 0)).wrap(Wrap { trim: false }), text_area);
    let mut sb = ScrollbarState::new(line_count.saturating_sub(text_area.height as usize)).position(scroll as usize);
    frame.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight).style(border_style), sb_area, &mut sb);
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

fn pane_block(title: &str, focused: bool, normal: Style, active: Style) -> Block<'_> {
    let border_style = if focused { active } else { normal };
    let label = if focused { format!(" {} * ", title) } else { format!(" {} ", title) };
    Block::bordered().title(label).border_style(border_style)
}
