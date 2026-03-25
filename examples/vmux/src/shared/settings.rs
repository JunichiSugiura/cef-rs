//! Load optional `settings.toml` for Vim-style key bindings (see `resources/settings.example.toml`).

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use winit::keyboard::KeyCode;

/// Raw file format (all keys optional except via defaults).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct VmuxSettingsFile {
    pub vim: VimSettingsFile,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct VimSettingsFile {
    pub enabled: bool,
    /// Second `g` within this window (ms) counts as `gg` → scroll top.
    pub scroll_top_double_press_ms: u64,
    pub scroll_line_down: String,
    pub scroll_line_up: String,
    pub scroll_page_down: String,
    pub scroll_page_up: String,
    /// First key of `gg` (usually `g`). Empty disables double-g scroll top.
    pub scroll_top_prefix: String,
    pub scroll_bottom: String,
    pub history_back: String,
    pub history_forward: String,
    pub reload: String,
}

impl Default for VimSettingsFile {
    fn default() -> Self {
        Self {
            enabled: true,
            scroll_top_double_press_ms: 500,
            scroll_line_down: "j".to_string(),
            scroll_line_up: "k".to_string(),
            scroll_page_down: "d".to_string(),
            scroll_page_up: "u".to_string(),
            scroll_top_prefix: "g".to_string(),
            scroll_bottom: "shift+g".to_string(),
            history_back: "shift+h".to_string(),
            history_forward: "shift+l".to_string(),
            reload: "r".to_string(),
        }
    }
}

impl Default for VmuxSettingsFile {
    fn default() -> Self {
        Self {
            vim: VimSettingsFile::default(),
        }
    }
}

/// Parsed modifier + physical key (US layout–agnostic via `PhysicalKey` in winit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyChord {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub cmd: bool,
    pub key: KeyCode,
}

#[derive(Debug, Clone)]
pub struct ResolvedVimSettings {
    pub enabled: bool,
    pub scroll_top_double_press_ms: u64,
    pub scroll_line_down: Option<KeyChord>,
    pub scroll_line_up: Option<KeyChord>,
    pub scroll_page_down: Option<KeyChord>,
    pub scroll_page_up: Option<KeyChord>,
    /// Key that arms `gg` (no modifiers).
    pub scroll_top_prefix: Option<KeyChord>,
    pub scroll_bottom: Option<KeyChord>,
    pub history_back: Option<KeyChord>,
    pub history_forward: Option<KeyChord>,
    pub reload: Option<KeyChord>,
}

impl ResolvedVimSettings {
    pub fn from_file(file: &VmuxSettingsFile) -> Self {
        let v = &file.vim;
        Self {
            enabled: v.enabled,
            scroll_top_double_press_ms: v.scroll_top_double_press_ms.max(50),
            scroll_line_down: parse_chord_opt(&v.scroll_line_down),
            scroll_line_up: parse_chord_opt(&v.scroll_line_up),
            scroll_page_down: parse_chord_opt(&v.scroll_page_down),
            scroll_page_up: parse_chord_opt(&v.scroll_page_up),
            scroll_top_prefix: parse_chord_opt(&v.scroll_top_prefix),
            scroll_bottom: parse_chord_opt(&v.scroll_bottom),
            history_back: parse_chord_opt(&v.history_back),
            history_forward: parse_chord_opt(&v.history_forward),
            reload: parse_chord_opt(&v.reload),
        }
    }
}

fn parse_chord_opt(s: &str) -> Option<KeyChord> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return None;
    }
    match parse_key_chord(s) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("vmux settings: bad key chord {s:?}: {e}");
            None
        }
    }
}

fn parse_key_chord(s: &str) -> Result<KeyChord, String> {
    let mut shift = false;
    let mut ctrl = false;
    let mut alt = false;
    let mut cmd = false;
    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return Err("empty chord".into());
    }
    let key_name = *parts.last().ok_or("no key")?;
    for p in &parts[..parts.len() - 1] {
        match p.to_ascii_lowercase().as_str() {
            "shift" => shift = true,
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "cmd" | "command" | "meta" | "super" => cmd = true,
            other => return Err(format!("unknown modifier {other:?}")),
        }
    }
    let key = parse_key_code(key_name)?;
    Ok(KeyChord {
        shift,
        ctrl,
        alt,
        cmd,
        key,
    })
}

fn parse_key_code(name: &str) -> Result<KeyCode, String> {
    let n = name.trim().to_ascii_lowercase();
    let code = match n.as_str() {
        "a" => KeyCode::KeyA,
        "b" => KeyCode::KeyB,
        "c" => KeyCode::KeyC,
        "d" => KeyCode::KeyD,
        "e" => KeyCode::KeyE,
        "f" => KeyCode::KeyF,
        "g" => KeyCode::KeyG,
        "h" => KeyCode::KeyH,
        "i" => KeyCode::KeyI,
        "j" => KeyCode::KeyJ,
        "k" => KeyCode::KeyK,
        "l" => KeyCode::KeyL,
        "m" => KeyCode::KeyM,
        "n" => KeyCode::KeyN,
        "o" => KeyCode::KeyO,
        "p" => KeyCode::KeyP,
        "q" => KeyCode::KeyQ,
        "r" => KeyCode::KeyR,
        "s" => KeyCode::KeyS,
        "t" => KeyCode::KeyT,
        "u" => KeyCode::KeyU,
        "v" => KeyCode::KeyV,
        "w" => KeyCode::KeyW,
        "x" => KeyCode::KeyX,
        "y" => KeyCode::KeyY,
        "z" => KeyCode::KeyZ,
        "0" => KeyCode::Digit0,
        "1" => KeyCode::Digit1,
        "2" => KeyCode::Digit2,
        "3" => KeyCode::Digit3,
        "4" => KeyCode::Digit4,
        "5" => KeyCode::Digit5,
        "6" => KeyCode::Digit6,
        "7" => KeyCode::Digit7,
        "8" => KeyCode::Digit8,
        "9" => KeyCode::Digit9,
        "space" => KeyCode::Space,
        "escape" | "esc" => KeyCode::Escape,
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backspace" => KeyCode::Backspace,
        "bracketleft" | "bracket_left" | "[" => KeyCode::BracketLeft,
        "bracketright" | "bracket_right" | "]" => KeyCode::BracketRight,
        "arrowleft" | "left" => KeyCode::ArrowLeft,
        "arrowright" | "right" => KeyCode::ArrowRight,
        "arrowup" | "up" => KeyCode::ArrowUp,
        "arrowdown" | "down" => KeyCode::ArrowDown,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        _ => return Err(format!("unknown key {name:?}")),
    };
    Ok(code)
}

pub fn chord_matches(
    chord: &KeyChord,
    mods: cef::sys::cef_event_flags_t,
    physical: &winit::keyboard::PhysicalKey,
) -> bool {
    use cef::sys::cef_event_flags_t as F;
    let winit::keyboard::PhysicalKey::Code(code) = physical else {
        return false;
    };
    if *code != chord.key {
        return false;
    }
    let shift = (mods.0 & F::EVENTFLAG_SHIFT_DOWN.0) != 0;
    let ctrl = (mods.0 & F::EVENTFLAG_CONTROL_DOWN.0) != 0;
    let alt = (mods.0 & F::EVENTFLAG_ALT_DOWN.0) != 0;
    let cmd = (mods.0 & F::EVENTFLAG_COMMAND_DOWN.0) != 0;
    shift == chord.shift && ctrl == chord.ctrl && alt == chord.alt && cmd == chord.cmd
}

fn settings_search_paths() -> Vec<PathBuf> {
    let mut out = vec![
        PathBuf::from("settings.toml"),
        PathBuf::from("Settings.toml"),
    ];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            out.push(dir.join("settings.toml"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        out.push(PathBuf::from(home).join(".config/vmux/settings.toml"));
    }
    out
}

/// Load first existing `settings.toml` from search paths, else defaults.
pub fn load_settings() -> Arc<ResolvedVimSettings> {
    let file = load_settings_file();
    Arc::new(ResolvedVimSettings::from_file(&file))
}

fn load_settings_file() -> VmuxSettingsFile {
    for p in settings_search_paths() {
        if p.is_file() {
            match std::fs::read_to_string(&p) {
                Ok(text) => match toml::from_str::<VmuxSettingsFile>(&text) {
                    Ok(s) => {
                        crate::shared::launch_trace(&format!(
                            "settings: loaded {}",
                            p.display()
                        ));
                        return s;
                    }
                    Err(e) => eprintln!("vmux: failed to parse {}: {e}", p.display()),
                },
                Err(e) => eprintln!("vmux: failed to read {}: {e}", p.display()),
            }
        }
    }
    crate::shared::launch_trace("settings: no settings.toml found, using defaults");
    VmuxSettingsFile::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_shift_g() {
        let c = parse_key_chord("shift+g").unwrap();
        assert!(c.shift);
        assert_eq!(c.key, KeyCode::KeyG);
    }

    #[test]
    fn parse_j() {
        let c = parse_key_chord("j").unwrap();
        assert!(!c.shift);
        assert_eq!(c.key, KeyCode::KeyJ);
    }
}
