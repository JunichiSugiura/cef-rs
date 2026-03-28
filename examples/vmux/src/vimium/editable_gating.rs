//! Editable-focus snapshot and gating for app-level keyboard UX (browse vs insert).
//! Built from ECS [`crate::browser::view_state::EditableFocusHint`] in
//! [`crate::window::pending_window_events::apply_osr_host_window_dispatches_system`] and vimium replay paths.

use std::collections::HashMap;

/// Per-browser editable / IME hint from ECS, keyed by CEF `browser_id`.
pub type EditableFocusSnapshot = HashMap<i32, Option<bool>>;

pub(crate) fn may_handle_history_shortcuts(
    hints: &EditableFocusSnapshot,
    browser_id: i32,
) -> bool {
    match hints.get(&browser_id) {
        None | Some(None) | Some(Some(false)) => true,
        Some(Some(true)) => false,
    }
}

/// Page is in “browse” mode for app shortcuts (not focused in a text control).
pub(crate) fn page_allows_app_shortcuts(hints: &EditableFocusSnapshot, browser_id: i32) -> bool {
    matches!(hints.get(&browser_id), Some(Some(false)))
}

pub(crate) fn editable_focus_is_typing(hints: &EditableFocusSnapshot, browser_id: i32) -> bool {
    matches!(hints.get(&browser_id), Some(Some(true)))
}
