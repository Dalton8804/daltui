use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use portable_pty::PtySize;
use ratatui::crossterm::event::{self, Event};

use crate::git::{parse_branches, parse_diff, parse_worktrees, Branch, FileDiff, Worktree};
use crate::pty::{spawn_pty_session, PtySession};
use crate::util::{branch_name_for, run_in};
use crate::{events, ui};

// ── Pane / tab state ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Claude,
    Git,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitTab {
    Diff,
    Worktrees,
    Branches,
    Log,
}

impl GitTab {
    pub const ALL: [GitTab; 4] = [GitTab::Diff, GitTab::Worktrees, GitTab::Branches, GitTab::Log];

    pub fn title(self) -> &'static str {
        match self {
            GitTab::Diff => " Diff ",
            GitTab::Worktrees => " Worktrees ",
            GitTab::Branches => " Branches ",
            GitTab::Log => " Log ",
        }
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap()
    }

    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    pub fn prev(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }
}

// ── Input mode ────────────────────────────────────────────────────────────

pub enum InputMode {
    NewWorktree,
    ConfirmDelete(PathBuf, String),
    ConfirmDeleteBranch(String),
}

// ── Window ────────────────────────────────────────────────────────────────

pub struct Window {
    pub name: String,
    pub path: PathBuf,
    pub claude: Option<PtySession>,
    pub terminal: Option<PtySession>,
    pub focused: Pane,
    pub git_tab: GitTab,
    pub claude_scroll: usize,
    pub scroll_offsets: [u16; 4],
    pub file_diffs: Vec<FileDiff>,
    pub diff_file_idx: usize,
    pub diff_content_scroll: u16,
    pub diff_show_list: bool,
    pub fullscreen: bool,
    pub git_log: String,
    pub git_worktrees: Vec<Worktree>,
    pub worktree_selected: usize,
    pub git_branches: Vec<Branch>,
    pub branch_selected: usize,
}

impl Window {
    pub fn new(path: PathBuf, name: String) -> Self {
        let claude = spawn_pty_session(
            &path,
            "claude",
            &[],
            PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 },
            1000,
        );
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let terminal = spawn_pty_session(
            &path,
            &shell,
            &[],
            PtySize { rows: 12, cols: 80, pixel_width: 0, pixel_height: 0 },
            0,
        );
        let mut win = Self {
            name,
            path,
            claude,
            terminal,
            focused: Pane::Claude,
            git_tab: GitTab::Diff,
            claude_scroll: 0,
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

    pub fn refresh(&mut self) {
        self.git_log = run_in(
            &self.path,
            "git",
            &["log", "--oneline", "--graph", "--decorate", "--color=never", "-100"],
        );
        self.git_worktrees =
            parse_worktrees(&run_in(&self.path, "git", &["worktree", "list", "--porcelain"]));
        self.git_branches =
            parse_branches(&run_in(&self.path, "git", &["branch", "-a", "-vv"]));
        self.branch_selected =
            self.branch_selected.min(self.git_branches.len().saturating_sub(1));
        self.file_diffs = parse_diff(&run_in(&self.path, "git", &["diff"]));
        self.scroll_offsets = [0; 4];
        self.diff_file_idx = 0;
        self.diff_content_scroll = 0;
        self.worktree_selected =
            self.worktree_selected.min(self.git_worktrees.len().saturating_sub(1));
    }

    pub fn resize_claude_pty(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if let Some(ref mut s) = self.claude {
            let _ = s.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
            if let Ok(mut p) = s.parser.lock() {
                p.set_size(rows, cols);
            }
        }
    }

    pub fn resize_terminal_pty(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if let Some(ref mut s) = self.terminal {
            let _ = s.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
            if let Ok(mut p) = s.parser.lock() {
                p.set_size(rows, cols);
            }
        }
    }

    pub fn simple_tab_content(&self) -> &str {
        match self.git_tab {
            GitTab::Log => &self.git_log,
            _ => "",
        }
    }

    pub fn current_scroll(&self) -> u16 {
        match self.git_tab {
            GitTab::Diff => self.diff_content_scroll,
            t => self.scroll_offsets[t.index()],
        }
    }

    pub fn scroll_down(&mut self, amount: u16) {
        if self.git_tab == GitTab::Diff {
            let max = self
                .file_diffs
                .get(self.diff_file_idx)
                .map(|f| {
                    if self.fullscreen { f.sbs.len() } else { f.content.lines().count() }
                        .saturating_sub(1) as u16
                })
                .unwrap_or(0);
            self.diff_content_scroll = (self.diff_content_scroll + amount).min(max);
        } else {
            let i = self.git_tab.index();
            let max = self.simple_tab_content().lines().count().saturating_sub(1) as u16;
            self.scroll_offsets[i] = (self.scroll_offsets[i] + amount).min(max);
        }
    }

    pub fn scroll_up(&mut self, amount: u16) {
        if self.git_tab == GitTab::Diff {
            self.diff_content_scroll = self.diff_content_scroll.saturating_sub(amount);
        } else {
            let i = self.git_tab.index();
            self.scroll_offsets[i] = self.scroll_offsets[i].saturating_sub(amount);
        }
    }

    pub fn diff_select_next(&mut self) {
        if !self.file_diffs.is_empty() {
            self.diff_file_idx = (self.diff_file_idx + 1).min(self.file_diffs.len() - 1);
            self.diff_content_scroll = 0;
        }
    }

    pub fn diff_select_prev(&mut self) {
        self.diff_file_idx = self.diff_file_idx.saturating_sub(1);
        self.diff_content_scroll = 0;
    }
}

// ── App ───────────────────────────────────────────────────────────────────

pub struct App {
    pub windows: Vec<Window>,
    pub active: usize,
    pub should_quit: bool,
    pub input_mode: Option<InputMode>,
    pub input_buf: String,
    pub pty_dims: (u16, u16, u16), // (half_cols, claude_rows, term_rows)
}

impl App {
    pub fn new() -> Self {
        let path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let name = branch_name_for(&path);
        let win = Window::new(path, name);
        Self {
            windows: vec![win],
            active: 0,
            should_quit: false,
            input_mode: None,
            input_buf: String::new(),
            pty_dims: (80, 22, 10),
        }
    }

    pub fn run(terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
        let mut app = App::new();

        let size = terminal.size()?;
        let content_rows = size.height.saturating_sub(1);
        let half_cols = (size.width / 2).saturating_sub(2);
        let claude_rows = content_rows.saturating_sub(2);
        let term_rows = (content_rows * 40 / 100).saturating_sub(2).max(1);
        app.pty_dims = (half_cols, claude_rows, term_rows);
        for win in &mut app.windows {
            win.resize_claude_pty(half_cols, claude_rows);
            win.resize_terminal_pty(half_cols, term_rows);
        }

        while !app.should_quit {
            terminal.draw(|frame| ui::render(&app, frame))?;
            if event::poll(Duration::from_millis(16))? {
                match event::read()? {
                    Event::Key(key) => events::handle_key(&mut app, key),
                    Event::Resize(cols, rows) => events::handle_resize(&mut app, cols, rows),
                    _ => {}
                }
            }
        }

        for win in &mut app.windows {
            if let Some(ref mut s) = win.claude {
                let _ = s.child.kill();
            }
            if let Some(ref mut s) = win.terminal {
                let _ = s.child.kill();
            }
        }
        Ok(())
    }

    pub fn win(&self) -> &Window {
        &self.windows[self.active]
    }

    pub fn win_mut(&mut self) -> &mut Window {
        &mut self.windows[self.active]
    }

    pub fn open_window(&mut self, path: PathBuf) {
        if let Some(idx) = self.windows.iter().position(|w| w.path == path) {
            self.active = idx;
            return;
        }
        let name = branch_name_for(&path);
        self.windows.push(Window::new(path, name));
        self.active = self.windows.len() - 1;
        let (half_cols, claude_rows, term_rows) = self.pty_dims;
        let win = self.windows.last_mut().unwrap();
        win.resize_claude_pty(half_cols, claude_rows);
        win.resize_terminal_pty(half_cols, term_rows);
    }

    pub fn close_window(&mut self, idx: usize) {
        if let Some(win) = self.windows.get_mut(idx) {
            if let Some(ref mut s) = win.claude {
                let _ = s.child.kill();
            }
            if let Some(ref mut s) = win.terminal {
                let _ = s.child.kill();
            }
        }
        self.windows.remove(idx);
        if self.windows.is_empty() {
            self.should_quit = true;
        } else {
            self.active = self.active.min(self.windows.len() - 1);
        }
    }

    pub fn next_window(&mut self) {
        self.active = (self.active + 1) % self.windows.len();
    }

    pub fn prev_window(&mut self) {
        self.active = (self.active + self.windows.len() - 1) % self.windows.len();
    }

    pub fn delete_worktree(&mut self, path: &Path, name: &str) {
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

    pub fn delete_branch(&mut self, name: &str) {
        let base = self.win().path.clone();
        let _ = Command::new("git").args(["branch", "-d", name]).current_dir(&base).output();
        self.win_mut().refresh();
    }

    pub fn create_worktree(&mut self, name: &str) {
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

    pub fn open_paths(&self) -> HashSet<PathBuf> {
        self.windows.iter().map(|w| w.path.clone()).collect()
    }
}
