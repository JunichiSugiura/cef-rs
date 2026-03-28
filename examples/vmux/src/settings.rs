//! Load optional `settings.toml` for Vimium-style key bindings (see `resources/settings.example.toml`).
//!
//! Override the first-window URL with **`VMUX_STARTUP_URL`** (non-empty) when `open` is not passing env
//! — run `…/vmux.app/Contents/MacOS/vmux` from a shell with that variable set.

/// Default max parent nodes to walk when probing whether a focused DOM context allows typing.
pub const DEFAULT_DOM_FOCUS_ANCESTOR_WALK_MAX: usize = 64;

/// Default first tab URL when `[browser] startup_url` is missing or empty.
///
/// Staged startup loads `about:blank` first, then navigates here for `http`/`https` (see
/// [`OsrHostState::staged_initial_navigation_url`](crate::browser::renderer::OsrHostState::staged_initial_navigation_url)).
///
/// Default matches pass criteria (`open` does not pass env). Staged startup still loads
/// `about:blank` first for `https` URLs (see `OsrHostState::staged_initial_navigation_url`).
pub const DEFAULT_STARTUP_URL: &str = "https://www.google.com";

use std::path::PathBuf;
use std::sync::Arc;

use bevy_app::{App, Plugin, Startup};
use bevy_ecs::prelude::Resource;
use bevy_ecs::schedule::{IntoSystemConfigs, SystemSet};
use serde::{Deserialize, Serialize};
use winit::keyboard::KeyCode;

/// Raw file format (all keys optional except via defaults).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SettingsFile {
    pub vimium: VimiumSettingsFile,
    pub browser: BrowserSettingsFile,
}

/// General browser / DOM tuning (not key bindings).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct BrowserSettingsFile {
    /// Max parent nodes to walk from the focused DOM node when deciding if typing is allowed.
    pub dom_focus_ancestor_walk_max: usize,
    /// Initial URL for the first window when the app starts. Empty uses [`DEFAULT_STARTUP_URL`].
    pub startup_url: String,
}

impl Default for BrowserSettingsFile {
    fn default() -> Self {
        Self {
            dom_focus_ancestor_walk_max: DEFAULT_DOM_FOCUS_ANCESTOR_WALK_MAX,
            startup_url: DEFAULT_STARTUP_URL.to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct VimiumSettingsFile {
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
    /// Link hints chord (default `f`). While hints are visible, the same key is fed to the page
    /// (hint letter), not a toggle — use Escape to cancel. Empty / `none` disables.
    pub hint_links: String,
    /// Pass keys to the page (Vimium insert). Empty / `none` disables.
    pub mode_insert: String,
    /// Open in-page find HUD (`/`). Empty / `none` disables.
    pub mode_find_open: String,
    /// Visual mode: `y` copies selection; Esc exits. Empty / `none` disables.
    pub mode_visual: String,
    pub find_next: String,
    pub find_prev: String,
    /// Copy page URL (clipboard). Empty / `none` disables.
    pub yank_url: String,
}

impl Default for VimiumSettingsFile {
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
            hint_links: "f".to_string(),
            mode_insert: "i".to_string(),
            mode_find_open: "/".to_string(),
            mode_visual: "v".to_string(),
            find_next: "n".to_string(),
            find_prev: "shift+n".to_string(),
            yank_url: "shift+y".to_string(),
        }
    }
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            vimium: VimiumSettingsFile::default(),
            browser: BrowserSettingsFile::default(),
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

impl KeyChord {
    #[inline]
    fn matches_physical_key(&self, physical: &winit::keyboard::PhysicalKey) -> bool {
        matches!(physical, winit::keyboard::PhysicalKey::Code(code) if *code == self.key)
    }

    #[inline]
    fn matches_modifiers(&self, mods: cef::sys::cef_event_flags_t) -> bool {
        use cef::sys::cef_event_flags_t as F;
        let mask = F::EVENTFLAG_SHIFT_DOWN.0
            | F::EVENTFLAG_CONTROL_DOWN.0
            | F::EVENTFLAG_ALT_DOWN.0
            | F::EVENTFLAG_COMMAND_DOWN.0;
        let actual = mods.0 & mask;
        let expected = (if self.shift { F::EVENTFLAG_SHIFT_DOWN.0 } else { 0 })
            | (if self.ctrl { F::EVENTFLAG_CONTROL_DOWN.0 } else { 0 })
            | (if self.alt { F::EVENTFLAG_ALT_DOWN.0 } else { 0 })
            | (if self.cmd { F::EVENTFLAG_COMMAND_DOWN.0 } else { 0 });
        actual == expected
    }
}

/// Parsed vmux key bindings plus startup URL and DOM focus tuning from [`SettingsFile`].
#[derive(Debug, Clone)]
pub struct KeySettings {
    pub enabled: bool,
    /// See [`BrowserSettingsFile::dom_focus_ancestor_walk_max`].
    pub dom_focus_ancestor_walk_max: usize,
    /// See [`BrowserSettingsFile::startup_url`].
    pub startup_url: String,
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
    pub hint_links: Option<KeyChord>,
    pub mode_insert: Option<KeyChord>,
    pub mode_find_open: Option<KeyChord>,
    pub mode_visual: Option<KeyChord>,
    pub find_next: Option<KeyChord>,
    pub find_prev: Option<KeyChord>,
    pub yank_url: Option<KeyChord>,
}

#[derive(Resource, Clone)]
pub struct SettingsResource(pub Arc<KeySettings>);

/// Bevy [`Startup`] set: `load_settings_resource_system` runs here. Other plugins should schedule
/// work that needs [`SettingsResource`] **after** this set (e.g. [`crate::browser::backend::cef::CefPlugin`]).
#[derive(SystemSet, Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct VmuxSettingsStartup;

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        app.configure_sets(Startup, VmuxSettingsStartup);
        app.add_systems(
            Startup,
            load_settings_resource_system.in_set(VmuxSettingsStartup),
        );
    }
}

fn load_settings_resource_system(mut commands: bevy_ecs::system::Commands) {
    let file = load_settings_file();
    commands.insert_resource(SettingsResource(Arc::new(key_settings_from_file(
        &file,
    ))));
}

fn key_settings_from_file(file: &SettingsFile) -> KeySettings {
    let v = &file.vimium;
    let startup_url = {
        let u = file.browser.startup_url.trim();
        let mut url = if u.is_empty() {
            DEFAULT_STARTUP_URL.to_string()
        } else {
            u.to_string()
        };
        if let Ok(env_url) = std::env::var("VMUX_STARTUP_URL") {
            let t = env_url.trim();
            if !t.is_empty() {
                url = t.to_string();
            }
        }
        url
    };
    KeySettings {
        enabled: v.enabled,
        dom_focus_ancestor_walk_max: file
            .browser
            .dom_focus_ancestor_walk_max
            .clamp(1, 512),
        startup_url,
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
        hint_links: parse_chord_opt(&v.hint_links),
        mode_insert: parse_chord_opt(&v.mode_insert),
        mode_find_open: parse_chord_opt(&v.mode_find_open),
        mode_visual: parse_chord_opt(&v.mode_visual),
        find_next: parse_chord_opt(&v.find_next),
        find_prev: parse_chord_opt(&v.find_prev),
        yank_url: parse_chord_opt(&v.yank_url),
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
        "slash" | "/" => KeyCode::Slash,
        _ => return Err(format!("unknown key {name:?}")),
    };
    Ok(code)
}

pub fn chord_matches(
    chord: &KeyChord,
    mods: cef::sys::cef_event_flags_t,
    physical: &winit::keyboard::PhysicalKey,
) -> bool {
    chord.matches_physical_key(physical) && chord.matches_modifiers(mods)
}

pub fn chord_matches_winit(
    chord: &KeyChord,
    mods: winit::keyboard::ModifiersState,
    physical: &winit::keyboard::PhysicalKey,
) -> bool {
    chord.matches_physical_key(physical)
        && chord.shift == mods.shift_key()
        && chord.ctrl == mods.control_key()
        && chord.alt == mods.alt_key()
        && chord.cmd == mods.super_key()
}

fn settings_search_paths() -> Vec<PathBuf> {
    let mut out = vec![
        PathBuf::from("settings.toml"),
        PathBuf::from("Settings.toml"),
    ];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos_dir) = exe.parent() {
            out.push(macos_dir.join("settings.toml"));
            // Bundled `.app`: `Contents/MacOS/vmux` → `Contents/Resources/settings.toml` (see `bundle-cef-app`).
            if let Some(contents) = macos_dir.parent() {
                out.push(contents.join("Resources").join("settings.toml"));
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        out.push(PathBuf::from(home).join(".config/vmux/settings.toml"));
    }
    out
}

/// Fallback when [`SettingsResource`] is missing (e.g. ordering); matches `SettingsFile::default()` parsing.
pub(crate) fn default_key_settings() -> Arc<KeySettings> {
    Arc::new(key_settings_from_file(&SettingsFile::default()))
}

fn load_settings_file() -> SettingsFile {
    for p in settings_search_paths() {
        if p.is_file() {
            match std::fs::read_to_string(&p) {
                Ok(text) => match toml::from_str::<SettingsFile>(&text) {
                    Ok(s) => {
                        bevy_log::info!(
                            target: "vmux",
                            pid = std::process::id(),
                            "settings: loaded {}",
                            p.display()
                        );
                        return s;
                    }
                    Err(e) => eprintln!("vmux: failed to parse {}: {e}", p.display()),
                },
                Err(e) => eprintln!("vmux: failed to read {}: {e}", p.display()),
            }
        }
    }
    bevy_log::info!(
        target: "vmux",
        pid = std::process::id(),
        "settings: no settings.toml found, using defaults"
    );
    SettingsFile::default()
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
