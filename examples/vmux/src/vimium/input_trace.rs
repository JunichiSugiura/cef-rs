//! Opt-in tracing for vimium vs winit input. Set **`VMUX_VIMIUM_INPUT_LOG`** to any non-empty
//! value, then reproduce the issue and read **`vmux-bevy.log`** (see [`crate::bundle_log`]).

use winit::event::{ElementState, Ime, KeyEvent};
use winit::window::WindowId;

#[inline]
pub(crate) fn enabled() -> bool {
    std::env::var_os("VMUX_VIMIUM_INPUT_LOG").is_some_and(|v| !v.is_empty())
}

pub(crate) fn log_startup_notice() {
    if enabled() {
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "vimium_input_trace: VMUX_VIMIUM_INPUT_LOG is set; logging KeyboardInput / Ime for vimium debugging"
        );
    }
}

pub(crate) fn keyboard_vimium(window_id: WindowId, handled: bool, event: &KeyEvent) {
    if !enabled() {
        return;
    }
    bevy_log::info!(
        target: "vmux",
        pid = std::process::id(),
        ?window_id,
        vimium_handled = handled,
        state = ?event.state,
        repeat = event.repeat,
        physical = ?event.physical_key,
        logical = ?event.logical_key,
        text = ?event.text,
        "vimium_input: KeyboardInput (after try_handle_vimium_keys)",
    );
}

pub(crate) fn ime_event(window_id: WindowId, browser_id: i32, ime: &Ime) {
    if !enabled() {
        return;
    }
    match ime {
        Ime::Commit(s) => {
            bevy_log::info!(
                target: "vmux",
                pid = std::process::id(),
                ?window_id,
                browser_id,
                commit_len = s.len(),
                commit = ?s,
                "vimium_input: Ime::Commit (before try_handle_vimium_ime_commit)",
            );
        }
        Ime::Preedit(s, caret) => {
            bevy_log::info!(
                target: "vmux",
                pid = std::process::id(),
                ?window_id,
                browser_id,
                preedit_len = s.len(),
                preedit = ?s,
                caret = ?caret,
                "vimium_input: Ime::Preedit",
            );
        }
        Ime::Disabled => {
            bevy_log::info!(
                target: "vmux",
                pid = std::process::id(),
                ?window_id,
                browser_id,
                "vimium_input: Ime::Disabled",
            );
        }
        Ime::Enabled => {
            bevy_log::info!(
                target: "vmux",
                pid = std::process::id(),
                ?window_id,
                browser_id,
                "vimium_input: Ime::Enabled",
            );
        }
    }
}

pub(crate) fn ime_commit_vimium_result(browser_id: i32, text: &str, handled: bool) {
    if !enabled() {
        return;
    }
    bevy_log::info!(
        target: "vmux",
        pid = std::process::id(),
        browser_id,
        vimium_handled = handled,
        text = ?text,
        "vimium_input: Ime::Commit (after try_handle_vimium_ime_commit)",
    );
}

pub(crate) fn keyboard_forward_cef_keydown(browser_id: i32, event: &KeyEvent, vk: i32) {
    if !enabled() || event.state != ElementState::Pressed {
        return;
    }
    bevy_log::info!(
        target: "vmux",
        pid = std::process::id(),
        browser_id,
        vk,
        physical = ?event.physical_key,
        "vimium_input: KeyboardInput → CEF RAWKEYDOWN/KEYDOWN",
    );
}

pub(crate) fn keyboard_forward_send_char(browser_id: i32, text: &str) {
    if !enabled() {
        return;
    }
    bevy_log::info!(
        target: "vmux",
        pid = std::process::id(),
        browser_id,
        text = ?text,
        "vimium_input: KeyboardInput → send_char to CEF",
    );
}

pub(crate) fn ime_forward_send_char(browser_id: i32, text: &str) {
    if !enabled() {
        return;
    }
    bevy_log::info!(
        target: "vmux",
        pid = std::process::id(),
        browser_id,
        text = ?text,
        "vimium_input: Ime::Commit → send_char to CEF (vimium did not handle commit)",
    );
}
