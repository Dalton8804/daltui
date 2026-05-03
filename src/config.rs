use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyBinding {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyBinding {
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    pub fn matches(&self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        self.code == code && self.modifiers == modifiers
    }

    pub fn display(&self) -> String {
        let key = match self.code {
            KeyCode::Char(c) => {
                if self.modifiers.contains(KeyModifiers::CONTROL) {
                    c.to_ascii_uppercase().to_string()
                } else {
                    c.to_string()
                }
            }
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Esc => "Esc".to_string(),
            KeyCode::Backspace => "BS".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::F(n) => format!("F{n}"),
            _ => "?".to_string(),
        };
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            format!("^{key}")
        } else if self.modifiers.contains(KeyModifiers::ALT) {
            format!("M-{key}")
        } else if self.modifiers.contains(KeyModifiers::SHIFT) {
            format!("S{key}")
        } else {
            key
        }
    }
}

impl<'de> Deserialize<'de> for KeyBinding {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_key_binding(&s).map_err(serde::de::Error::custom)
    }
}

fn parse_key_binding(s: &str) -> Result<KeyBinding, String> {
    let parts: Vec<&str> = s.split('+').collect();
    let (key_str, mod_strs) = parts.split_last().ok_or("empty binding")?;

    let mut modifiers = KeyModifiers::NONE;
    for m in mod_strs {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "alt" | "meta" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            other => return Err(format!("unknown modifier: {other}")),
        }
    }

    let code = match key_str.to_ascii_lowercase().as_str() {
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "backspace" | "bs" => KeyCode::Backspace,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "tab" => KeyCode::Tab,
        "delete" | "del" => KeyCode::Delete,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        s if s.len() == 1 => KeyCode::Char(s.chars().next().unwrap()),
        s if s.starts_with('f') => {
            let n: u8 = s[1..].parse().map_err(|_| format!("invalid key: {s}"))?;
            KeyCode::F(n)
        }
        other => return Err(format!("unknown key: {other}")),
    };

    Ok(KeyBinding::new(code, modifiers))
}

fn ctrl(c: char) -> KeyBinding {
    KeyBinding::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn key(c: char) -> KeyBinding {
    KeyBinding::new(KeyCode::Char(c), KeyModifiers::NONE)
}

fn enter() -> KeyBinding {
    KeyBinding::new(KeyCode::Enter, KeyModifiers::NONE)
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct GlobalKeys {
    pub quit: KeyBinding,
    pub cycle_pane: KeyBinding,
    pub next_window: KeyBinding,
    pub prev_window: KeyBinding,
    pub close_window: KeyBinding,
}

impl Default for GlobalKeys {
    fn default() -> Self {
        Self {
            quit: ctrl('q'),
            cycle_pane: ctrl('w'),
            next_window: ctrl('n'),
            prev_window: ctrl('p'),
            close_window: ctrl('x'),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct GitKeys {
    pub open: KeyBinding,
    pub new_worktree: KeyBinding,
    pub delete: KeyBinding,
    pub refresh: KeyBinding,
}

impl Default for GitKeys {
    fn default() -> Self {
        Self {
            open: enter(),
            new_worktree: ctrl('t'),
            delete: key('d'),
            refresh: ctrl('r'),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct DiffKeys {
    pub fullscreen: KeyBinding,
    pub explorer: KeyBinding,
    pub scroll_down: KeyBinding,
    pub scroll_up: KeyBinding,
}

impl Default for DiffKeys {
    fn default() -> Self {
        Self {
            fullscreen: ctrl('f'),
            explorer: key('e'),
            scroll_down: key('j'),
            scroll_up: key('k'),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct PtyKeys {
    pub cycle_pane: KeyBinding,
    pub quit: KeyBinding,
    pub scroll_up: KeyBinding,
    pub scroll_down: KeyBinding,
    pub page_up: KeyBinding,
    pub page_down: KeyBinding,
}

fn shift_up() -> KeyBinding {
    KeyBinding::new(KeyCode::Up, KeyModifiers::SHIFT)
}

fn shift_down() -> KeyBinding {
    KeyBinding::new(KeyCode::Down, KeyModifiers::SHIFT)
}

fn page_up() -> KeyBinding {
    KeyBinding::new(KeyCode::PageUp, KeyModifiers::NONE)
}

fn page_down() -> KeyBinding {
    KeyBinding::new(KeyCode::PageDown, KeyModifiers::NONE)
}

impl Default for PtyKeys {
    fn default() -> Self {
        Self {
            cycle_pane: ctrl('w'),
            quit: ctrl('q'),
            scroll_up: shift_up(),
            scroll_down: shift_down(),
            page_up: page_up(),
            page_down: page_down(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct KeyConfig {
    pub global: GlobalKeys,
    pub git: GitKeys,
    pub diff: DiffKeys,
    pub pty: PtyKeys,
}

impl KeyConfig {
    pub fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let path = std::path::Path::new(&home).join(".config/daltui/config.toml");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }
}
