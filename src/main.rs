use std::io::{Read, Write};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
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
enum Pane { Claude, Git }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitTab { Log, Worktrees, Branches, Diff }

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

    fn index(self) -> usize { Self::ALL.iter().position(|t| *t == self).unwrap() }
    fn next(self) -> Self { Self::ALL[(self.index() + 1) % Self::ALL.len()] }
    fn prev(self) -> Self { Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()] }
}

// ── Claude PTY session ────────────────────────────────────────────────────

struct ClaudeSession {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    child:  Box<dyn portable_pty::Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
}

fn spawn_claude_session() -> Option<ClaudeSession> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }).ok()?;

    let child = pair.slave.spawn_command(CommandBuilder::new("claude")).ok()?;
    drop(pair.slave); // must drop slave in parent or reads block after child exits

    let writer = pair.master.take_writer().ok()?;
    let reader = pair.master.try_clone_reader().ok()?;

    let parser = Arc::new(Mutex::new(vt100::Parser::new(24, 80, 0)));
    let parser_clone = Arc::clone(&parser);

    std::thread::spawn(move || pty_reader_thread(reader, parser_clone));

    Some(ClaudeSession { parser, writer, child, master: pair.master })
}

fn pty_reader_thread(mut reader: Box<dyn Read + Send>, parser: Arc<Mutex<vt100::Parser>>) {
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if let Ok(mut p) = parser.lock() {
                    p.process(&buf[..n]);
                }
            }
        }
    }
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

struct FileDiff { filename: String, content: String, sbs: Vec<SbsRow> }

fn parse_hunk_start(line: &str) -> Option<(usize, usize)> {
    let s = line.strip_prefix("@@ ")?;
    let mut parts = s.split_whitespace();
    let old: usize = parts.next()?.strip_prefix('-')?.split(',').next()?.parse().ok()?;
    let new: usize = parts.next()?.strip_prefix('+')?.split(',').next()?.parse().ok()?;
    Some((old, new))
}

fn flush_sbs(rows: &mut Vec<SbsRow>, rem: &mut Vec<(usize, String)>, add: &mut Vec<(usize, String)>) {
    let n = rem.len().max(add.len());
    for i in 0..n {
        let kind = match (i < rem.len(), i < add.len()) {
            (true, true) => SbsKind::Changed,
            (true, false) => SbsKind::Removed,
            _ => SbsKind::Added,
        };
        let (l_no, l) = rem.get(i).cloned().unwrap_or_default();
        let (r_no, r) = add.get(i).cloned().unwrap_or_default();
        rows.push(SbsRow {
            left_no: if i < rem.len() { Some(l_no) } else { None }, left: l,
            right_no: if i < add.len() { Some(r_no) } else { None }, right: r,
            kind,
        });
    }
    rem.clear(); add.clear();
}

fn parse_side_by_side(content: &str) -> Vec<SbsRow> {
    let mut rows = Vec::new();
    let (mut ln, mut rn) = (1usize, 1usize);
    let (mut rem, mut add) = (Vec::new(), Vec::new());
    for line in content.lines() {
        if line.starts_with("@@") {
            flush_sbs(&mut rows, &mut rem, &mut add);
            if let Some((l, r)) = parse_hunk_start(line) { ln = l; rn = r; }
            rows.push(SbsRow { left_no: None, left: line.to_string(), right_no: None, right: String::new(), kind: SbsKind::Header });
        } else if line.starts_with("diff ") || line.starts_with("index ") || line.starts_with("--- ") || line.starts_with("+++ ") {
            flush_sbs(&mut rows, &mut rem, &mut add);
            rows.push(SbsRow { left_no: None, left: line.to_string(), right_no: None, right: String::new(), kind: SbsKind::Header });
        } else if let Some(rest) = line.strip_prefix('-') {
            rem.push((ln, rest.to_string())); ln += 1;
        } else if let Some(rest) = line.strip_prefix('+') {
            add.push((rn, rest.to_string())); rn += 1;
        } else {
            flush_sbs(&mut rows, &mut rem, &mut add);
            let text = line.strip_prefix(' ').unwrap_or(line);
            rows.push(SbsRow { left_no: Some(ln), left: text.to_string(), right_no: Some(rn), right: text.to_string(), kind: SbsKind::Context });
            ln += 1; rn += 1;
        }
    }
    flush_sbs(&mut rows, &mut rem, &mut add);
    rows
}

fn parse_diff(raw: &str) -> Vec<FileDiff> {
    let mut result = Vec::new();
    let (mut content, mut filename) = (String::new(), String::new());
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            if !filename.is_empty() {
                result.push(FileDiff { filename: filename.clone(), sbs: parse_side_by_side(&content), content: content.clone() });
            }
            filename = rest.split_whitespace().nth(1).unwrap_or(rest).trim_start_matches("b/").to_string();
            content = line.to_string();
        } else {
            content.push('\n'); content.push_str(line);
        }
    }
    if !filename.is_empty() {
        result.push(FileDiff { filename, sbs: parse_side_by_side(&content), content });
    }
    result
}

// ── App state ─────────────────────────────────────────────────────────────

struct App {
    focused: Pane,
    git_tab: GitTab,
    scroll_offsets: [u16; 4],
    file_diffs: Vec<FileDiff>,
    diff_file_idx: usize,
    diff_content_scroll: u16,
    diff_show_list: bool,
    should_quit: bool,
    fullscreen: bool,
    git_log: String,
    git_worktrees: String,
    git_branches: String,
    claude: Option<ClaudeSession>,
}

impl App {
    fn new() -> Self {
        let mut app = Self {
            focused: Pane::Claude,
            git_tab: GitTab::Diff,
            scroll_offsets: [0; 4],
            file_diffs: Vec::new(),
            diff_file_idx: 0,
            diff_content_scroll: 0,
            diff_show_list: true,
            should_quit: false,
            fullscreen: false,
            git_log: String::new(),
            git_worktrees: String::new(),
            git_branches: String::new(),
            claude: spawn_claude_session(),
        };
        app.refresh();
        app
    }

    fn refresh(&mut self) {
        self.git_log = run_command("git", &["log", "--oneline", "--graph", "--decorate", "--color=never", "-100"]);
        self.git_worktrees = run_command("git", &["worktree", "list", "--porcelain"]);
        self.git_branches = run_command("git", &["branch", "-a", "-vv"]);
        self.file_diffs = parse_diff(&run_command("git", &["diff"]));
        self.scroll_offsets = [0; 4];
        self.diff_file_idx = 0;
        self.diff_content_scroll = 0;
    }

    fn resize_claude_pty(&mut self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if let Some(ref mut s) = self.claude {
            let _ = s.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
            if let Ok(mut p) = s.parser.lock() {
                p.set_size(rows, cols);
            }
        }
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
                .map(|f| if self.fullscreen { f.sbs.len() } else { f.content.lines().count() }.saturating_sub(1) as u16)
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
    Command::new(cmd).args(args).output()
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

    // Size the PTY to match the actual pane on startup
    let size = terminal.size()?;
    let pty_cols = (size.width / 2).saturating_sub(2);
    let pty_rows = size.height.saturating_sub(2);
    app.resize_claude_pty(pty_cols, pty_rows);

    while !app.should_quit {
        terminal.draw(|frame| render(&app, frame))?;
        // Poll 16ms so PTY output causes continuous redraws without blocking
        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) => handle_key(&mut app, key),
                Event::Resize(cols, rows) => handle_resize(&mut app, cols, rows),
                _ => {}
            }
        }
    }

    if let Some(ref mut s) = app.claude {
        let _ = s.child.kill();
    }
    Ok(())
}

// ── Events ────────────────────────────────────────────────────────────────

fn handle_resize(app: &mut App, total_cols: u16, total_rows: u16) {
    let pty_cols = if app.fullscreen && app.focused == Pane::Claude {
        total_cols.saturating_sub(2)
    } else {
        (total_cols / 2).saturating_sub(2)
    };
    let pty_rows = total_rows.saturating_sub(2);
    app.resize_claude_pty(pty_cols, pty_rows);
}

fn handle_key(app: &mut App, key: KeyEvent) {
    if key.kind != KeyEventKind::Press { return; }

    // Global: Ctrl+Q always quits
    if key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }
    // Global: Ctrl+W switches panes
    if key.code == KeyCode::Char('w') && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.focused = match app.focused { Pane::Claude => Pane::Git, Pane::Git => Pane::Claude };
        return;
    }

    match app.focused {
        Pane::Claude => {
            if let Some(ref mut s) = app.claude {
                let bytes = key_to_bytes(&key);
                if !bytes.is_empty() {
                    let _ = s.writer.write_all(&bytes);
                }
            }
        }
        Pane::Git => handle_git_key(app, key),
    }
}

fn handle_git_key(app: &mut App, key: KeyEvent) {
    use KeyCode::*;
    use KeyModifiers as KM;
    match (key.code, key.modifiers) {
        (Char('c'), KM::CONTROL) | (Char('q'), _) => app.should_quit = true,
        (Char('f'), KM::CONTROL) => { app.fullscreen = !app.fullscreen; app.diff_content_scroll = 0; }
        (Char('r'), KM::CONTROL) => app.refresh(),
        (Char('e'), KM::NONE) if app.git_tab == GitTab::Diff => app.diff_show_list = !app.diff_show_list,
        (Right, _) => app.git_tab = app.git_tab.next(),
        (Left, _)  => app.git_tab = app.git_tab.prev(),
        (Up, _) if app.git_tab == GitTab::Diff && app.diff_show_list => app.diff_select_prev(),
        (Down, _) if app.git_tab == GitTab::Diff && app.diff_show_list => app.diff_select_next(),
        (Up, _)       => app.scroll_up(1),
        (Down, _)     => app.scroll_down(1),
        (Char('k'), KM::NONE) => app.scroll_up(20),
        (Char('j'), KM::NONE) => app.scroll_down(20),
        _ => {}
    }
}

fn key_to_bytes(key: &KeyEvent) -> Vec<u8> {
    use KeyCode::*;
    use KeyModifiers as KM;
    match key.code {
        Char(c) => {
            if key.modifiers.contains(KM::CONTROL) {
                let b = (c as u8).to_ascii_uppercase();
                vec![if b >= b'A' && b <= b'Z' { b - b'A' + 1 } else { c as u8 & 0x1F }]
            } else if key.modifiers.contains(KM::ALT) {
                let mut v = vec![0x1B];
                let mut tmp = [0u8; 4];
                v.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
                v
            } else {
                let mut tmp = [0u8; 4];
                c.encode_utf8(&mut tmp).as_bytes().to_vec()
            }
        }
        Enter     => vec![b'\r'],
        Backspace => vec![0x7F],
        Delete    => vec![0x1B, b'[', b'3', b'~'],
        Esc       => vec![0x1B],
        Tab       => vec![b'\t'],
        BackTab   => vec![0x1B, b'[', b'Z'],
        Up        => vec![0x1B, b'[', b'A'],
        Down      => vec![0x1B, b'[', b'B'],
        Right     => vec![0x1B, b'[', b'C'],
        Left      => vec![0x1B, b'[', b'D'],
        Home      => vec![0x1B, b'[', b'H'],
        End       => vec![0x1B, b'[', b'F'],
        PageUp    => vec![0x1B, b'[', b'5', b'~'],
        PageDown  => vec![0x1B, b'[', b'6', b'~'],
        Insert    => vec![0x1B, b'[', b'2', b'~'],
        F(1)  => vec![0x1B, b'O', b'P'],
        F(2)  => vec![0x1B, b'O', b'Q'],
        F(3)  => vec![0x1B, b'O', b'R'],
        F(4)  => vec![0x1B, b'O', b'S'],
        F(5)  => vec![0x1B, b'[', b'1', b'5', b'~'],
        F(6)  => vec![0x1B, b'[', b'1', b'7', b'~'],
        F(7)  => vec![0x1B, b'[', b'1', b'8', b'~'],
        F(8)  => vec![0x1B, b'[', b'1', b'9', b'~'],
        F(9)  => vec![0x1B, b'[', b'2', b'0', b'~'],
        F(10) => vec![0x1B, b'[', b'2', b'1', b'~'],
        F(11) => vec![0x1B, b'[', b'2', b'3', b'~'],
        F(12) => vec![0x1B, b'[', b'2', b'4', b'~'],
        _ => vec![],
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────

fn render(app: &App, frame: &mut Frame) {
    let normal = Style::default().fg(Color::Blue);
    let active = Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD);

    if app.fullscreen {
        let area = frame.area();
        match app.focused {
            Pane::Claude => render_claude_pane(app, frame, area, true, normal, active),
            Pane::Git    => render_git_pane(app, frame, area, true, normal, active),
        }
        return;
    }

    let [left, right] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .areas(frame.area());
    render_claude_pane(app, frame, left,  app.focused == Pane::Claude, normal, active);
    render_git_pane(app, frame, right, app.focused == Pane::Git,    normal, active);
}

// ── Claude pane ───────────────────────────────────────────────────────────

fn render_claude_pane(app: &App, frame: &mut Frame, area: Rect, focused: bool, normal: Style, active: Style) {
    let border_style = if focused { active } else { normal };
    let hint = if focused { " ^W switch pane  ^Q quit " } else { "" };

    let block = Block::bordered()
        .border_style(border_style)
        .title(if focused { " Claude Code * " } else { " Claude Code " })
        .title_bottom(Line::from(hint).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(ref session) = app.claude else {
        frame.render_widget(
            Paragraph::new("Failed to start claude CLI.\nEnsure `claude` is on PATH.")
                .style(Style::default().fg(Color::Red)),
            inner,
        );
        return;
    };

    let Ok(parser) = session.parser.lock() else { return };
    let screen = parser.screen();
    let (screen_rows, screen_cols) = screen.size();

    let lines: Vec<Line> = (0..screen_rows).map(|row| {
        let mut spans: Vec<Span> = Vec::new();
        let mut run_text = String::new();
        let mut run_style = Style::default();

        for col in 0..screen_cols {
            let (text, style) = match screen.cell(row, col) {
                None => (" ".to_string(), Style::default()),
                Some(cell) => {
                    let c = cell.contents();
                    let t = if c.is_empty() { " ".to_string() } else { c };
                    (t, cell_to_style(cell))
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
    }).collect();

    frame.render_widget(Paragraph::new(lines), inner);

    // Show cursor
    if focused {
        let (crow, ccol) = screen.cursor_position();
        let cx = inner.x.saturating_add(ccol);
        let cy = inner.y.saturating_add(crow);
        if cx < inner.x + inner.width && cy < inner.y + inner.height {
            frame.set_cursor_position((cx, cy));
        }
    }
}

fn cell_to_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();
    if let Some(fg) = vt100_color(cell.fgcolor()) { style = style.fg(fg); }
    if let Some(bg) = vt100_color(cell.bgcolor()) { style = style.bg(bg); }
    if cell.bold()      { style = style.add_modifier(Modifier::BOLD); }
    if cell.italic()    { style = style.add_modifier(Modifier::ITALIC); }
    if cell.underline() { style = style.add_modifier(Modifier::UNDERLINED); }
    if cell.inverse()   { style = style.add_modifier(Modifier::REVERSED); }
    style
}

fn vt100_color(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(match i {
            0 => Color::Black,        1 => Color::Red,
            2 => Color::Green,        3 => Color::Yellow,
            4 => Color::Blue,         5 => Color::Magenta,
            6 => Color::Cyan,         7 => Color::White,
            8 => Color::DarkGray,     9 => Color::LightRed,
            10 => Color::LightGreen,  11 => Color::LightYellow,
            12 => Color::LightBlue,   13 => Color::LightMagenta,
            14 => Color::LightCyan,   15 => Color::Gray,
            n => Color::Indexed(n),
        }),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

// ── Git pane ──────────────────────────────────────────────────────────────

fn render_git_pane(app: &App, frame: &mut Frame, area: Rect, focused: bool, normal: Style, active: Style) {
    let border_style = if focused { active } else { normal };
    let hint = if focused {
        match (app.git_tab, app.fullscreen) {
            (GitTab::Diff, true)  => " ↑↓/j/k scroll  e explorer  ^F exit fullscreen  ^R refresh ",
            (GitTab::Diff, false) => " ←→ tabs  ↑↓ file  j/k scroll  e explorer  ^F fullscreen  ^R refresh ",
            _                    => " ←→ tabs  ↑↓ scroll  ^R refresh ",
        }
    } else { "" };

    let block = Block::bordered()
        .border_style(border_style)
        .title_bottom(Line::from(hint).right_aligned());
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

    let diff_area = if app.diff_show_list {
        let [list_area, div_area, content_area] = Layout::horizontal([
            Constraint::Percentage(20), Constraint::Length(1), Constraint::Min(0),
        ]).areas(area);

        let items: Vec<ListItem> = app.file_diffs.iter().map(|f| ListItem::new(f.filename.as_str())).collect();
        let list = List::new(items)
            .highlight_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::REVERSED))
            .highlight_symbol("▶ ");
        let mut list_state = ListState::default().with_selected(Some(app.diff_file_idx));
        frame.render_stateful_widget(list, list_area, &mut list_state);
        frame.render_widget(Block::default().borders(Borders::LEFT).border_style(border_style), div_area);
        content_area
    } else { area };

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
    let max_no = rows.iter().flat_map(|r| [r.left_no, r.right_no]).flatten().max().unwrap_or(1);
    let no_w = max_no.to_string().len();

    let [left_area, div_area, right_area, sb_area] = Layout::horizontal([
        Constraint::Percentage(50), Constraint::Length(1),
        Constraint::Percentage(50), Constraint::Length(1),
    ]).areas(area);

    frame.render_widget(Block::default().borders(Borders::LEFT).border_style(border_style), div_area);

    let height = left_area.height as usize;
    let start = scroll as usize;
    let end = (start + height).min(total);

    let (mut left_lines, mut right_lines) = (Vec::with_capacity(height), Vec::with_capacity(height));
    for row in rows.get(start..end).unwrap_or(&[]) {
        let (l, r) = sbs_row_to_lines(row, no_w);
        left_lines.push(l); right_lines.push(r);
    }

    frame.render_widget(Paragraph::new(left_lines), left_area);
    frame.render_widget(Paragraph::new(right_lines), right_area);

    let mut sb = ScrollbarState::new(total.saturating_sub(height)).position(scroll as usize);
    frame.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight).style(border_style), sb_area, &mut sb);
}

fn sbs_row_to_lines(row: &SbsRow, no_w: usize) -> (Line<'static>, Line<'static>) {
    let no_s = Style::default().fg(Color::DarkGray);
    let fmt_no = |n: Option<usize>| format!("{:>no_w$} ", n.map(|v| v.to_string()).unwrap_or_default());

    match row.kind {
        SbsKind::Header => {
            let s = Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD);
            (Line::from(Span::styled(row.left.clone(), s)), Line::from(Span::styled(row.right.clone(), s)))
        }
        SbsKind::Context => (
            Line::from(vec![Span::styled(fmt_no(row.left_no), no_s),  Span::raw(row.left.clone())]),
            Line::from(vec![Span::styled(fmt_no(row.right_no), no_s), Span::raw(row.right.clone())]),
        ),
        SbsKind::Removed => (
            Line::from(vec![Span::styled(fmt_no(row.left_no), no_s), Span::styled(row.left.clone(), Style::default().fg(Color::Red))]),
            Line::from(Span::raw("")),
        ),
        SbsKind::Added => (
            Line::from(Span::raw("")),
            Line::from(vec![Span::styled(fmt_no(row.right_no), no_s), Span::styled(row.right.clone(), Style::default().fg(Color::Green))]),
        ),
        SbsKind::Changed => (
            Line::from(vec![Span::styled(fmt_no(row.left_no), no_s),  Span::styled(row.left.clone(),  Style::default().fg(Color::Red))]),
            Line::from(vec![Span::styled(fmt_no(row.right_no), no_s), Span::styled(row.right.clone(), Style::default().fg(Color::Green))]),
        ),
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
    let style = if line.starts_with('+') && !line.starts_with("+++") { Style::default().fg(Color::Green) }
        else if line.starts_with('-') && !line.starts_with("---") { Style::default().fg(Color::Red) }
        else if line.starts_with("@@") { Style::default().fg(Color::Cyan) }
        else if line.starts_with("diff ") || line.starts_with("index ") { Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD) }
        else { Style::default() };
    Line::from(Span::styled(line, style))
}

