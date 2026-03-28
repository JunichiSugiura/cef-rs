//! Winit [`WindowEvent`] dispatch into CEF OSR — implementation lives here.

use std::sync::atomic::Ordering;

use bevy_ecs::entity::Entity;
use cef::sys::cef_event_flags_t;
use cef::*;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::window::WindowId;

use crate::vimium::VimiumState;

use crate::browser::backend::cef::bootstrap;
use crate::browser::editable_focus as editable_focus;
use crate::browser::event_loop::RuntimeState;
use crate::browser::shell_ops::{enqueue_browser_ui_op, BrowserUiOp};
use crate::browser::renderer::input::{keyboard, mouse};
use crate::browser::event_loop::LinkHintsNavPending;

use crate::vimium::editable_gating::EditableFocusSnapshot;
use crate::browser::renderer::osr_host::state::OsrHostState;
use crate::vimium::window_input;
use crate::vimium::window_input::BrowserEventBatch;

fn update_mods_from_winit(_osr_host: &mut OsrHostState, rt: &mut RuntimeState, m: ModifiersState) {
    rt.mods_winit = m;
    rt.mods = keyboard::update_mods_from_winit(m);
}

pub(crate) fn handle_window_event(
    osr_host: &mut OsrHostState,
    rt: &mut RuntimeState,
    vim: &mut VimiumState,
    window_id: WindowId,
    event: WindowEvent,
    ecs_browser: Option<(Entity, i32)>,
    editable_focus: &EditableFocusSnapshot,
    link_hints_nav_pending: &mut LinkHintsNavPending,
    out: &mut BrowserEventBatch,
) {
    osr_host.apply_pending_titles();
    let link_nav_ids = std::mem::take(&mut link_hints_nav_pending.0);
    window_input::apply_link_hints_navigation_resets(osr_host, vim, out, link_nav_ids);

    // Update modifier flags without holding the windows_store lock.
    if let WindowEvent::ModifiersChanged(m) = &event {
        update_mods_from_winit(osr_host, rt, m.state());
    }

    // Vimium-style bindings (`settings.toml` `[vimium]`, defaults like j/k/d/u, shift+h/l, gg, shift+g, r).
    // Find mode swallows key-up here so CEF does not get unmatched KEYUP.
    if let WindowEvent::KeyboardInput { event, .. } = &event {
        let vimium_handled = window_input::try_handle_vimium_keys(
            osr_host,
            rt,
            vim,
            window_id,
            event,
            ecs_browser,
            editable_focus,
            out,
        );
        crate::vimium::input_trace::keyboard_vimium(window_id, vimium_handled, event);
        if vimium_handled {
            return;
        }
    }

    // History: **Cmd+[** / **Cmd+]** (macOS) or **Ctrl+[** / **Ctrl+]** (Windows/Linux), same as
    // Chromium window shortcuts. Unlike Shift+H/L, we do **not** consult the editable-focus hint
    // so back/forward still run from search fields and other inputs.
    if let WindowEvent::KeyboardInput { event, .. } = &event {
        if event.state == ElementState::Pressed {
            let cmd = rt.mods_winit.super_key();
            let ctrl = rt.mods_winit.control_key();
            let primary = if cfg!(target_os = "macos") { cmd } else { ctrl };
            if primary {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let go_forward = match code {
                        KeyCode::BracketLeft => Some(false),
                        KeyCode::BracketRight => Some(true),
                        _ => None,
                    };
                    if let Some(go_forward) = go_forward {
                        if let Some(bid) = osr_host.browser_id_for_window(window_id, ecs_browser) {
                            osr_host.request_set_active_browser(bid);
                            out.navigate.push(crate::browser::events::NavigateBrowserEvent {
                                browser_id: bid,
                                go_forward,
                            });
                            return;
                        }
                    }
                }
            }
        }
    }

    // **Alt+Left** / **Alt+Right** — typical browser back/forward (esp. Windows); no editable probe.
    if let WindowEvent::KeyboardInput { event, .. } = &event {
        if event.state == ElementState::Pressed {
            let alt = rt.mods_winit.alt_key();
            let cmd = rt.mods_winit.super_key();
            let ctrl = rt.mods_winit.control_key();
            if alt && !cmd && !ctrl {
                if let PhysicalKey::Code(code) = event.physical_key {
                    let go_forward = match code {
                        KeyCode::ArrowLeft => Some(false),
                        KeyCode::ArrowRight => Some(true),
                        _ => None,
                    };
                    if let Some(go_forward) = go_forward {
                        if let Some(bid) = osr_host.browser_id_for_window(window_id, ecs_browser) {
                            osr_host.request_set_active_browser(bid);
                            out.navigate.push(crate::browser::events::NavigateBrowserEvent {
                                browser_id: bid,
                                go_forward,
                            });
                            return;
                        }
                    }
                }
            }
        }
    }

    // Global app shortcuts that shouldn't depend on the current browser/window entry.
    if let WindowEvent::KeyboardInput { event: key, .. } = &event {
        let cmd = rt.mods_winit.super_key();
        if cmd && key.state == ElementState::Pressed {
            match key.physical_key {
                PhysicalKey::Code(KeyCode::KeyQ) => {
                    // Quit: force-close all browsers. The normal shutdown path is driven by
                    // `on_before_close` setting the shutdown flag once the last browser closes.
                    crate::lifecycle_trace::record_runtime_event(
                        "window_dispatch Cmd+Q quit_close_all_browsers queued",
                    );
                    crate::browser::renderer::osr_host::quit_feedback::try_begin_quit_visual_feedback();
                    rt.quit_requested = true;
                    out.quit_close_all_browsers
                        .push(crate::browser::events::QuitCloseAllBrowsersBrowserEvent);
                    return;
                }
                _ => {}
            }
        }
    }

    let Ok(mut windows) = osr_host.cef_attach.windows_store.lock() else {
        return;
    };

    match event {
        WindowEvent::KeyboardInput { event, .. } => {
            let Some(entry) = windows.get(&window_id) else {
                return;
            };
            let browser = entry.browser.clone();
            let bid = browser.identifier();
            let ctrl = rt.mods_winit.control_key();
            let cmd = rt.mods_winit.super_key();
            let alt = rt.mods_winit.alt_key();
            let _shift = rt.mods_winit.shift_key();

            // Drop the store before CEF / follow-ups: `request_redraw_after_new_texture` may
            // touch the compositor and re-lock `windows_store`. Same pattern as `MouseWheel`.
            drop(windows);

            if let Some(h) = browser.host() {
                h.set_focus(1);
            }

            let hint_ch = keyboard::hint_label_char_from_key_event(&event);

            // While `LinkHints` is armed, do **not** run editable-focus dismiss from here.
            // Google (and similar) often keeps a search `<input>` in the tree; probing after
            // `do_message_loop_work` can flip to "editable" mid-hint-sequence, clear Rust hint
            // mode, and the **next** letter is delivered to CEF — so you "suddenly type in the
            // search box" after a few hint keys. Dismiss hints via Esc (`try_handle_vimium_keys`)
            // or when the feed reports hints ended / JS cleans up.
            // Hint letters are routed only while Rust `LinkHints` mode is active (armed by `f`).
            // Feeding runs on the CEF UI thread; completion is posted as [`super::winit_runner::LinkHintFeedEvent`].
            if vim.link_hints_active() {
                if let Some(ch) = hint_ch {
                    if !ctrl && !cmd && !alt {
                        match event.state {
                            ElementState::Pressed => {
                                // OS key-repeat would append the same letter twice (e.g. "bb"
                                // for hint "bd") → JS had no matches and called cleanup → Rust
                                // cleared LinkHints while badges remained; swallow repeats only.
                                if event.repeat {
                                    return;
                                }
                                // If focus moved into a real editable, don't swallow the
                                // keystroke for hint feeding; instead dismiss hints and
                                // let CEF handle typing.
                                // When hints are armed, every plain hint-letter is a command
                                // for the hint state machine; don't dismiss mid-sequence.
                                let prior = vim.link_hints_typed_prefix().len();
                                out.link_hints_feed_key_deferred.push(
                                    crate::browser::events::LinkHintsFeedKeyDeferredBrowserEvent {
                                        browser_id: bid,
                                        ch,
                                        prior_typed_len: prior,
                                    },
                                );
                                return;
                            }
                            ElementState::Released => {
                                // Keep swallowing releases for hint letters so the page
                                // doesn't receive partial hint keystrokes.
                                return;
                            }
                        }
                    }
                }
            }

            let Ok(windows) = osr_host.cef_attach.windows_store.lock() else {
                return;
            };
            let Some(entry) = windows.get(&window_id) else {
                return;
            };
            let Some(host) = entry.browser.host() else {
                return;
            };

            let hints_active = vim.link_hints_active();

            // Emacs-style Ctrl bindings for text fields (plus Cmd+A select-all).
            // We implement these at the OSR layer because web pages don't always get native
            // Cocoa text-system bindings when driven via synthetic key events.
            if !hints_active && event.state == ElementState::Pressed {
                if let PhysicalKey::Code(code) = event.physical_key {
                    // Cmd+A => Select All
                    if cmd && code == KeyCode::KeyA {
                        if let Some(frame) = entry
                            .browser
                            .focused_frame()
                            .or_else(|| entry.browser.main_frame())
                        {
                            frame.select_all();
                        }
                        return;
                    }

                    if ctrl {
                        // Ctrl+A => Home, Ctrl+E => End, Ctrl+B/F => left/right, Ctrl+P/N => up/down
                        let mapped: Option<(i32, i32, u16)> = match code {
                            KeyCode::KeyA => Some((0x24, 0x73, 0)), // Home
                            KeyCode::KeyE => Some((0x23, 0x77, 0)), // End
                            KeyCode::KeyB => Some((0x25, 0x7B, 0)), // Left
                            KeyCode::KeyF => Some((0x27, 0x7C, 0)), // Right
                            KeyCode::KeyP => Some((0x26, 0x7E, 0)), // Up
                            KeyCode::KeyN => Some((0x28, 0x7D, 0)), // Down
                            _ => None,
                        };
                        if let Some((vk, native, ch)) = mapped {
                            keyboard::send_key_press_and_release(
                                &host,
                                cef_event_flags_t::EVENTFLAG_NONE,
                                vk,
                                native,
                                ch,
                            );
                            return;
                        }
                    }
                }
            }

            if let Some((vk, native, key_char_u16)) =
                keyboard::key_codes_from_physical(&event.physical_key)
            {
                match event.state {
                    ElementState::Pressed => {
                        crate::vimium::input_trace::keyboard_forward_cef_keydown(bid, &event, vk);
                        // On macOS shortcuts (e.g. Cmd+A) often require KEYDOWN delivery,
                        // while some navigation expects RAWKEYDOWN. Send both.
                        for type_ in [KeyEventType::RAWKEYDOWN, KeyEventType::KEYDOWN] {
                            let kev = KeyEvent {
                                type_,
                                modifiers: rt.mods.0,
                                windows_key_code: vk,
                                native_key_code: native,
                                is_system_key: 0,
                                character: key_char_u16,
                                unmodified_character: key_char_u16,
                                focus_on_editable_field: 1,
                                ..Default::default()
                            };
                            host.send_key_event(Some(&kev));
                        }
                    }
                    ElementState::Released => {
                        let kev = KeyEvent {
                            type_: KeyEventType::KEYUP,
                            modifiers: rt.mods.0,
                            windows_key_code: vk,
                            native_key_code: native,
                            is_system_key: 0,
                            character: key_char_u16,
                            unmodified_character: key_char_u16,
                            focus_on_editable_field: 1,
                            ..Default::default()
                        };
                        host.send_key_event(Some(&kev));
                    }
                }
            }

            if event.state == ElementState::Pressed {
                // When Command/Ctrl is held down we want shortcuts like Cmd+A, Cmd+L, etc.
                // Do not generate CHAR events in that case.
                let has_shortcut_mod =
                    rt.mods_winit.super_key() || rt.mods_winit.control_key();
                if !hints_active && !has_shortcut_mod {
                    if let Some(text) = &event.text {
                        crate::vimium::input_trace::keyboard_forward_send_char(bid, text.as_str());
                        let mut any = false;
                        for ch in text.chars() {
                            keyboard::send_char(&host, rt.mods, ch);
                            any = true;
                        }
                        // Sites like Ledger use search UIs that our DOM probe often misses; once
                        // printable text is injected, treat focus as typing so `d`/`g`/`r` vimium
                        // bindings do not eat the rest of the word (e.g. "ledger" → "lee").
                        if any {
                            enqueue_browser_ui_op(
                                &osr_host.browser_ui_ops,
                                BrowserUiOp::SetEditableFocusHint {
                                    browser_id: bid,
                                    editable: true,
                                });
                        }
                    }
                }
            }

            if event.state == ElementState::Pressed && !hints_active {
                match event.physical_key {
                    PhysicalKey::Code(KeyCode::Tab) => {
                        let bid = entry.browser.identifier();
                        enqueue_browser_ui_op(
                            &osr_host.browser_ui_ops,
                            BrowserUiOp::InvalidateEditableFocusHint {
                                browser_id: bid,
                            });
                        editable_focus::enqueue_probe_request(&osr_host.editable_focus_queues.pending_probes, bid);
                    }
                    _ => {}
                }
            }
        }
        WindowEvent::Ime(ime) => {
            let Some(entry) = windows.get(&window_id) else {
                return;
            };
            let browser = entry.browser.clone();
            #[cfg(target_os = "macos")]
            let window = entry.surface.window.clone();
            drop(windows);

            let Some(host) = browser.host() else {
                return;
            };
            host.set_focus(1);
            let bid = browser.identifier();
            crate::vimium::input_trace::ime_event(window_id, bid, &ime);
            if let winit::event::Ime::Commit(text) = &ime {
                let vimium_handled = window_input::try_handle_vimium_ime_commit(
                    osr_host,
                    rt,
                    vim,
                    window_id,
                    bid,
                    text.as_str(),
                    editable_focus,
                    out,
                );
                crate::vimium::input_trace::ime_commit_vimium_result(
                    bid,
                    text.as_str(),
                    vimium_handled,
                );
                if vimium_handled {
                    #[cfg(target_os = "macos")]
                    {
                        if !window.has_focus() {
                            window.focus_window();
                        }
                        host.set_focus(1);
                        rt.macos_shell_refocus_window = Some(window_id);
                        rt.macos_shell_refocus_ticks = rt.macos_shell_refocus_ticks.max(8);
                    }
                    return;
                }
            }
            // `KeyboardInput` for hint letters is swallowed, but macOS still emits `Ime::Commit`
            // for the same physical key; forwarding it would inject into the page (e.g. second
            // letter of hint "by") and sites like Ledger show "press / for search" when focus
            // churns.
            match &ime {
                winit::event::Ime::Commit(_) => {
                    if vim.link_hints_active()
                        && vim.link_hints_browser_id() == Some(bid)
                    {
                        return;
                    }
                    enqueue_browser_ui_op(
                        &osr_host.browser_ui_ops,
                        BrowserUiOp::SetEditableFocusHint {
                            browser_id: bid,
                            editable: true,
                        });
                }
                winit::event::Ime::Disabled => {
                    enqueue_browser_ui_op(
                        &osr_host.browser_ui_ops,
                        BrowserUiOp::InvalidateEditableFocusHint {
                            browser_id: bid,
                        });
                    editable_focus::enqueue_probe_request(&osr_host.editable_focus_queues.pending_probes, bid);
                }
                _ => {
                    enqueue_browser_ui_op(
                        &osr_host.browser_ui_ops,
                        BrowserUiOp::SetEditableFocusHint {
                            browser_id: bid,
                            editable: true,
                        });
                }
            }
            if let winit::event::Ime::Commit(text) = ime {
                crate::vimium::input_trace::ime_forward_send_char(bid, text.as_str());
                for ch in text.chars() {
                    keyboard::send_char(&host, rt.mods, ch);
                }
            }
            // macOS can hand first responder to IME helpers in another activation context;
            // keep the winit osr_host key so typing stays in this window.
            #[cfg(target_os = "macos")]
            {
                if !window.has_focus() {
                    window.focus_window();
                }
                host.set_focus(1);
                rt.macos_shell_refocus_window = Some(window_id);
                rt.macos_shell_refocus_ticks = rt.macos_shell_refocus_ticks.max(8);
            }
        }
        WindowEvent::CursorMoved { position, .. } => {
            if let Some(entry) = windows.get(&window_id) {
                if let Some(host) = entry.browser.host() {
                    // CEF OSR expects DIP (logical) coordinates.
                    rt.last_cursor_pos =
                        mouse::cursor_moved_dip(position, entry.surface.window.scale_factor());
                    let ev = MouseEvent {
                        x: rt.last_cursor_pos.0,
                        y: rt.last_cursor_pos.1,
                        modifiers: rt.mods.0,
                    };
                    host.send_mouse_move_event(Some(&ev), 0);
                }
            }
        }
        WindowEvent::CursorLeft { .. } => {
            rt.primary_mouse_down = false;
            if let Some(entry) = windows.get(&window_id) {
                if let Some(host) = entry.browser.host() {
                    let ev = MouseEvent {
                        x: rt.last_cursor_pos.0,
                        y: rt.last_cursor_pos.1,
                        modifiers: rt.mods.0,
                    };
                    host.send_mouse_move_event(Some(&ev), 1);
                }
            }
        }
        WindowEvent::Focused(focused) => {
            if !focused {
                rt.primary_mouse_down = false;
            }
            #[cfg(target_os = "macos")]
            if !focused {
                // User left this window (or transient IME blur) — stop the pump from calling
                // `focus_window` in a loop, which would steal key window back from other apps.
                rt.macos_shell_refocus_ticks = 0;
                rt.macos_shell_refocus_window = None;
                // Winit/AppKit often emits a transient blur while the osr_host window is still the
                // right target (IME, key-window churn). Telling CEF `set_focus(0)` clears the
                // focused `<input>`, caret, and selection — avoid that for windowless OSR.
                // Re-assert browser focus only (do not `focus_window` here — that would steal
                // activation when the user intentionally switched to another app).
                if let Some(entry) = windows.get(&window_id) {
                    if let Some(host) = entry.browser.host() {
                        host.set_focus(1);
                    }
                }
                return;
            }
            if let Some(entry) = windows.get(&window_id) {
                if let Some(host) = entry.browser.host() {
                    host.set_focus(focused.into());
                    if focused {
                        osr_host.request_set_active_browser(entry.browser.identifier());
                    }
                }
            }
        }
        WindowEvent::ModifiersChanged(_m) => {}
        WindowEvent::MouseInput { state, button, .. } => {
            let Some(entry) = windows.get(&window_id) else {
                return;
            };
            match (state, button) {
                (ElementState::Pressed, MouseButton::Back) => {
                    let bid = entry.browser.identifier();
                    drop(windows);
                    osr_host.request_set_active_browser(bid);
                    out.navigate.push(crate::browser::events::NavigateBrowserEvent {
                        browser_id: bid,
                        go_forward: false,
                    });
                    return;
                }
                (ElementState::Pressed, MouseButton::Forward) => {
                    let bid = entry.browser.identifier();
                    drop(windows);
                    osr_host.request_set_active_browser(bid);
                    out.navigate.push(crate::browser::events::NavigateBrowserEvent {
                        browser_id: bid,
                        go_forward: true,
                    });
                    return;
                }
                _ => {}
            }
            let Some(entry) = windows.get(&window_id) else {
                return;
            };
            #[cfg(target_os = "macos")]
            let window = entry.surface.window.clone();
            let Some(host) = entry.browser.host() else {
                return;
            };
            match state {
                ElementState::Pressed => {
                    // Become key *before* CEF sees the click so nested focus logic sees our window.
                    #[cfg(target_os = "macos")]
                    if !window.has_focus() {
                        window.focus_window();
                    }
                    host.set_focus(1);
                    osr_host.request_set_active_browser(entry.browser.identifier());
                }
                ElementState::Released => {}
            }
            let cef_button = match button {
                MouseButton::Left => MouseButtonType::LEFT,
                MouseButton::Right => MouseButtonType::RIGHT,
                MouseButton::Middle => MouseButtonType::MIDDLE,
                _ => return,
            };
            match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => {
                    rt.primary_mouse_down = true;
                }
                (MouseButton::Left, ElementState::Released) => {
                    rt.primary_mouse_down = false;
                }
                _ => {}
            }
            let mouse_up = match state {
                ElementState::Released => 1,
                _ => 0,
            };
            let ev = MouseEvent {
                x: rt.last_cursor_pos.0,
                y: rt.last_cursor_pos.1,
                modifiers: rt.mods.0,
            };
            host.send_mouse_click_event(Some(&ev), cef_button, mouse_up, 1);
            // Re-hit-test hover/cursor after click so `on_cursor_change` runs (I-beam on inputs).
            if cef_button == MouseButtonType::LEFT && mouse_up == 1 {
                host.send_mouse_move_event(Some(&ev), 0);
                let bid = entry.browser.identifier();
                drop(windows);
                enqueue_browser_ui_op(
                    &osr_host.browser_ui_ops,
                    BrowserUiOp::InvalidateEditableFocusHint {
                        browser_id: bid,
                    },
                );
                // Async probe alone can finish after the first keystroke, so `d`/`g`/`r` vimium
                // bindings still run with a stale "not editable" hint — sync refresh before typing.
                editable_focus::enqueue_probe_request(
                    &osr_host.editable_focus_queues.pending_probes,
                    bid,
                );
                return;
            }
            // Windowless CEF often has no real NSView for the page; after focusing an `<input>`,
            // Chromium can resign our osr_host window’s key status and another app becomes active.
            // Only arm refocus on press — release would re-steal key after drag-release outside.
            #[cfg(target_os = "macos")]
            match state {
                ElementState::Pressed => {
                    if !window.has_focus() {
                        window.focus_window();
                    }
                    rt.macos_shell_refocus_window = Some(window_id);
                    rt.macos_shell_refocus_ticks = rt.macos_shell_refocus_ticks.max(10);
                }
                ElementState::Released => {}
            }
        }
        WindowEvent::MouseWheel { delta, .. } => {
            let Some(entry) = windows.get(&window_id) else {
                return;
            };
            let browser = entry.browser.clone();
            drop(windows);

            let mods = rt.mods;
            let (dx, dy, mods) = mouse::wheel_to_cef(delta, mods);

            let Some(host) = browser.host() else {
                return;
            };
            let Some((dx_i, dy_i)) =
                mouse::take_wheel_deltas_i32(&mut rt.wheel_residual, dx, dy)
            else {
                return;
            };
            let ev = MouseEvent {
                x: rt.last_cursor_pos.0,
                y: rt.last_cursor_pos.1,
                modifiers: mods.0,
            };
            host.send_mouse_wheel_event(Some(&ev), dx_i, dy_i);
        }
        WindowEvent::CloseRequested => {
            if windows.contains_key(&window_id) {
                drop(windows);
                out.arm_windowless_close
                    .push(crate::browser::events::ArmWindowlessCloseBrowserEvent);
                let Ok(windows) = osr_host.cef_attach.windows_store.lock() else {
                    return;
                };
                if let Some(entry) = windows.get(&window_id) {
                    if let Some(host) = entry.browser.host() {
                        host.try_close_browser();
                    }
                }
            }
        }
        WindowEvent::RedrawRequested => {
            #[cfg(all(
                any(target_os = "macos", target_os = "windows", target_os = "linux"),
                feature = "accelerated_osr"
            ))]
            if let Some(entry) = windows.get_mut(&window_id) {
                if let Some(host) = entry.browser.host() {
                    host.send_external_begin_frame();
                }
            }
            // Shell may exist before `on_after_created` — still queue paint so the window is not blank.
            rt.vmux_osr_redraw_queue.push_back(window_id);
        }
        WindowEvent::Resized(physical) => {
            if let Some(entry) = windows.get_mut(&window_id) {
                entry
                    .surface
                    .resize(&*crate::browser::event_loop::foreign_gpu(), physical);
                bootstrap::set_device_scale_factor(entry.surface.window.scale_factor() as f32);
                let logical = physical.to_logical(entry.surface.window.scale_factor());
                if let Ok(mut s) = entry.size.lock() {
                    *s = logical;
                }
                if let Some(host) = entry.browser.host() {
                    host.was_resized();
                    host.notify_screen_info_changed();
                    host.invalidate(PaintElementType::default());
                }
            } else if let Some(p) = rt
                .pending_browser_hosts
                .iter_mut()
                .find(|p| p.surface.window.id() == window_id)
            {
                p.surface
                    .resize(&*crate::browser::event_loop::foreign_gpu(), physical);
                bootstrap::set_device_scale_factor(p.surface.window.scale_factor() as f32);
                p.logical = physical.to_logical(p.surface.window.scale_factor());
                p.surface.window.request_redraw();
            }
        }
        WindowEvent::ScaleFactorChanged {
            scale_factor: _,
            inner_size_writer: _,
        } => {
            // macOS: this fires on backing scale changes (and sometimes during resizes).
            // Treat it like a resize to keep CEF's view rect and our surface in sync.
            let new_physical = if let Some(entry) = windows.get(&window_id) {
                entry.surface.window.inner_size()
            } else if let Some(p) = rt
                .pending_browser_hosts
                .iter_mut()
                .find(|p| p.surface.window.id() == window_id)
            {
                p.surface.window.inner_size()
            } else {
                return;
            };
            if let Some(entry) = windows.get_mut(&window_id) {
                entry
                    .surface
                    .resize(&*crate::browser::event_loop::foreign_gpu(), new_physical);
                bootstrap::set_device_scale_factor(entry.surface.window.scale_factor() as f32);
                let logical = new_physical.to_logical(entry.surface.window.scale_factor());
                if let Ok(mut s) = entry.size.lock() {
                    *s = logical;
                }
                if let Some(host) = entry.browser.host() {
                    host.notify_screen_info_changed();
                    host.was_resized();
                    host.invalidate(PaintElementType::default());
                }
                entry.surface.window.request_redraw();
            } else if let Some(p) = rt
                .pending_browser_hosts
                .iter_mut()
                .find(|p| p.surface.window.id() == window_id)
            {
                p.surface
                    .resize(&*crate::browser::event_loop::foreign_gpu(), new_physical);
                bootstrap::set_device_scale_factor(p.surface.window.scale_factor() as f32);
                p.logical = new_physical.to_logical(p.surface.window.scale_factor());
                p.surface.window.request_redraw();
            }
        }
        WindowEvent::Destroyed => {
            // The NSWindow can go away while `CefBrowserHandles` still holds a `Browser`
            // clone (we never called `try_close_browser`, or the system closed the window). Then
            // CEF keeps helpers alive and `on_before_close` may not run with an empty list, so
            // `shutdown` stays false and the main loop spins forever. Force-close the browser to
            // drive `on_before_close` and helper teardown.
            let removed_attached = if let Some(entry) = windows.remove(&window_id) {
                let bid = entry.browser.identifier();
                bevy_log::info!(
                    target: "vmux",
                    pid = std::process::id(),
                    "WindowEvent::Destroyed: winit window lost, force CEF close browser_id={bid}"
                );
                if let Some(host) = entry.browser.host() {
                    host.close_browser(1);
                }
                crate::browser::backend::osr::foreign_index::unregister_tab(
                    crate::browser::event_loop::foreign_osr_index().as_ref(),
                    bid,
                );
                true
            } else {
                false
            };
            if !removed_attached {
                drop(windows);
                let before = rt.pending_browser_hosts.len();
                rt.pending_browser_hosts
                    .retain(|p| p.surface.window.id() != window_id);
                let removed = before.saturating_sub(rt.pending_browser_hosts.len());
                for _ in 0..removed {
                    osr_host.cef_attach
                        .unpaired_cef_shells
                        .fetch_sub(1, Ordering::Release);
                }
                if removed > 0 {
                    bevy_log::info!(
                        target: "vmux",
                        pid = std::process::id(),
                        "WindowEvent::Destroyed: removed pending osr_host (browser not attached yet)"
                    );
                } else {
                    bevy_log::info!(
                        target: "vmux",
                        pid = std::process::id(),
                        "WindowEvent::Destroyed: unknown WindowId (no map entry, no pending osr_host) {window_id:?}"
                    );
                }
                return;
            }
        }
        _ => {}
    }
}