use std::io::Write;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::app::{App, GitTab, InputMode, Pane};
use crate::config::KeyConfig;

pub fn handle_resize(app: &mut App, total_cols: u16, total_rows: u16) {
    let content_rows = total_rows.saturating_sub(1);
    let half_cols = (total_cols / 2).saturating_sub(2);
    let claude_rows = content_rows.saturating_sub(2);
    let term_rows = (content_rows * 40 / 100).saturating_sub(2).max(1);
    app.pty_dims = (half_cols, claude_rows, term_rows);
    for win in &mut app.windows {
        let claude_cols = if win.fullscreen && win.focused == Pane::Claude {
            total_cols.saturating_sub(2)
        } else {
            half_cols
        };
        win.resize_claude_pty(claude_cols, claude_rows);
        if !win.fullscreen {
            win.resize_terminal_pty(half_cols, term_rows);
        }
    }
}

pub fn handle_key(app: &mut App, key: KeyEvent, kc: &KeyConfig) {
    if key.kind != KeyEventKind::Press {
        return;
    }

    if app.input_mode.is_some() {
        handle_input_mode(app, key);
        return;
    }

    if kc.global.quit.matches(key.code, key.modifiers) {
        app.should_quit = true;
        return;
    }
    if kc.global.cycle_pane.matches(key.code, key.modifiers) {
        let win = app.win_mut();
        win.focused = match win.focused {
            Pane::Claude => Pane::Git,
            Pane::Git => Pane::Terminal,
            Pane::Terminal => Pane::Claude,
        };
        return;
    }
    if kc.global.next_window.matches(key.code, key.modifiers) {
        app.next_window();
        return;
    }
    if kc.global.prev_window.matches(key.code, key.modifiers) {
        app.prev_window();
        return;
    }
    if kc.global.close_window.matches(key.code, key.modifiers) {
        let idx = app.active;
        app.close_window(idx);
        return;
    }

    match app.win().focused {
        Pane::Claude => {
            if kc.pty.scroll_up.matches(key.code, key.modifiers) {
                let clamped = probe_scroll(app, 3);
                app.win_mut().claude_scroll = clamped;
                return;
            }
            if kc.pty.scroll_down.matches(key.code, key.modifiers) {
                let s = &mut app.win_mut().claude_scroll;
                *s = s.saturating_sub(3);
                return;
            }
            if kc.pty.page_up.matches(key.code, key.modifiers) {
                let clamped = probe_scroll(app, 20);
                app.win_mut().claude_scroll = clamped;
                return;
            }
            if kc.pty.page_down.matches(key.code, key.modifiers) {
                let s = &mut app.win_mut().claude_scroll;
                *s = s.saturating_sub(20);
                return;
            }
            app.win_mut().claude_scroll = 0;
            if let Some(ref mut s) = app.win_mut().claude {
                let bytes = key_to_bytes(&key);
                if !bytes.is_empty() {
                    let _ = s.writer.write_all(&bytes);
                }
            }
        }
        Pane::Terminal => {
            if let Some(ref mut s) = app.win_mut().terminal {
                let bytes = key_to_bytes(&key);
                if !bytes.is_empty() {
                    let _ = s.writer.write_all(&bytes);
                }
            }
        }
        Pane::Git => handle_git_key(app, key, kc),
    }
}

fn handle_input_mode(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.input_mode = None;
            app.input_buf.clear();
        }
        KeyCode::Enter => match app.input_mode.take() {
            Some(InputMode::NewWorktree) => {
                let name = app.input_buf.trim().to_string();
                app.input_buf.clear();
                if !name.is_empty() {
                    app.create_worktree(&name);
                }
            }
            Some(InputMode::ConfirmDelete(_, _)) | Some(InputMode::ConfirmDeleteBranch(_)) => {
                app.input_buf.clear();
            }
            None => {}
        },
        KeyCode::Char(c @ 'y') | KeyCode::Char(c @ 'Y') => {
            if matches!(app.input_mode, Some(InputMode::NewWorktree))
                && !key.modifiers.contains(KeyModifiers::CONTROL)
            {
                app.input_buf.push(c);
            } else {
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
        }
        KeyCode::Char(c @ 'n') | KeyCode::Char(c @ 'N') => {
            if matches!(app.input_mode, Some(InputMode::ConfirmDelete(_, _)))
                || matches!(app.input_mode, Some(InputMode::ConfirmDeleteBranch(_)))
            {
                app.input_mode = None;
                app.input_buf.clear();
            } else if matches!(app.input_mode, Some(InputMode::NewWorktree))
                && !key.modifiers.contains(KeyModifiers::CONTROL)
            {
                app.input_buf.push(c);
            }
        }
        KeyCode::Backspace => {
            if matches!(app.input_mode, Some(InputMode::NewWorktree)) {
                app.input_buf.pop();
            }
        }
        KeyCode::Char(c)
            if matches!(app.input_mode, Some(InputMode::NewWorktree))
                && !key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.input_buf.push(c);
        }
        _ => {}
    }
}

fn handle_git_key(app: &mut App, key: KeyEvent, kc: &KeyConfig) {
    use KeyCode::*;
    use KeyModifiers as KM;

    if app.win().git_tab == GitTab::Worktrees {
        if kc.git.open.matches(key.code, key.modifiers) {
            let path = app
                .win()
                .git_worktrees
                .get(app.win().worktree_selected)
                .map(|wt| wt.path.clone());
            if let Some(p) = path {
                app.open_window(p);
            }
            return;
        }
        if kc.git.new_worktree.matches(key.code, key.modifiers) {
            app.input_mode = Some(InputMode::NewWorktree);
            app.input_buf.clear();
            return;
        }
        if kc.git.delete.matches(key.code, key.modifiers) {
            let sel = app.win().worktree_selected;
            if let Some(wt) = app.win().git_worktrees.get(sel) {
                if wt.path != app.win().path {
                    app.input_mode =
                        Some(InputMode::ConfirmDelete(wt.path.clone(), wt.name.clone()));
                }
            }
            return;
        }
    }

    if app.win().git_tab == GitTab::Branches {
        if kc.git.delete.matches(key.code, key.modifiers) {
            let sel = app.win().branch_selected;
            if let Some(branch) = app.win().git_branches.get(sel) {
                if !branch.is_current {
                    app.input_mode = Some(InputMode::ConfirmDeleteBranch(branch.name.clone()));
                }
            }
            return;
        }
    }

    // Ctrl+C and q as fallback quit in git pane (vim-style)
    match (key.code, key.modifiers) {
        (Char('c'), KM::CONTROL) | (Char('q'), KM::NONE) => app.should_quit = true,
        _ => {}
    }

    let win = app.win_mut();

    if kc.diff.fullscreen.matches(key.code, key.modifiers) {
        win.fullscreen = !win.fullscreen;
        win.diff_content_scroll = 0;
        return;
    }
    if kc.git.refresh.matches(key.code, key.modifiers) {
        win.refresh();
        return;
    }
    if kc.diff.explorer.matches(key.code, key.modifiers) && win.git_tab == GitTab::Diff {
        win.diff_show_list = !win.diff_show_list;
        return;
    }
    if kc.diff.scroll_down.matches(key.code, key.modifiers) {
        win.scroll_down(20);
        return;
    }
    if kc.diff.scroll_up.matches(key.code, key.modifiers) {
        win.scroll_up(20);
        return;
    }

    match (key.code, key.modifiers) {
        (Right, _) => win.git_tab = win.git_tab.next(),
        (Left, _) => win.git_tab = win.git_tab.prev(),
        (Up, _) if win.git_tab == GitTab::Worktrees => {
            win.worktree_selected = win.worktree_selected.saturating_sub(1);
        }
        (Down, _) if win.git_tab == GitTab::Worktrees => {
            let max = win.git_worktrees.len().saturating_sub(1);
            win.worktree_selected = (win.worktree_selected + 1).min(max);
        }
        (Up, _) if win.git_tab == GitTab::Branches => {
            win.branch_selected = win.branch_selected.saturating_sub(1);
        }
        (Down, _) if win.git_tab == GitTab::Branches => {
            let max = win.git_branches.len().saturating_sub(1);
            win.branch_selected = (win.branch_selected + 1).min(max);
        }
        (Up, _) if win.git_tab == GitTab::Diff && win.diff_show_list => win.diff_select_prev(),
        (Down, _) if win.git_tab == GitTab::Diff && win.diff_show_list => win.diff_select_next(),
        (Up, _) => win.scroll_up(1),
        (Down, _) => win.scroll_down(1),
        _ => {}
    }
}

fn probe_scroll(app: &App, step: usize) -> usize {
    let desired = app.win().claude_scroll.saturating_add(step);
    app.win()
        .claude
        .as_ref()
        .and_then(|s| s.parser.lock().ok())
        .map(|mut p| {
            p.set_scrollback(desired);
            let actual = p.screen().scrollback();
            let (screen_rows, _) = p.screen().size();
            p.set_scrollback(0);
            // Cap at screen_rows: visible_rows() does `take(rows_len - offset)` which
            // underflows if offset > rows_len, causing a panic.
            actual.min(screen_rows as usize)
        })
        .unwrap_or(0)
}

fn key_to_bytes(key: &KeyEvent) -> Vec<u8> {
    use KeyCode::*;
    use KeyModifiers as KM;
    match key.code {
        Char(c) => {
            if key.modifiers.contains(KM::CONTROL) {
                let b = (c as u8).to_ascii_uppercase();
                vec![if (b'A'..=b'Z').contains(&b) { b - b'A' + 1 } else { c as u8 & 0x1F }]
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
        Enter => vec![b'\r'],
        Backspace => vec![0x7F],
        Delete => vec![0x1B, b'[', b'3', b'~'],
        Esc => vec![0x1B],
        Tab => vec![b'\t'],
        BackTab => vec![0x1B, b'[', b'Z'],
        Up => vec![0x1B, b'[', b'A'],
        Down => vec![0x1B, b'[', b'B'],
        Right => vec![0x1B, b'[', b'C'],
        Left => vec![0x1B, b'[', b'D'],
        Home => vec![0x1B, b'[', b'H'],
        End => vec![0x1B, b'[', b'F'],
        PageUp => vec![0x1B, b'[', b'5', b'~'],
        PageDown => vec![0x1B, b'[', b'6', b'~'],
        Insert => vec![0x1B, b'[', b'2', b'~'],
        F(1) => vec![0x1B, b'O', b'P'],
        F(2) => vec![0x1B, b'O', b'Q'],
        F(3) => vec![0x1B, b'O', b'R'],
        F(4) => vec![0x1B, b'O', b'S'],
        F(5) => vec![0x1B, b'[', b'1', b'5', b'~'],
        F(6) => vec![0x1B, b'[', b'1', b'7', b'~'],
        F(7) => vec![0x1B, b'[', b'1', b'8', b'~'],
        F(8) => vec![0x1B, b'[', b'1', b'9', b'~'],
        F(9) => vec![0x1B, b'[', b'2', b'0', b'~'],
        F(10) => vec![0x1B, b'[', b'2', b'1', b'~'],
        F(11) => vec![0x1B, b'[', b'2', b'3', b'~'],
        F(12) => vec![0x1B, b'[', b'2', b'4', b'~'],
        _ => vec![],
    }
}
