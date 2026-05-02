use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
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
enum GitTab { Diff, Worktrees, Branches, Log }

impl GitTab {
    const ALL: [GitTab; 4] = [GitTab::Diff, GitTab::Worktrees, GitTab::Branches, GitTab::Log];
    fn title(self) -> &'static str {
        match self {
            GitTab::Diff => " Diff ", GitTab::Worktrees => " Worktrees ",
            GitTab::Branches => " Branches ", GitTab::Log => " Log ",
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

fn spawn_claude_session_in(path: &Path) -> Option<ClaudeSession> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }).ok()?;
    let mut cmd = CommandBuilder::new("claude");
    cmd.cwd(path);
    let child = pair.slave.spawn_command(cmd).ok()?;
    drop(pair.slave);
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
            Ok(n) => { if let Ok(mut p) = parser.lock() { p.process(&buf[..n]); } }
        }
    }
}

// ── Worktree parsing ──────────────────────────────────────────────────────

#[derive(Clone)]
struct Worktree {
    path: PathBuf,
    name: String, // branch short name or "(detached)"
}

fn parse_worktrees(raw: &str) -> Vec<Worktree> {
    raw.split("\n\n")
        .filter(|s| !s.trim().is_empty())
        .filter_map(|block| {
            let mut path: Option<PathBuf> = None;
            let mut name: Option<String> = None;
            let mut detached = false;
            for line in block.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    path = Some(PathBuf::from(p.trim()));
                } else if let Some(b) = line.strip_prefix("branch ") {
                    name = Some(b.trim().trim_start_matches("refs/heads/").to_string());
                } else if line.trim() == "detached" || line.trim() == "bare" {
                    detached = true;
                }
            }
            let path = path?;
            let name = name.unwrap_or_else(|| {
                if detached { "(detached)".to_string() }
                else { path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "?".to_string()) }
            });
            Some(Worktree { path, name })
        })
        .collect()
}

// ── Branch parsing ───────────────────────────────────────────────────────

#[derive(Clone)]
struct Branch {
    name: String,
    is_current: bool,
}

fn parse_branches(raw: &str) -> Vec<Branch> {
    raw.lines()
        .filter(|l| { let t = l.trim(); !t.is_empty() && !t.contains("remotes/") })
        .filter_map(|line| {
            let is_current = line.starts_with("* ");
            let rest = line.strip_prefix("* ").or_else(|| line.strip_prefix("  "))?;
            let name = rest.split_whitespace().next()?.to_string();
            if name.is_empty() { return None; }
            Some(Branch { name, is_current })
        })
        .collect()
}

// ── Diff parsing ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum SbsKind { Context, Removed, Added, Changed, Header }

#[derive(Clone)]
struct SbsRow {
    left_no: Option<usize>, left: String,
    right_no: Option<usize>, right: String,
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
    for i in 0..rem.len().max(add.len()) {
        let kind = match (i < rem.len(), i < add.len()) {
            (true, true) => SbsKind::Changed, (true, false) => SbsKind::Removed, _ => SbsKind::Added,
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
        } else { content.push('\n'); content.push_str(line); }
    }
    if !filename.is_empty() {
        result.push(FileDiff { filename, sbs: parse_side_by_side(&content), content });
    }
    result
}

// ── Window ────────────────────────────────────────────────────────────────

struct Window {
    name: String,
    path: PathBuf,
    claude: Option<ClaudeSession>,
    focused: Pane,
    git_tab: GitTab,
    scroll_offsets: [u16; 4],
    file_diffs: Vec<FileDiff>,
    diff_file_idx: usize,
    diff_content_scroll: u16,
    diff_show_list: bool,
    fullscreen: bool,
    git_log: String,
    git_worktrees: Vec<Worktree>,
    worktree_selected: usize,
    git_branches: Vec<Branch>,
    branch_selected: usize,
}

impl Window {
    fn new(path: PathBuf, name: String) -> Self {
        let claude = spawn_claude_session_in(&path);
        let mut win = Self {
            name, path,
            claude,
            focused: Pane::Claude,
            git_tab: GitTab::Diff,
            scroll_offsets: [0; 4],
            file_diffs: Vec::new(),
            diff_file_idx: 0,
            diff_content_scroll: 0,
            diff_show_list: true,
            fullscreen: false,
            git_log: String::new(),
            git_worktrees: Vec::new(),
            worktree_selected: 0,
            git_branches: Vec::new(),
            branch_selected: 0,
        };
        win.refresh();
        win
    }

    fn refresh(&mut self) {
        self.git_log = run_in(&self.path, "git", &["log", "--oneline", "--graph", "--decorate", "--color=never", "-100"]);
        self.git_worktrees = parse_worktrees(&run_in(&self.path, "git", &["worktree", "list", "--porcelain"]));
        self.git_branches = parse_branches(&run_in(&self.path, "git", &["branch", "-a", "-vv"]));
        self.branch_selected = self.branch_selected.min(self.git_branches.len().saturating_sub(1));
        self.file_diffs = parse_diff(&run_in(&self.path, "git", &["diff"]));
        self.scroll_offsets = [0; 4];
        self.diff_file_idx = 0;
        self.diff_content_scroll = 0;
        self.worktree_selected = self.worktree_selected.min(self.git_worktrees.len().saturating_sub(1));
    }

    fn resize_pty(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if let Some(ref mut s) = self.claude {
            let _ = s.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
            if let Ok(mut p) = s.parser.lock() { p.set_size(rows, cols); }
        }
    }

    fn simple_tab_content(&self) -> &str {
        match self.git_tab {
            GitTab::Log => &self.git_log,
            _ => "",
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

// ── App ───────────────────────────────────────────────────────────────────

enum InputMode { NewWorktree, ConfirmDelete(PathBuf, String), ConfirmDeleteBranch(String) }

struct App {
    windows: Vec<Window>,
    active: usize,
    should_quit: bool,
    input_mode: Option<InputMode>,
    input_buf: String,
}

impl App {
    fn new() -> Self {
        let path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let name = branch_name_for(&path);
        let win = Window::new(path, name);
        Self { windows: vec![win], active: 0, should_quit: false, input_mode: None, input_buf: String::new() }
    }

    fn win(&self) -> &Window { &self.windows[self.active] }
    fn win_mut(&mut self) -> &mut Window { &mut self.windows[self.active] }

    fn open_window(&mut self, path: PathBuf) {
        if let Some(idx) = self.windows.iter().position(|w| w.path == path) {
            self.active = idx;
            return;
        }
        let name = branch_name_for(&path);
        self.windows.push(Window::new(path, name));
        self.active = self.windows.len() - 1;
    }

    fn close_window(&mut self, idx: usize) {
        if let Some(win) = self.windows.get_mut(idx) {
            if let Some(ref mut s) = win.claude { let _ = s.child.kill(); }
        }
        self.windows.remove(idx);
        if self.windows.is_empty() {
            self.should_quit = true;
        } else {
            self.active = self.active.min(self.windows.len() - 1);
        }
    }

    fn next_window(&mut self) { self.active = (self.active + 1) % self.windows.len(); }
    fn prev_window(&mut self) { self.active = (self.active + self.windows.len() - 1) % self.windows.len(); }

    fn delete_worktree(&mut self, path: &Path, name: &str) {
        // Close any window open on this worktree first
        if let Some(idx) = self.windows.iter().position(|w| w.path == path) {
            self.close_window(idx);
        }
        let base = self.win().path.clone();
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force", path.to_str().unwrap_or(name)])
            .current_dir(&base)
            .output();
        self.win_mut().refresh();
    }

    fn delete_branch(&mut self, name: &str) {
        let base = self.win().path.clone();
        let _ = Command::new("git").args(["branch", "-d", name]).current_dir(&base).output();
        self.win_mut().refresh();
    }

    fn create_worktree(&mut self, name: &str) {
        let base = self.win().path.clone();
        let target = base.parent().unwrap_or(&base).join(name);
        let ok = Command::new("git")
            .args(["worktree", "add", "-b", name, target.to_str().unwrap_or(name)])
            .current_dir(&base)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            self.win_mut().refresh();
            self.open_window(target);
        }
    }

    fn open_paths(&self) -> HashSet<PathBuf> {
        self.windows.iter().map(|w| w.path.clone()).collect()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn run_in(dir: &Path, cmd: &str, args: &[&str]) -> String {
    Command::new(cmd).args(args).current_dir(dir).output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr).into_owned();
            if out.is_empty() && !err.is_empty() { err } else { out }
        })
        .unwrap_or_else(|e| format!("error: {e}"))
}

fn branch_name_for(path: &Path) -> String {
    Command::new("git")
        .args(["symbolic-ref", "--short", "HEAD"])
        .current_dir(path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "main".to_string())
        })
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

    let size = terminal.size()?;
    let content_rows = size.height.saturating_sub(1); // minus window tab bar
    let pty_cols = (size.width / 2).saturating_sub(2);
    let pty_rows = content_rows.saturating_sub(2);
    for win in &mut app.windows { win.resize_pty(pty_cols, pty_rows); }

    while !app.should_quit {
        terminal.draw(|frame| render(&app, frame))?;
        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                Event::Key(key) => handle_key(&mut app, key),
                Event::Resize(cols, rows) => handle_resize(&mut app, cols, rows),
                _ => {}
            }
        }
    }

    for win in &mut app.windows {
        if let Some(ref mut s) = win.claude { let _ = s.child.kill(); }
    }
    Ok(())
}

// ── Events ────────────────────────────────────────────────────────────────

fn handle_resize(app: &mut App, total_cols: u16, total_rows: u16) {
    let content_rows = total_rows.saturating_sub(1);
    for win in &mut app.windows {
        let (pty_cols, pty_rows) = if win.fullscreen && win.focused == Pane::Claude {
            (total_cols.saturating_sub(2), content_rows.saturating_sub(2))
        } else {
            ((total_cols / 2).saturating_sub(2), content_rows.saturating_sub(2))
        };
        win.resize_pty(pty_cols, pty_rows);
    }
}

fn handle_key(app: &mut App, key: KeyEvent) {
    if key.kind != KeyEventKind::Press { return; }

    // Input prompt mode
    if app.input_mode.is_some() {
        match key.code {
            KeyCode::Esc => { app.input_mode = None; app.input_buf.clear(); }
            KeyCode::Enter => {
                match app.input_mode.take() {
                    Some(InputMode::NewWorktree) => {
                        let name = app.input_buf.trim().to_string();
                        app.input_buf.clear();
                        if !name.is_empty() { app.create_worktree(&name); }
                    }
                    Some(InputMode::ConfirmDelete(_, _)) | Some(InputMode::ConfirmDeleteBranch(_)) => { app.input_buf.clear(); }
                    None => {}
                }
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                match app.input_mode.take() {
                    Some(InputMode::ConfirmDelete(path, name)) => {
                        app.input_buf.clear();
                        app.delete_worktree(&path, &name);
                    }
                    Some(InputMode::ConfirmDeleteBranch(name)) => {
                        app.input_buf.clear();
                        app.delete_branch(&name);
                    }
                    _ => {}
                }
            }
            KeyCode::Char(c @ 'n') | KeyCode::Char(c @ 'N') => {
                if matches!(app.input_mode, Some(InputMode::ConfirmDelete(_, _))) {
                    app.input_mode = None;
                    app.input_buf.clear();
                } else if matches!(app.input_mode, Some(InputMode::NewWorktree))
                    && !key.modifiers.contains(KeyModifiers::CONTROL) {
                    app.input_buf.push(c);
                }
            }
            KeyCode::Backspace => {
                if matches!(app.input_mode, Some(InputMode::NewWorktree)) { app.input_buf.pop(); }
            }
            KeyCode::Char(c) if matches!(app.input_mode, Some(InputMode::NewWorktree))
                && !key.modifiers.contains(KeyModifiers::CONTROL) => { app.input_buf.push(c); }
            _ => {}
        }
        return;
    }

    // Global bindings
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), KeyModifiers::CONTROL) => { app.should_quit = true; return; }
        // Ctrl+W switches Claude↔Git pane
        (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
            let win = app.win_mut();
            win.focused = match win.focused { Pane::Claude => Pane::Git, Pane::Git => Pane::Claude };
            return;
        }
        // Window cycling (Ctrl+N / Ctrl+P)
        (KeyCode::Char('n'), KeyModifiers::CONTROL) => { app.next_window(); return; }
        (KeyCode::Char('p'), KeyModifiers::CONTROL) => { app.prev_window(); return; }
        // Close current window
        (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
            let idx = app.active;
            app.close_window(idx);
            return;
        }
        _ => {}
    }

    match app.win().focused {
        Pane::Claude => {
            if let Some(ref mut s) = app.win_mut().claude {
                let bytes = key_to_bytes(&key);
                if !bytes.is_empty() { let _ = s.writer.write_all(&bytes); }
            }
        }
        Pane::Git => handle_git_key(app, key),
    }
}

fn handle_git_key(app: &mut App, key: KeyEvent) {
    use KeyCode::*;
    use KeyModifiers as KM;

    // Operations that need &mut App (window management)
    if app.win().git_tab == GitTab::Worktrees {
        match (key.code, key.modifiers) {
            (Enter, _) => {
                let path = app.win().git_worktrees.get(app.win().worktree_selected).map(|wt| wt.path.clone());
                if let Some(p) = path { app.open_window(p); }
                return;
            }
            (Char('t'), KM::CONTROL) => {
                app.input_mode = Some(InputMode::NewWorktree);
                app.input_buf.clear();
                return;
            }
            (Char('d'), KM::NONE) => {
                let sel = app.win().worktree_selected;
                if let Some(wt) = app.win().git_worktrees.get(sel) {
                    // Don't allow deleting the main worktree (same path as current window's repo root)
                    if wt.path != app.win().path {
                        app.input_mode = Some(InputMode::ConfirmDelete(wt.path.clone(), wt.name.clone()));
                    }
                }
                return;
            }
            _ => {}
        }
    }

    if app.win().git_tab == GitTab::Branches {
        if let (Char('d'), KM::NONE) = (key.code, key.modifiers) {
            let sel = app.win().branch_selected;
            if let Some(branch) = app.win().git_branches.get(sel) {
                if !branch.is_current {
                    app.input_mode = Some(InputMode::ConfirmDeleteBranch(branch.name.clone()));
                }
            }
            return;
        }
    }

    match (key.code, key.modifiers) {
        (Char('c'), KM::CONTROL) | (Char('q'), KM::NONE) => app.should_quit = true,
        _ => {}
    }
    let win = app.win_mut();
    match (key.code, key.modifiers) {
        (Char('f'), KM::CONTROL) => { win.fullscreen = !win.fullscreen; win.diff_content_scroll = 0; }
        (Char('r'), KM::CONTROL) => win.refresh(),
        (Char('e'), KM::NONE) if win.git_tab == GitTab::Diff => win.diff_show_list = !win.diff_show_list,
        (Right, _) => win.git_tab = win.git_tab.next(),
        (Left,  _) => win.git_tab = win.git_tab.prev(),
        // Worktrees tab navigation
        (Up, _) if win.git_tab == GitTab::Worktrees => {
            win.worktree_selected = win.worktree_selected.saturating_sub(1);
        }
        (Down, _) if win.git_tab == GitTab::Worktrees => {
            let max = win.git_worktrees.len().saturating_sub(1);
            win.worktree_selected = (win.worktree_selected + 1).min(max);
        }
        // Branches tab navigation
        (Up, _) if win.git_tab == GitTab::Branches => {
            win.branch_selected = win.branch_selected.saturating_sub(1);
        }
        (Down, _) if win.git_tab == GitTab::Branches => {
            let max = win.git_branches.len().saturating_sub(1);
            win.branch_selected = (win.branch_selected + 1).min(max);
        }
        // Diff tab navigation
        (Up, _) if win.git_tab == GitTab::Diff && win.diff_show_list => win.diff_select_prev(),
        (Down, _) if win.git_tab == GitTab::Diff && win.diff_show_list => win.diff_select_next(),
        (Up, _)   => win.scroll_up(1),
        (Down, _) => win.scroll_down(1),
        (Char('k'), KM::NONE) => win.scroll_up(20),
        (Char('j'), KM::NONE) => win.scroll_down(20),
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
                let mut v = vec![0x1B]; let mut tmp = [0u8; 4];
                v.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes()); v
            } else {
                let mut tmp = [0u8; 4]; c.encode_utf8(&mut tmp).as_bytes().to_vec()
            }
        }
        Enter => vec![b'\r'], Backspace => vec![0x7F], Delete => vec![0x1B, b'[', b'3', b'~'],
        Esc => vec![0x1B], Tab => vec![b'\t'], BackTab => vec![0x1B, b'[', b'Z'],
        Up => vec![0x1B, b'[', b'A'], Down => vec![0x1B, b'[', b'B'],
        Right => vec![0x1B, b'[', b'C'], Left => vec![0x1B, b'[', b'D'],
        Home => vec![0x1B, b'[', b'H'], End => vec![0x1B, b'[', b'F'],
        PageUp => vec![0x1B, b'[', b'5', b'~'], PageDown => vec![0x1B, b'[', b'6', b'~'],
        Insert => vec![0x1B, b'[', b'2', b'~'],
        F(1) => vec![0x1B, b'O', b'P'], F(2) => vec![0x1B, b'O', b'Q'],
        F(3) => vec![0x1B, b'O', b'R'], F(4) => vec![0x1B, b'O', b'S'],
        F(5) => vec![0x1B, b'[', b'1', b'5', b'~'], F(6) => vec![0x1B, b'[', b'1', b'7', b'~'],
        F(7) => vec![0x1B, b'[', b'1', b'8', b'~'], F(8) => vec![0x1B, b'[', b'1', b'9', b'~'],
        F(9) => vec![0x1B, b'[', b'2', b'0', b'~'], F(10) => vec![0x1B, b'[', b'2', b'1', b'~'],
        F(11) => vec![0x1B, b'[', b'2', b'3', b'~'], F(12) => vec![0x1B, b'[', b'2', b'4', b'~'],
        _ => vec![],
    }
}

// ── Rendering ─────────────────────────────────────────────────────────────

fn render(app: &App, frame: &mut Frame) {
    let [winbar_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(frame.area());

    render_window_bar(app, frame, winbar_area);

    let win = app.win();
    let normal = Style::default().fg(Color::Blue);
    let active = Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD);

    if win.fullscreen {
        match win.focused {
            Pane::Claude => render_claude_pane(win, frame, content_area, true, normal, active),
            Pane::Git    => render_git_pane(win, frame, content_area, true, normal, active, &app.open_paths()),
        }
        return;
    }

    let [left, right] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .areas(content_area);
    render_claude_pane(win, frame, left,  win.focused == Pane::Claude, normal, active);
    render_git_pane(win, frame, right, win.focused == Pane::Git, normal, active, &app.open_paths());

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
        .alignment(ratatui::layout::Alignment::Center);
    frame.render_widget(title, area);
}

fn render_input_prompt(app: &App, frame: &mut Frame) {
    let area = frame.area();
    let w = 54u16.min(area.width.saturating_sub(4));
    let h = 5u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let dialog = Rect { x, y, width: w, height: h };

    frame.render_widget(ratatui::widgets::Clear, dialog);

    match &app.input_mode {
        Some(InputMode::NewWorktree) => {
            let block = Block::bordered()
                .border_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD))
                .title(" New Worktree ");
            let inner = block.inner(dialog);
            frame.render_widget(block, dialog);
            let [label_area, input_area] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
            frame.render_widget(Paragraph::new("Branch name (worktree created at ../name):"), label_area);
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
            let [label_area, confirm_area] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
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
            let [label_area, confirm_area] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);
            frame.render_widget(Paragraph::new(format!("Delete branch '{name}'?")), label_area);
            frame.render_widget(
                Paragraph::new("Press y to confirm, n or Esc to cancel")
                    .style(Style::default().fg(Color::DarkGray)),
                confirm_area,
            );
        }
        None => {}
    }
}

// ── Claude pane ───────────────────────────────────────────────────────────

fn render_claude_pane(win: &Window, frame: &mut Frame, area: Rect, focused: bool, normal: Style, active: Style) {
    let border_style = if focused { active } else { normal };
    let hint = if focused { " ^W pane  ^W→Git  ^]close  ^Q quit " } else { "" };
    let title = if focused { format!(" {} * ", win.name) } else { format!(" {} ", win.name) };

    let block = Block::bordered()
        .border_style(border_style)
        .title(title)
        .title_bottom(Line::from(hint).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(ref session) = win.claude else {
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
                    (if c.is_empty() { " ".to_string() } else { c }, cell_to_style(cell))
                }
            };
            if style == run_style {
                run_text.push_str(&text);
            } else {
                if !run_text.is_empty() { spans.push(Span::styled(std::mem::take(&mut run_text), run_style)); }
                run_text = text; run_style = style;
            }
        }
        if !run_text.is_empty() { spans.push(Span::styled(run_text, run_style)); }
        Line::from(spans)
    }).collect();

    frame.render_widget(Paragraph::new(lines), inner);

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
            0 => Color::Black,       1 => Color::Red,         2 => Color::Green,
            3 => Color::Yellow,      4 => Color::Blue,        5 => Color::Magenta,
            6 => Color::Cyan,        7 => Color::White,       8 => Color::DarkGray,
            9 => Color::LightRed,   10 => Color::LightGreen, 11 => Color::LightYellow,
            12 => Color::LightBlue, 13 => Color::LightMagenta, 14 => Color::LightCyan,
            15 => Color::Gray,       n => Color::Indexed(n),
        }),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

// ── Git pane ──────────────────────────────────────────────────────────────

fn render_git_pane(win: &Window, frame: &mut Frame, area: Rect, focused: bool, normal: Style, active: Style, open_paths: &HashSet<PathBuf>) {
    let border_style = if focused { active } else { normal };
    let hint = if focused {
        match (win.git_tab, win.fullscreen) {
            (GitTab::Diff, true)      => " ↑↓/j/k scroll  e explorer  ^F exit fullscreen  ^R refresh ",
            (GitTab::Diff, false)     => " ←→ tabs  ↑↓ file  j/k scroll  e explorer  ^F fullscreen  ^R refresh ",
            (GitTab::Worktrees, _)    => " ←→ tabs  ↑↓ select  Enter open  ^T new  d delete  ^R refresh ",
            (GitTab::Branches, _)    => " ←→ tabs  ↑↓ select  d delete  ^R refresh ",
            _                        => " ←→ tabs  ↑↓ scroll  ^R refresh ",
        }
    } else { "" };

    let block = Block::bordered()
        .border_style(border_style)
        .title_bottom(Line::from(hint).right_aligned());
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [tabs_area, sep_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(inner);

    let tab_style = if focused { Style::default().fg(Color::Blue) } else { Style::default().fg(Color::DarkGray) };
    let tabs = Tabs::new(GitTab::ALL.iter().map(|t| t.title()).collect::<Vec<_>>())
        .select(win.git_tab.index())
        .style(tab_style)
        .highlight_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::UNDERLINED))
        .divider("│");
    frame.render_widget(tabs, tabs_area);
    frame.render_widget(Block::default().borders(Borders::TOP).border_style(border_style), sep_area);

    match win.git_tab {
        GitTab::Diff      => render_diff_tab(win, frame, content_area, focused, border_style),
        GitTab::Worktrees => render_worktrees_tab(win, frame, content_area, border_style, open_paths),
        GitTab::Branches  => render_branches_tab(win, frame, content_area),
        GitTab::Log       => render_scrollable_text(win.simple_tab_content(), win.current_scroll(), frame, content_area, border_style),
    }
}

fn render_worktrees_tab(win: &Window, frame: &mut Frame, area: Rect, _border_style: Style, open_paths: &HashSet<PathBuf>) {
    if win.git_worktrees.is_empty() {
        frame.render_widget(Paragraph::new("No worktrees found."), area);
        return;
    }

    let items: Vec<ListItem> = win.git_worktrees.iter().map(|wt| {
        let is_open = open_paths.contains(&wt.path);
        let label = if is_open {
            format!("{} [open]", wt.name)
        } else {
            wt.name.clone()
        };
        ListItem::new(label)
    }).collect();

    let list = List::new(items)
        .highlight_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .highlight_symbol("▶ ");
    let mut state = ListState::default().with_selected(Some(win.worktree_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_branches_tab(win: &Window, frame: &mut Frame, area: Rect) {
    if win.git_branches.is_empty() {
        frame.render_widget(Paragraph::new("No branches found."), area);
        return;
    }
    let items: Vec<ListItem> = win.git_branches.iter().map(|b| {
        let marker = if b.is_current { "* " } else { "  " };
        let style = if b.is_current {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        ListItem::new(format!("{}{}", marker, b.name)).style(style)
    }).collect();
    let list = List::new(items)
        .highlight_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::REVERSED))
        .highlight_symbol("▶ ");
    let mut state = ListState::default().with_selected(Some(win.branch_selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_diff_tab(win: &Window, frame: &mut Frame, area: Rect, focused: bool, border_style: Style) {
    if win.file_diffs.is_empty() {
        frame.render_widget(Paragraph::new(if focused { "No changes in working tree." } else { "" }), area);
        return;
    }

    let diff_area = if win.diff_show_list {
        let [list_area, div_area, content_area] = Layout::horizontal([
            Constraint::Percentage(20), Constraint::Length(1), Constraint::Min(0),
        ]).areas(area);

        let items: Vec<ListItem> = win.file_diffs.iter().map(|f| ListItem::new(f.filename.as_str())).collect();
        let list = List::new(items)
            .highlight_style(Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD | Modifier::REVERSED))
            .highlight_symbol("▶ ");
        let mut list_state = ListState::default().with_selected(Some(win.diff_file_idx));
        frame.render_stateful_widget(list, list_area, &mut list_state);
        frame.render_widget(Block::default().borders(Borders::LEFT).border_style(border_style), div_area);
        content_area
    } else { area };

    if let Some(fd) = win.file_diffs.get(win.diff_file_idx) {
        if win.fullscreen {
            render_sbs(fd, win.diff_content_scroll, frame, diff_area, border_style);
        } else {
            render_unified_diff(&fd.content, win.diff_content_scroll, frame, diff_area, border_style);
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
    let (mut ll, mut rl) = (Vec::with_capacity(height), Vec::with_capacity(height));
    for row in rows.get(start..end).unwrap_or(&[]) {
        let (l, r) = sbs_row_to_lines(row, no_w);
        ll.push(l); rl.push(r);
    }
    frame.render_widget(Paragraph::new(ll), left_area);
    frame.render_widget(Paragraph::new(rl), right_area);

    let mut sb = ScrollbarState::new(total.saturating_sub(height)).position(scroll as usize);
    frame.render_stateful_widget(Scrollbar::new(ScrollbarOrientation::VerticalRight).style(border_style), sb_area, &mut sb);
}

fn sbs_row_to_lines(row: &SbsRow, no_w: usize) -> (Line<'static>, Line<'static>) {
    let no_s = Style::default().fg(Color::DarkGray);
    let fmt = |n: Option<usize>| format!("{:>no_w$} ", n.map(|v| v.to_string()).unwrap_or_default());
    match row.kind {
        SbsKind::Header => {
            let s = Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD);
            (Line::from(Span::styled(row.left.clone(), s)), Line::from(Span::styled(row.right.clone(), s)))
        }
        SbsKind::Context => (
            Line::from(vec![Span::styled(fmt(row.left_no), no_s),  Span::raw(row.left.clone())]),
            Line::from(vec![Span::styled(fmt(row.right_no), no_s), Span::raw(row.right.clone())]),
        ),
        SbsKind::Removed => (
            Line::from(vec![Span::styled(fmt(row.left_no), no_s), Span::styled(row.left.clone(), Style::default().fg(Color::Red))]),
            Line::from(Span::raw("")),
        ),
        SbsKind::Added => (
            Line::from(Span::raw("")),
            Line::from(vec![Span::styled(fmt(row.right_no), no_s), Span::styled(row.right.clone(), Style::default().fg(Color::Green))]),
        ),
        SbsKind::Changed => (
            Line::from(vec![Span::styled(fmt(row.left_no),  no_s), Span::styled(row.left.clone(),  Style::default().fg(Color::Red))]),
            Line::from(vec![Span::styled(fmt(row.right_no), no_s), Span::styled(row.right.clone(), Style::default().fg(Color::Green))]),
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
