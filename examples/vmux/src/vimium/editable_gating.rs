//! Editable-focus snapshot and gating for app-level keyboard UX (browse vs insert).
//! Built from ECS [`crate::browser::view_state::EditableFocusHint`] in
//! [`crate::window::pending_window_events::apply_osr_host_window_dispatches_system`] and vimium replay paths.
//!
//! **Link hints (`f`), scroll/reload, and mode chords (`/`, `i`, …)** intentionally do **not** use
//! [`page_allows_app_shortcuts`]: Google-style pages autofocus search and the probe reports typing,
//! which would block those keys on first paint (see `window_input`).
//!
//! **Shift+H/L** still use [`may_handle_history_shortcuts`] so back/forward yield to a focused field
//! unless you use the OS shortcuts (e.g. Cmd+[) from `window::dispatch`.

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn page_allows_app_shortcuts_only_after_probe_says_not_editable() {
        let mut hints = HashMap::new();
        hints.insert(7, None);
        assert!(!page_allows_app_shortcuts(&hints, 7));
        hints.insert(7, Some(true));
        assert!(!page_allows_app_shortcuts(&hints, 7));
        hints.insert(7, Some(false));
        assert!(page_allows_app_shortcuts(&hints, 7));
    }

    #[test]
    fn may_handle_history_shortcuts_blocks_only_confirmed_editable() {
        let mut hints = HashMap::new();
        assert!(may_handle_history_shortcuts(&hints, 1));
        hints.insert(1, None);
        assert!(may_handle_history_shortcuts(&hints, 1));
        hints.insert(1, Some(false));
        assert!(may_handle_history_shortcuts(&hints, 1));
        hints.insert(1, Some(true));
        assert!(!may_handle_history_shortcuts(&hints, 1));
    }
}
