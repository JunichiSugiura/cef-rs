//! Vimium key handling, find-mode queue, and link-hint UX (winit → CEF osr_host).
//! Lives under the top-level [`crate::vimium`] module; consumed by [`crate::window::dispatch`]
//! and [`crate::vimium::VimiumPlugin`] systems.

use std::time::{Duration, Instant};

use bevy_ecs::entity::Entity;
use winit::event::ElementState;
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::WindowId;

use crate::vimium;
use crate::vimium::{VimiumChromeCleanup, VimiumState};
use crate::settings::chord_matches_winit;

use crate::browser::events::{
    ArmWindowlessCloseBrowserEvent, LinkHintsFeedKeyDeferredBrowserEvent, LinkHintsHideBrowserEvent,
    LinkHintsShowBrowserEvent, NavigateBrowserEvent, QuitCloseAllBrowsersBrowserEvent,
    ReloadBrowserEvent,
};
use crate::browser::editable_focus as editable_focus;
use crate::browser::event_loop::RuntimeState;
use crate::browser::shell_ops::{enqueue_browser_ui_op, BrowserUiOp};
use crate::browser::renderer::input::keyboard;
use crate::browser::event_loop::{LinkHintFeedEvent, VimiumKeyReplayEvent};
use crate::vimium::editable_gating::{self as editable_gating, EditableFocusSnapshot};
use crate::browser::renderer::osr_host::state::OsrHostState;

#[derive(Default)]
pub(crate) struct BrowserEventBatch {
    pub navigate: Vec<NavigateBrowserEvent>,
    pub reload: Vec<ReloadBrowserEvent>,
    pub link_hints_show: Vec<LinkHintsShowBrowserEvent>,
    pub link_hints_hide: Vec<LinkHintsHideBrowserEvent>,
    pub link_hints_feed_key_deferred: Vec<LinkHintsFeedKeyDeferredBrowserEvent>,
    pub arm_windowless_close: Vec<ArmWindowlessCloseBrowserEvent>,
    pub quit_close_all_browsers: Vec<QuitCloseAllBrowsersBrowserEvent>,
}

pub(crate) fn queue_find_mode_key(
    _osr_host: &mut OsrHostState,
    rt: &mut RuntimeState,
    window_id: WindowId,
    event: winit::event::KeyEvent,
) {
    rt.pending_find_mode_keys.push_back((window_id, event));
}

pub(crate) fn drain_find_mode_keys(osr_host: &mut OsrHostState, rt: &mut RuntimeState, vim: &mut VimiumState) {
    while let Some((window_id, event)) = rt.pending_find_mode_keys.pop_front() {
        let _ = handle_find_mode_pressed_inner(osr_host, rt, vim, window_id, &event);
    }
}

pub(crate) fn vimium_state_snapshot(_osr_host: &OsrHostState, vim: &VimiumState) -> crate::vimium::VimiumStateSnapshot {
    vim.snapshot()
}

pub(crate) fn defer_vimium_key_after_editable_probe(
        osr_host: &mut OsrHostState,
    window_id: WindowId,
    browser_id: i32,
    event: &winit::event::KeyEvent,
) -> bool {
    editable_focus::enqueue_vimium_replay_request(
        &osr_host.editable_focus_queues.pending_vimium_replays,
        browser_id,
        window_id,
        event.clone(),
    );
    true
}

pub(crate) fn apply_link_hint_feed_command(
    osr_host: &mut OsrHostState,
    vim: &mut VimiumState,
    window_id: WindowId,
    browser_id: i32,
    ch: char,
    outcome_still_active: bool,
    _hint_label_width: u8,
    out: &mut BrowserEventBatch,
) {
    if vim.link_hints_browser_id() != Some(browser_id) {
        return;
    }
    if outcome_still_active {
        vim.link_hints_push_typed_char(ch);
    } else {
        out.link_hints_hide.push(LinkHintsHideBrowserEvent { browser_id });
        vim.clear_link_hints();
        enqueue_browser_ui_op(
            &osr_host.browser_ui_ops,
            BrowserUiOp::InvalidateEditableFocusHint { browser_id },
        );
        editable_focus::enqueue_probe_request(
            &osr_host.editable_focus_queues.pending_probes,
            browser_id,
        );
    }
    osr_host.nudge_osr_view_after_input(window_id);
}
fn discard_vimium_link_hints_for_browser_if_any(
    osr_host: &mut OsrHostState,
    vim: &mut VimiumState,
    bid: i32,
    out: &mut BrowserEventBatch,
) {
    vim.clear_link_hints_if_browser(bid);
    let Some(wid) = crate::browser::backend::osr::foreign_index::window_id_for_browser(
        crate::browser::event_loop::foreign_osr_index().as_ref(),
        bid,
    ) else {
        return;
    };
    // Always hide on navigation invalidation: Rust may already be `Browse` while the DOM
    // overlay remains (e.g. missed sync), which breaks a follow-up `f`.
    out.link_hints_hide.push(LinkHintsHideBrowserEvent { browser_id: bid });
    osr_host.nudge_osr_view_after_input(wid);
}

pub(crate) fn apply_link_hints_navigation_resets(
    osr_host: &mut OsrHostState,
    vim: &mut VimiumState,
    out: &mut BrowserEventBatch,
    browser_ids: Vec<i32>,
) {
    for bid in browser_ids {
        discard_vimium_link_hints_for_browser_if_any(osr_host, vim, bid, out);
        discard_vimium_ux_for_browser_if_any(osr_host, vim, bid);
    }
}

fn cleanup_vimium_ux_ui_at_window(osr_host: &OsrHostState, vim: &VimiumState, window_id: WindowId) {
    let Some(b) = osr_host.browser_for_window(window_id) else {
        return;
    };
    match vim.chrome_cleanup() {
        Some(VimiumChromeCleanup::Find) => {
            vimium::modes::find_ui_hide(&b);
            vimium::modes::cef_stop_finding(&b, true);
        }
        Some(VimiumChromeCleanup::Visual) => vimium::modes::visual_hint_hide(&b),
        None => {}
    }
}

fn clear_vimium_ux_if_other_browser(osr_host: &mut OsrHostState, vim: &mut VimiumState, bid: i32) {
    let Some(old_bid) = vim.ux_browser_id() else {
        return;
    };
    if old_bid == bid {
        return;
    }
    discard_vimium_ux_for_browser_if_any(osr_host, vim, old_bid);
}

/// End insert / find / visual for this browser without clearing `find_committed` (same-tab mode switch).
pub(crate) fn teardown_vimium_ux_at_window_keep_committed(
    osr_host: &mut OsrHostState,
    vim: &mut VimiumState,
    window_id: WindowId,
    bid: i32,
) {
    if vim.ux_browser_id() != Some(bid) {
        return;
    }
    cleanup_vimium_ux_ui_at_window(osr_host, vim, window_id);
    vim.exit_ux_to_browse();
}

fn discard_vimium_ux_for_browser_if_any(osr_host: &mut OsrHostState, vim: &mut VimiumState, bid: i32) {
    if vim.ux_browser_id() != Some(bid) {
        return;
    }
    let Some(wid) = crate::browser::backend::osr::foreign_index::window_id_for_browser(
        crate::browser::event_loop::foreign_osr_index().as_ref(),
        bid,
    ) else {
        vim.exit_ux_to_browse();
        vim.find_committed.clear();
        return;
    };
    cleanup_vimium_ux_ui_at_window(osr_host, vim, wid);
    vim.exit_ux_to_browse();
    vim.find_committed.clear();
    osr_host.nudge_osr_view_after_input(wid);
}

pub(crate) fn handle_find_mode_pressed(
        osr_host: &mut OsrHostState,
    rt: &mut RuntimeState,
    window_id: WindowId,
    event: &winit::event::KeyEvent,
) -> bool {
    queue_find_mode_key(osr_host, rt, window_id, event.clone());
    true
}

pub(crate) fn handle_find_mode_pressed_inner(
        osr_host: &mut OsrHostState,
    rt: &mut RuntimeState,
    vim: &mut VimiumState,
    window_id: WindowId,
    event: &winit::event::KeyEvent,
) -> bool {
    let ctrl = rt.mods_winit.control_key();
    let cmd = rt.mods_winit.super_key();
    if ctrl || cmd {
        return false;
    }
    let Some(browser) = osr_host.browser_for_window(window_id) else {
        return true;
    };
    if event.state == ElementState::Released {
        return true;
    }

    match &event.logical_key {
        Key::Named(NamedKey::Escape) => {
            vimium::modes::find_ui_hide(&browser);
            vimium::modes::cef_stop_finding(&browser, true);
            vim.cancel_find();
            osr_host.nudge_osr_view_after_input(window_id);
            return true;
        }
        Key::Named(NamedKey::Enter) => {
            vimium::modes::find_ui_hide(&browser);
            vim.finish_find_accept();
            if vim.find_committed.is_empty() {
                vimium::modes::cef_stop_finding(&browser, true);
            }
            osr_host.nudge_osr_view_after_input(window_id);
            return true;
        }
        Key::Named(NamedKey::Backspace) => {
            let Some(query): Option<&mut String> = vim.find_query_mut() else {
                return true;
            };
            query.pop();
            vimium::modes::find_ui_set_query(&browser, query);
            vimium::modes::cef_find(&browser, query, true, false);
            osr_host.nudge_osr_view_after_input(window_id);
            return true;
        }
        Key::Character(input) => {
            let Some(query): Option<&mut String> = vim.find_query_mut() else {
                return true;
            };
            if input.chars().any(|c| c.is_control()) {
                return true;
            }
            query.push_str(input);
            vimium::modes::find_ui_set_query(&browser, query);
            vimium::modes::cef_find(&browser, query, true, false);
            osr_host.nudge_osr_view_after_input(window_id);
            return true;
        }
        _ => {}
    }
    let Some(query): Option<&mut String> = vim.find_query_mut() else {
        return true;
    };
    // Fallback for platforms/keys where logical key did not provide a character.
    if let Some(t) = &event.text {
        for ch in t.chars() {
            if !ch.is_control() {
                query.push(ch);
            }
        }
        vimium::modes::find_ui_set_query(&browser, query);
        vimium::modes::cef_find(&browser, query, true, false);
        osr_host.nudge_osr_view_after_input(window_id);
        return true;
    }
    // Physical letters: macOS can deliver `LogicalKey::Unidentified` and empty `text` for a
    // printable key (IME / pipeline timing), so find-in-page would eat the stroke with no effect.
    if let Some(ch) = keyboard::lowercase_letter_from_physical(&event.physical_key) {
        query.push(ch);
        vimium::modes::find_ui_set_query(&browser, query);
        vimium::modes::cef_find(&browser, query, true, false);
        osr_host.nudge_osr_view_after_input(window_id);
        return true;
    }
    true
}
pub(crate) fn try_handle_vimium_keys(
    osr_host: &mut OsrHostState,
    rt: &mut RuntimeState,
    vim: &mut VimiumState,
    window_id: WindowId,
    event: &winit::event::KeyEvent,
    ecs_browser: Option<(Entity, i32)>,
    editable_focus: &EditableFocusSnapshot,
    out: &mut BrowserEventBatch,
) -> bool {
    try_handle_vimium_keys_inner(
        osr_host,
        rt,
        vim,
        window_id,
        event,
        false,
        ecs_browser,
        editable_focus,
        out,
    )
}

pub(crate) fn try_handle_vimium_keys_after_editable_probe(
    osr_host: &mut OsrHostState,
    rt: &mut RuntimeState,
    vim: &mut VimiumState,
    window_id: WindowId,
    event: &winit::event::KeyEvent,
    editable_focus: &EditableFocusSnapshot,
    out: &mut BrowserEventBatch,
) -> bool {
    try_handle_vimium_keys_inner(
        osr_host,
        rt,
        vim,
        window_id,
        event,
        true,
        None,
        editable_focus,
        out,
    )
}

/// `editable_hint_fresh`: editable-focus DOM probe has just run; skip scheduling another probe before branching.
pub(crate) fn try_handle_vimium_keys_inner(
    osr_host: &mut OsrHostState,
    rt: &mut RuntimeState,
    vim: &mut VimiumState,
    window_id: WindowId,
    event: &winit::event::KeyEvent,
    editable_hint_fresh: bool,
    ecs_browser: Option<(Entity, i32)>,
    editable_focus: &EditableFocusSnapshot,
    out: &mut BrowserEventBatch,
) -> bool {
    let km = &*osr_host.key_settings;
    let Some(bid) = osr_host.browser_id_for_window(window_id, ecs_browser) else {
        return false;
    };

    if !km.enabled {
        if vim.link_hints_browser_id() == Some(bid) {
                out.link_hints_hide.push(LinkHintsHideBrowserEvent { browser_id: bid });
            osr_host.nudge_osr_view_after_input(window_id);
            vim.clear_link_hints();
        }
        if vim.ux_browser_id() == Some(bid) {
                cleanup_vimium_ux_ui_at_window(osr_host, vim, window_id);
            vim.exit_ux_to_browse();
            vim.find_committed.clear();
        }
        return false;
    }

    if vim.find_swallows_keyup(bid) {
        if event.state == ElementState::Released {
            return true;
        }
    }

    if event.state != ElementState::Pressed {
        return false;
    }

    let chord_mods = rt.mods_winit;
    let physical = &event.physical_key;
    let mod_shift = chord_mods.shift_key();
    let mod_ctrl = chord_mods.control_key();
    let mod_alt = chord_mods.alt_key();
    let mod_cmd = chord_mods.super_key();
    let letter_press_no_winit_text = keyboard::physical_letter_press_without_winit_text(event);
    let key_sends_printable_text = keyboard::keyevent_has_printable_text(event);
    let now = Instant::now();

    if let Some(hid_bid) = vim.expire_link_hints_if_due(now) {
        out.link_hints_hide.push(LinkHintsHideBrowserEvent {
            browser_id: hid_bid,
        });
        osr_host.nudge_osr_view_after_input(window_id);
    }

    // `LinkHints`: when typing the hint letters themselves we must NOT dismiss based on
    // "editable focus" probes, otherwise hints can disappear without activating a target.
    if vim.link_hints_active() {
        let is_hint_letter = keyboard::lowercase_letter_from_physical(physical).is_some();
        let esc = match physical {
            PhysicalKey::Code(KeyCode::Escape) => true,
            _ => false,
        };
        let no_ctrl_alt_cmd = !mod_ctrl && !mod_alt && !mod_cmd;

        // Dismiss only for keys other than plain hint letters.
        if !(is_hint_letter && no_ctrl_alt_cmd) {
            if !editable_hint_fresh {
                return defer_vimium_key_after_editable_probe(osr_host, window_id, bid, event);
            }
            if !editable_gating::may_handle_history_shortcuts(editable_focus, bid) {
                out.link_hints_hide.push(LinkHintsHideBrowserEvent { browser_id: bid });
                vim.clear_link_hints();
                osr_host.nudge_osr_view_after_input(window_id);
            }
        }

        if !vim.link_hints_active() {
            // Dismissed above.
            return false;
        }

        if esc && no_ctrl_alt_cmd {
            out.link_hints_hide.push(LinkHintsHideBrowserEvent { browser_id: bid });
            vim.clear_link_hints();
            osr_host.nudge_osr_view_after_input(window_id);
            return true;
        }
        return false;
    }

    let window_ms = km.scroll_top_double_press_ms;
    if let Some(prev) = vim.scroll_g_pending {
        if now.duration_since(prev) > Duration::from_millis(window_ms) {
            vim.scroll_g_pending = None;
        }
    }

    if vim.is_insert(bid) {
        let esc = match physical {
            PhysicalKey::Code(KeyCode::Escape) => true,
            _ => false,
        };
        let no_mod = !mod_ctrl && !mod_cmd && !mod_alt && !mod_shift;
        let ctrl_ob = mod_ctrl && !mod_cmd && !mod_alt
            && match physical {
                PhysicalKey::Code(KeyCode::BracketLeft) => true,
                _ => false,
            };
        if (esc && no_mod) || ctrl_ob {
            vim.exit_ux_to_browse();
            return true;
        }
        return false;
    }

    if vim.is_find(bid) {
        return handle_find_mode_pressed(osr_host, rt, window_id, event);
    }

    if vim.is_visual(bid) {
        let esc = match physical {
            PhysicalKey::Code(KeyCode::Escape) => true,
            _ => false,
        };
        let no_mod = !mod_ctrl && !mod_cmd && !mod_alt && !mod_shift;
        if esc && no_mod {
            if let Some(b) = osr_host.browser_for_window(window_id) {
                vimium::modes::visual_hint_hide(&b);
            }
            vim.exit_ux_to_browse();
            osr_host.nudge_osr_view_after_input(window_id);
            return true;
        }
        let y_plain = !mod_shift && !mod_ctrl && !mod_cmd && !mod_alt;
        if y_plain {
            match physical {
                PhysicalKey::Code(KeyCode::KeyY) => {
                    if let Some(b) = osr_host.browser_for_window(window_id) {
                        vimium::modes::yank_selection(&b);
                        osr_host.nudge_osr_view_after_input(window_id);
                    }
                    return true;
                }
                _ => {}
            }
        }
        return false;
    }

    // Link hints (`f`): before the printable-text bail (winit sets `text` on letter keys).
    //
    // - `may_handle_history_shortcuts`: block when probe/IME says **sure** text focus.
    // - First keydown uses `!editable_hint_fresh` → defer + DOM probe, then replay once.
    // - After replay, require `vimium_keys_safe_for_page` (probe **sure** not in an editable)
    //   before arming — avoids (a) an infinite defer loop when the hint never becomes
    //   `Some(false)`, and (b) arming hints while Google's search box has focus but the probe
    //   still said "page" for a moment. If we're not sure, pass `f` to the page.
    if let Some(ref chord) = km.hint_links {
        if chord_matches_winit(chord, chord_mods, physical) {
            if !editable_gating::may_handle_history_shortcuts(editable_focus, bid) {
                return false;
            }
            if !editable_hint_fresh {
                return defer_vimium_key_after_editable_probe(osr_host, window_id, bid, event);
            }
            if !editable_gating::page_allows_app_shortcuts(editable_focus, bid) {
                return false;
            }
            out.link_hints_show.push(LinkHintsShowBrowserEvent { browser_id: bid });
            vim.arm_link_hints(bid, now);
            osr_host.nudge_osr_view_after_input(window_id);
            return true;
        }
    }

    // Browse: when the hint says focus is in a text control, pass unmodified keys to CEF.
    // Do **not** use `KeyEvent::text` here — winit sets it for almost every letter (`j`/`k`/…),
    // which would block all vimium scrolling. Printable-text guarding is applied only where needed
    // (e.g. find-next/prev) and inside `page_ok` via `letter_press_no_winit_text` for IME.
    {
        let plain = !mod_ctrl && !mod_cmd && !mod_alt;
        if plain && editable_gating::editable_focus_is_typing(editable_focus, bid) {
            return false;
        }
    }

    if let Some(ref chord) = km.history_back {
        if chord_matches_winit(chord, chord_mods, physical) {
            if !editable_hint_fresh {
                return defer_vimium_key_after_editable_probe(osr_host, window_id, bid, event);
            }
            if editable_gating::page_allows_app_shortcuts(editable_focus, bid) {
                vim.scroll_g_pending = None;
                osr_host.request_set_active_browser(bid);
                out.navigate.push(NavigateBrowserEvent {
                        browser_id: bid,
                        go_forward: false,
                    });
                return true;
            }
            return false;
        }
    }
    if let Some(ref chord) = km.history_forward {
        if chord_matches_winit(chord, chord_mods, physical) {
            if !editable_hint_fresh {
                return defer_vimium_key_after_editable_probe(osr_host, window_id, bid, event);
            }
            if editable_gating::page_allows_app_shortcuts(editable_focus, bid) {
                vim.scroll_g_pending = None;
                osr_host.request_set_active_browser(bid);
                out.navigate.push(NavigateBrowserEvent {
                        browser_id: bid,
                        go_forward: true,
                    });
                return true;
            }
            return false;
        }
    }

    if let Some(browser) = osr_host.browser_for_window(window_id) {
        if !vim.find_committed.is_empty() {
            if let Some(ref chord) = km.find_next {
                if chord_matches_winit(chord, chord_mods, physical) {
                    if editable_gating::editable_focus_is_typing(editable_focus, bid)
                        || key_sends_printable_text
                    {
                        return false;
                    }
                    vim.scroll_g_pending = None;
                    vimium::modes::cef_find(&browser, &vim.find_committed, true, true);
                    osr_host.nudge_osr_view_after_input(window_id);
                    return true;
                }
            }
            if let Some(ref chord) = km.find_prev {
                if chord_matches_winit(chord, chord_mods, physical) {
                    if editable_gating::editable_focus_is_typing(editable_focus, bid)
                        || key_sends_printable_text
                    {
                        return false;
                    }
                    vim.scroll_g_pending = None;
                    vimium::modes::cef_find(&browser, &vim.find_committed, false, true);
                    osr_host.nudge_osr_view_after_input(window_id);
                    return true;
                }
            }
        }
    }

    // Avoid enqueueing editable-focus probes on every key: each runs a DOM visit +
    // message-loop pump and can crash or corrupt CEF when re-entered while typing in an `<input>`.
    let might_mode_chord = [
        km.mode_insert.as_ref(),
        km.mode_find_open.as_ref(),
        km.mode_visual.as_ref(),
        km.yank_url.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|c| chord_matches_winit(c, chord_mods, physical));

    if might_mode_chord {
        if !editable_hint_fresh {
            return defer_vimium_key_after_editable_probe(osr_host, window_id, bid, event);
        }
        // When `text` is missing, the real character may arrive only via `Ime::Commit`; do not
        // trust a stale "not editable" probe for mode chords (same class of bug as `d`/`g`/`r`).
        let page_ok_modes =
            editable_gating::page_allows_app_shortcuts(editable_focus, bid) && !letter_press_no_winit_text;

        if let Some(browser) = osr_host.browser_for_window(window_id) {
            if let Some(ref chord) = km.mode_insert {
                if chord_matches_winit(chord, chord_mods, physical) && page_ok_modes {
                        clear_vimium_ux_if_other_browser(osr_host, vim, bid);
                        teardown_vimium_ux_at_window_keep_committed(osr_host, vim, window_id, bid);
                        vim.enter_insert(bid);
                    osr_host.nudge_osr_view_after_input(window_id);
                    return true;
                }
            }
            if let Some(ref chord) = km.mode_find_open {
                if chord_matches_winit(chord, chord_mods, physical) && page_ok_modes {
                        clear_vimium_ux_if_other_browser(osr_host, vim, bid);
                        teardown_vimium_ux_at_window_keep_committed(osr_host, vim, window_id, bid);
                        vim.enter_find(bid);
                    vimium::modes::find_ui_show(&browser);
                    vimium::modes::find_ui_set_query(&browser, "");
                    osr_host.nudge_osr_view_after_input(window_id);
                    return true;
                }
            }
            if let Some(ref chord) = km.mode_visual {
                if chord_matches_winit(chord, chord_mods, physical)
                    && page_ok_modes
                    && !rt.primary_mouse_down
                    && !vim.link_hints_active()
                {
                        clear_vimium_ux_if_other_browser(osr_host, vim, bid);
                        teardown_vimium_ux_at_window_keep_committed(osr_host, vim, window_id, bid);
                        vim.enter_visual(bid);
                    vimium::modes::visual_hint_show(&browser);
                    osr_host.nudge_osr_view_after_input(window_id);
                    return true;
                }
            }
            if let Some(ref chord) = km.yank_url {
                if chord_matches_winit(chord, chord_mods, physical) && page_ok_modes {
                    vim.scroll_g_pending = None;
                    vimium::modes::yank_page_url(&browser);
                    osr_host.nudge_osr_view_after_input(window_id);
                    return true;
                }
            }
        }
    }

    let matches_vimium_content = [
        km.scroll_line_down.as_ref(),
        km.scroll_line_up.as_ref(),
        km.scroll_page_down.as_ref(),
        km.scroll_page_up.as_ref(),
        km.scroll_bottom.as_ref(),
        km.reload.as_ref(),
        km.scroll_top_prefix.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|c| chord_matches_winit(c, chord_mods, physical));

    let page_ok = if matches_vimium_content {
        if !editable_hint_fresh {
            return defer_vimium_key_after_editable_probe(osr_host, window_id, bid, event);
        }
        editable_gating::page_allows_app_shortcuts(editable_focus, bid) && !letter_press_no_winit_text
    } else {
        false
    };
    if let Some(browser) = osr_host.browser_for_window(window_id) {
        if let Some(ref chord) = km.scroll_line_down {
            if chord_matches_winit(chord, chord_mods, physical) {
                if page_ok {
                    vim.scroll_g_pending = None;
                    vimium::scroll::scroll_line_down(&browser);
                    osr_host.nudge_osr_view_after_input(window_id);
                }
                return page_ok;
            }
        }
        if let Some(ref chord) = km.scroll_line_up {
            if chord_matches_winit(chord, chord_mods, physical) {
                if page_ok {
                    vim.scroll_g_pending = None;
                    vimium::scroll::scroll_line_up(&browser);
                    osr_host.nudge_osr_view_after_input(window_id);
                }
                return page_ok;
            }
        }
        if let Some(ref chord) = km.scroll_page_down {
            if chord_matches_winit(chord, chord_mods, physical) {
                if page_ok {
                    vim.scroll_g_pending = None;
                    vimium::scroll::scroll_page_down(&browser);
                    osr_host.nudge_osr_view_after_input(window_id);
                }
                return page_ok;
            }
        }
        if let Some(ref chord) = km.scroll_page_up {
            if chord_matches_winit(chord, chord_mods, physical) {
                if page_ok {
                    vim.scroll_g_pending = None;
                    vimium::scroll::scroll_page_up(&browser);
                    osr_host.nudge_osr_view_after_input(window_id);
                }
                return page_ok;
            }
        }
        if let Some(ref chord) = km.scroll_bottom {
            if chord_matches_winit(chord, chord_mods, physical) {
                if page_ok {
                    vim.scroll_g_pending = None;
                    vimium::scroll::scroll_bottom(&browser);
                    osr_host.nudge_osr_view_after_input(window_id);
                }
                return page_ok;
            }
        }
        if let Some(ref chord) = km.reload {
            if chord_matches_winit(chord, chord_mods, physical) {
                if page_ok {
                    vim.scroll_g_pending = None;
                    osr_host.request_set_active_browser(bid);
                    out.reload.push(ReloadBrowserEvent { browser_id: bid });
                }
                return page_ok;
            }
        }

        if let Some(ref prefix) = km.scroll_top_prefix {
            if chord_matches_winit(prefix, chord_mods, physical) {
                if !page_ok {
                    return false;
                }
                if let Some(prev) = vim.scroll_g_pending {
                    if now.duration_since(prev) <= Duration::from_millis(window_ms) {
                        vim.scroll_g_pending = None;
                        vimium::scroll::scroll_top(&browser);
                        osr_host.nudge_osr_view_after_input(window_id);
                        return true;
                    }
                }
                vim.scroll_g_pending = Some(now);
                return true;
            }
        }
    }

    let prefix_matches = km
        .scroll_top_prefix
        .as_ref()
        .is_some_and(|p| chord_matches_winit(p, chord_mods, physical));
    if !prefix_matches {
        vim.scroll_g_pending = None;
    }

    false
}

pub(crate) fn handle_vimium_key_replay_event(
    osr_host: &mut OsrHostState,
    rt: &mut RuntimeState,
    vim: &mut VimiumState,
    event: VimiumKeyReplayEvent,
    editable_focus: &EditableFocusSnapshot,
    out: &mut BrowserEventBatch,
) {
    let _ = try_handle_vimium_keys_after_editable_probe(
        osr_host,
        rt,
        vim,
        event.window_id,
        &event.event,
        editable_focus,
        out,
    );
}

pub(crate) fn handle_link_hint_feed_event(
    osr_host: &mut OsrHostState,
    vim: &mut VimiumState,
    event: LinkHintFeedEvent,
    out: &mut BrowserEventBatch,
) {
    apply_link_hint_feed_command(
        osr_host,
        vim,
        event.window_id,
        event.browser_id,
        event.ch,
        event.still_active,
        event.hint_label_width,
        out,
    );
}