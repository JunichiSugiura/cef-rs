use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cef::*;
use cef::sys::cef_event_flags_t;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{WindowAttributes, WindowId};

use super::bootstrap;
use super::event_loop::VmuxUserEvent;
use super::demo_pages;
use super::gpu::WindowSurface;
use super::hub::{PendingOsrWindow, VmuxOsrAttach};
use super::input::{keyboard, mouse};
use super::vim_modes::{self};
use super::vim_state::{OsrVimMachine, VimChromeCleanup};
use super::{show_all_windows, track_window, titles};
use crate::shared::launch_trace;
use crate::shared::settings::{chord_matches, ResolvedVimSettings};
use crate::shared::vmux_handler::VmuxHandler;

pub struct VmuxOsrApp {
    client_holder: Rc<RefCell<Option<Client>>>,
    osr_attach: VmuxOsrAttach,
    key_settings: Arc<ResolvedVimSettings>,
    /// Browse / link hints / insert / find / visual — see [`super::vim_state`].
    vim: OsrVimMachine,
    /// Left button held (e.g. drag-selecting); avoids stealing `v` for visual mode mid-selection.
    osr_primary_mouse_down: bool,
    started: bool,
    last_cursor_pos: (i32, i32),
    mods: cef_event_flags_t,
    wheel_residual: (f64, f64),
    quit_requested: bool,
    /// Window shells waiting for async `browser_host_create_browser` + `on_after_created`.
    pending_browser_hosts: VecDeque<PendingOsrWindow>,
    /// macOS: reclaim key window after CEF focuses `<input>` (often happens on the next UI tick).
    #[cfg(target_os = "macos")]
    macos_shell_refocus_ticks: u8,
    #[cfg(target_os = "macos")]
    macos_shell_refocus_window: Option<WindowId>,
    /// Spread post-`browser_host_create_browser` Chromium settle work across main-loop cycles
    /// instead of one tight `do_message_loop_work` burst (see [`super::cef_pump::main_tick`]).
    cef_post_create_pumps_remaining: u8,
}

impl VmuxOsrApp {
    pub fn new(
        client_holder: Rc<RefCell<Option<Client>>>,
        osr_attach: VmuxOsrAttach,
        key_settings: Arc<ResolvedVimSettings>,
    ) -> Self {
        Self {
            client_holder,
            osr_attach,
            key_settings,
            vim: OsrVimMachine::default(),
            osr_primary_mouse_down: false,
            started: false,
            last_cursor_pos: (0, 0),
            mods: cef_event_flags_t::EVENTFLAG_NONE,
            wheel_residual: (0.0, 0.0),
            quit_requested: false,
            pending_browser_hosts: VecDeque::new(),
            #[cfg(target_os = "macos")]
            macos_shell_refocus_ticks: 0,
            #[cfg(target_os = "macos")]
            macos_shell_refocus_window: None,
            cef_post_create_pumps_remaining: 0,
        }
    }

    fn defer_vim_key_after_editable_probe(
        &mut self,
        window_id: WindowId,
        browser_id: i32,
        event: &winit::event::KeyEvent,
    ) -> bool {
        VmuxHandler::post_editable_probe_for_vim_replay(browser_id, window_id, event.clone());
        true
    }

    fn apply_link_hint_feed_command(
        &mut self,
        window_id: WindowId,
        browser_id: i32,
        ch: char,
        outcome_still_active: bool,
        _hint_label_width: u8,
    ) {
        if self.vim.link_hints_browser_id() != Some(browser_id) {
            return;
        }
        if outcome_still_active {
            self.vim.link_hints_push_typed_char(ch);
        } else {
            VmuxHandler::link_hints_hide(browser_id);
            self.vim.clear_link_hints();
            VmuxHandler::invalidate_osr_editable_focus_hint(browser_id);
            VmuxHandler::schedule_osr_editable_focus_probe(browser_id);
        }
        self.nudge_after_vim_action(window_id);
    }

    /// After `finish_next_pending_browser_if_any` returns true, extend the multi-frame settle budget.
    pub fn bump_cef_post_create_pumps(&mut self, extra: u8) {
        self.cef_post_create_pumps_remaining = self
            .cef_post_create_pumps_remaining
            .saturating_add(extra)
            .min(64);
    }

    /// Drain up to `cap_per_frame` toward [`Self::cef_post_create_pumps_remaining`]; used by the main loop.
    pub fn drain_cef_post_create_pumps(&mut self, cap_per_frame: u8) -> u32 {
        let take = self
            .cef_post_create_pumps_remaining
            .min(cap_per_frame);
        self.cef_post_create_pumps_remaining -= take;
        take as u32
    }

    /// Call from the main pump after the CEF tick and after `pump_app_events`.
    pub fn pump_macos_shell_refocus(&mut self) {
        #[cfg(target_os = "macos")]
        {
            if self.macos_shell_refocus_ticks == 0 {
                return;
            }
            let Some(wid) = self.macos_shell_refocus_window else {
                self.macos_shell_refocus_ticks = 0;
                return;
            };
            if let Ok(windows) = self.osr_attach.windows_store.lock() {
                if let Some(entry) = windows.get(&wid) {
                    // Only take key when we don't already have it — avoids hammering AppKit every pump.
                    if !entry.surface.window.has_focus() {
                        entry.surface.window.focus_window();
                    }
                    // `focus_window` fixes NSApp key window; CEF can still think the browser blurred.
                    if let Some(host) = entry.browser.host() {
                        host.set_focus(1);
                    }
                }
            }
            self.macos_shell_refocus_ticks -= 1;
            if self.macos_shell_refocus_ticks == 0 {
                self.macos_shell_refocus_window = None;
            }
        }
    }

    fn update_mods_from_winit(&mut self, m: ModifiersState) {
        self.mods = keyboard::update_mods_from_winit(m);
    }

    /// Call once per main-loop iteration **after** [`super::cef_pump::main_tick`] and **before**
    /// `pump_app_events`. Uses **async** `browser_host_create_browser` (not sync): the blocking
    /// `browser_host_create_browser_sync` deadlocks with `external_message_pump` + our winit pump
    /// even when deferred by one frame. Helpers spawn only after the browser is actually created.
    ///
    /// Returns `true` if this call invoked `browser_host_create_browser`. If `shell_fifo` still
    /// holds a shell from the previous create (before `on_after_created` pops it), this returns
    /// `false` without dequeuing — issuing a second create while one is in flight crashes Chromium
    /// on macOS (process dies before the second `on_after_created`).
    pub fn finish_next_pending_browser_if_any(&mut self) -> bool {
        if self.pending_browser_hosts.is_empty() {
            return false;
        }
        {
            let fifo = self
                .osr_attach
                .shell_fifo
                .lock()
                .expect("vmux shell_fifo");
            if !fifo.is_empty() {
                return false;
            }
        }

        let Some(pending) = self.pending_browser_hosts.pop_front() else {
            return false;
        };

        let Some(mut client) = self
            .client_holder
            .borrow()
            .as_ref()
            .map(Client::clone)
        else {
            launch_trace("FATAL: finish_pending: Client is None");
            std::process::exit(1);
        };

        let url = CefString::from(pending.url.as_str());
        self.osr_attach
            .shell_fifo
            .lock()
            .expect("vmux shell_fifo")
            .push_back(pending);

        let accelerated_osr = cfg!(all(
            any(
                target_os = "macos",
                target_os = "windows",
                target_os = "linux"
            ),
            feature = "accelerated_osr"
        ));
        let window_info = WindowInfo {
            windowless_rendering_enabled: true as _,
            shared_texture_enabled: accelerated_osr as _,
            external_begin_frame_enabled: accelerated_osr as _,
            ..Default::default()
        };

        launch_trace("finish_pending: calling browser_host_create_browser (async)");
        let rc = browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&url),
            Some(&BrowserSettings {
                windowless_frame_rate: 60,
                ..Default::default()
            }),
            None,
            None,
        );
        // CEF documents success as non-zero; some builds use `1` specifically.
        if rc == 0 {
            let _ = self
                .osr_attach
                .shell_fifo
                .lock()
                .expect("vmux shell_fifo")
                .pop_back();
            launch_trace(&format!(
                "FATAL: browser_host_create_browser returned {rc} (failure)"
            ));
            std::process::exit(1);
        }
        if rc != 1 {
            launch_trace(&format!(
                "finish_pending: browser_host_create_browser returned {rc} (continuing; expected 1 on some CEF builds)"
            ));
        }
        launch_trace("finish_pending: browser_host_create_browser accepted (on_after_created next)");
        true
    }

    fn apply_pending_titles(&mut self) {
        let Ok(mut windows) = self.osr_attach.windows_store.lock() else {
            return;
        };
        for (browser_id, title) in titles::drain_titles() {
            if let Some(wid) = bootstrap::hub().window_id_for_browser(browser_id) {
                if let Some(entry) = windows.get_mut(&wid) {
                    entry.surface.window.set_title(&title);
                }
            }
        }
    }

    /// Create winit window + wgpu surface and queue async CEF browser for `url` / window title.
    fn spawn_osr_window(&mut self, event_loop: &ActiveEventLoop, url: &str, title: &str) {
        if self.client_holder.borrow().is_none() {
            launch_trace("FATAL: spawn_osr_window: Client is None (bootstrap did not run)");
            eprintln!(
                "vmux: fix OSR bootstrap or see /tmp/vmux-launch.log for earlier errors."
            );
            std::process::exit(1);
        }

        launch_trace(&format!("spawn_osr_window: create_window title={title:?}"));
        let window = Arc::new(
            event_loop
                .create_window(
                    WindowAttributes::default()
                        .with_title(title.to_string())
                        .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0)),
                )
                .expect("vmux-osr: create_window"),
        );
        launch_trace("spawn_osr_window: create_window OK");
        track_window(&window);
        // Some macOS configurations (especially when launched via `open`) won't surface the window
        // reliably unless we explicitly mark it visible. Do not call `focus_window()` here; that
        // has been observed to destabilize startup with CEF + external pump.
        window.set_visible(true);
        // Force a sane position on the primary display to avoid the “window exists but is off-screen”
        // class of bugs (Spaces / saved state / multi-monitor changes).
        window.set_minimized(false);
        let _ = window.set_outer_position(winit::dpi::PhysicalPosition::new(80i32, 80i32));
        window.request_user_attention(None);
        bootstrap::set_device_scale_factor(window.scale_factor() as f32);
        #[cfg(target_os = "macos")]
        {
            // Otherwise winit's NSTextInput path fights windowless CEF for first responder when an
            // `<input>` is focused, and key focus can jump to another app.
            window.set_ime_allowed(false);
        }

        launch_trace("spawn_osr_window: WindowSurface::new (wgpu surface)");
        let surface = WindowSurface::new(&*bootstrap::gpu(), window.clone());
        let logical = surface
            .window
            .inner_size()
            .to_logical::<f32>(surface.window.scale_factor());

        surface.window.request_redraw();

        self.pending_browser_hosts.push_back(PendingOsrWindow {
            surface,
            logical,
            url: url.to_string(),
        });
        self.osr_attach
            .unpaired_osr_shells
            .fetch_add(1, Ordering::Release);
        launch_trace("spawn_osr_window: queued pending CEF browser");
    }

    fn discard_vim_link_hints_for_browser_if_any(&mut self, bid: i32) {
        self.vim.clear_link_hints_if_browser(bid);
        let Some(wid) = bootstrap::hub().window_id_for_browser(bid) else {
            return;
        };
        // Always hide on navigation invalidation: Rust may already be `Browse` while the DOM
        // overlay remains (e.g. missed sync), which breaks a follow-up `f`.
        VmuxHandler::link_hints_hide(bid);
        self.nudge_after_vim_action(wid);
    }

    fn apply_link_hints_navigation_resets(&mut self) {
        let hub = bootstrap::hub();
        let ids = hub.take_link_hints_nav_invalidations();
        for bid in ids {
            self.discard_vim_link_hints_for_browser_if_any(bid);
            self.discard_vim_ux_for_browser_if_any(bid);
        }
    }

    fn cleanup_vim_ux_ui_at_window(&self, window_id: WindowId) {
        let Some(b) = self.browser_for_window(window_id) else {
            return;
        };
        match self.vim.chrome_cleanup() {
            Some(VimChromeCleanup::Find) => {
                vim_modes::find_ui_hide(&b);
                vim_modes::cef_stop_finding(&b, true);
            }
            Some(VimChromeCleanup::Visual) => vim_modes::visual_hint_hide(&b),
            None => {}
        }
    }

    fn clear_vim_ux_if_other_browser(&mut self, bid: i32) {
        let Some(old_bid) = self.vim.ux_browser_id() else {
            return;
        };
        if old_bid == bid {
            return;
        }
        self.discard_vim_ux_for_browser_if_any(old_bid);
    }

    /// End insert / find / visual for this browser without clearing `find_committed` (same-tab mode switch).
    fn teardown_vim_ux_at_window_keep_committed(&mut self, window_id: WindowId, bid: i32) {
        if self.vim.ux_browser_id() != Some(bid) {
            return;
        }
        self.cleanup_vim_ux_ui_at_window(window_id);
        self.vim.exit_ux_to_browse();
    }

    fn discard_vim_ux_for_browser_if_any(&mut self, bid: i32) {
        if self.vim.ux_browser_id() != Some(bid) {
            return;
        }
        let Some(wid) = bootstrap::hub().window_id_for_browser(bid) else {
            self.vim.exit_ux_to_browse();
            self.vim.find_committed.clear();
            return;
        };
        self.cleanup_vim_ux_ui_at_window(wid);
        self.vim.exit_ux_to_browse();
        self.vim.find_committed.clear();
        self.nudge_after_vim_action(wid);
    }

    fn handle_find_mode_pressed(
        &mut self,
        window_id: WindowId,
        event: &winit::event::KeyEvent,
    ) -> bool {
        use cef::sys::cef_event_flags_t as F;
        let mods = self.mods;
        let ctrl = (mods.0 & F::EVENTFLAG_CONTROL_DOWN.0) != 0;
        let cmd = (mods.0 & F::EVENTFLAG_COMMAND_DOWN.0) != 0;
        if ctrl || cmd {
            return false;
        }
        let Some(browser) = self.browser_for_window(window_id) else {
            return true;
        };
        let physical = &event.physical_key;

        match physical {
            PhysicalKey::Code(KeyCode::Escape) => {
                vim_modes::find_ui_hide(&browser);
                vim_modes::cef_stop_finding(&browser, true);
                self.vim.cancel_find();
                self.nudge_after_vim_action(window_id);
                return true;
            }
            PhysicalKey::Code(KeyCode::Enter) => {
                vim_modes::find_ui_hide(&browser);
                self.vim.finish_find_accept();
                if self.vim.find_committed.is_empty() {
                    vim_modes::cef_stop_finding(&browser, true);
                }
                self.nudge_after_vim_action(window_id);
                return true;
            }
            PhysicalKey::Code(KeyCode::Backspace) => {
                let Some(query) = self.vim.find_query_mut() else {
                    return true;
                };
                query.pop();
                vim_modes::find_ui_set_query(&browser, query);
                vim_modes::cef_find(&browser, query, true, false);
                self.nudge_after_vim_action(window_id);
                return true;
            }
            _ => {}
        }
        let Some(query) = self.vim.find_query_mut() else {
            return true;
        };
        if let Some(t) = &event.text {
            for ch in t.chars() {
                if !ch.is_control() {
                    query.push(ch);
                }
            }
            vim_modes::find_ui_set_query(&browser, query);
            vim_modes::cef_find(&browser, query, true, false);
            self.nudge_after_vim_action(window_id);
            return true;
        }
        true
    }

    fn nudge_after_vim_action(&self, window_id: WindowId) {
        let Ok(windows) = self.osr_attach.windows_store.lock() else {
            return;
        };
        let Some(entry) = windows.get(&window_id) else {
            return;
        };
        if let Some(h) = entry.browser.host() {
            h.invalidate(PaintElementType::VIEW);
            #[cfg(all(
                any(target_os = "macos", target_os = "windows", target_os = "linux"),
                feature = "accelerated_osr"
            ))]
            h.send_external_begin_frame();
        }
        entry.surface.window.request_redraw();
    }

    fn browser_for_window(&self, window_id: WindowId) -> Option<Browser> {
        self.osr_attach
            .windows_store
            .lock()
            .ok()
            .and_then(|w| w.get(&window_id).map(|e| e.browser.clone()))
    }

    /// Vim-style bindings from `settings.toml` (`[vim]`). Returns `true` if the key was consumed.
    fn try_handle_vim_keys(&mut self, window_id: WindowId, event: &winit::event::KeyEvent) -> bool {
        self.try_handle_vim_keys_inner(window_id, event, false)
    }

    fn try_handle_vim_keys_after_editable_probe(
        &mut self,
        window_id: WindowId,
        event: &winit::event::KeyEvent,
    ) -> bool {
        self.try_handle_vim_keys_inner(window_id, event, true)
    }

    /// `editable_hint_fresh`: editable-focus DOM probe has just run; skip scheduling another probe before branching.
    fn try_handle_vim_keys_inner(
        &mut self,
        window_id: WindowId,
        event: &winit::event::KeyEvent,
        editable_hint_fresh: bool,
    ) -> bool {
        let km = &*self.key_settings;
        let Some(bid) = self
            .osr_attach
            .windows_store
            .lock()
            .ok()
            .and_then(|w| w.get(&window_id).map(|e| e.browser.identifier()))
        else {
            return false;
        };

        if !km.enabled {
            if self.vim.link_hints_browser_id() == Some(bid) {
                VmuxHandler::link_hints_hide(bid);
                self.nudge_after_vim_action(window_id);
                self.vim.clear_link_hints();
            }
            if self.vim.ux_browser_id() == Some(bid) {
                self.cleanup_vim_ux_ui_at_window(window_id);
                self.vim.exit_ux_to_browse();
                self.vim.find_committed.clear();
            }
            return false;
        }

        if self.vim.find_swallows_keyup(bid) {
            if event.state == ElementState::Released {
                return true;
            }
        }

        if event.state != ElementState::Pressed {
            return false;
        }

        let mods = self.mods;
        let physical = &event.physical_key;
        let letter_press_no_winit_text = keyboard::physical_letter_press_without_winit_text(event);
        let key_sends_printable_text = keyboard::keyevent_has_printable_text(event);
        let now = Instant::now();

        if let Some(hid_bid) = self.vim.expire_link_hints_if_due(now) {
            VmuxHandler::link_hints_hide(hid_bid);
            self.nudge_after_vim_action(window_id);
        }

        // `LinkHints`: when typing the hint letters themselves we must NOT dismiss based on
        // "editable focus" probes, otherwise hints can disappear without activating a target.
        if self.vim.link_hints_active() {
            use cef::sys::cef_event_flags_t as F;
            let is_hint_letter = keyboard::lowercase_letter_from_physical(physical).is_some();
            let esc = match physical {
                PhysicalKey::Code(KeyCode::Escape) => true,
                _ => false,
            };
            let no_ctrl_alt_cmd = (mods.0
                & (F::EVENTFLAG_CONTROL_DOWN.0
                    | F::EVENTFLAG_ALT_DOWN.0
                    | F::EVENTFLAG_COMMAND_DOWN.0))
                == 0;

            // Dismiss only for keys other than plain hint letters.
            if !(is_hint_letter && no_ctrl_alt_cmd) {
                if !editable_hint_fresh {
                    return self.defer_vim_key_after_editable_probe(window_id, bid, event);
                }
                if !VmuxHandler::osr_may_handle_history_shortcuts(bid) {
                    VmuxHandler::link_hints_hide(bid);
                    self.vim.clear_link_hints();
                    self.nudge_after_vim_action(window_id);
                }
            }

            if !self.vim.link_hints_active() {
                // Dismissed above.
                return false;
            }

            if esc && no_ctrl_alt_cmd {
                VmuxHandler::link_hints_hide(bid);
                self.vim.clear_link_hints();
                self.nudge_after_vim_action(window_id);
                return true;
            }
            return false;
        }

        let window_ms = km.scroll_top_double_press_ms;
        if let Some(prev) = self.vim.scroll_g_pending {
            if now.duration_since(prev) > Duration::from_millis(window_ms) {
                self.vim.scroll_g_pending = None;
            }
        }

        if self.vim.is_insert(bid) {
            use cef::sys::cef_event_flags_t as F;
            let esc = match physical {
                PhysicalKey::Code(KeyCode::Escape) => true,
                _ => false,
            };
            let no_mod = (mods.0
                & (F::EVENTFLAG_CONTROL_DOWN.0
                    | F::EVENTFLAG_COMMAND_DOWN.0
                    | F::EVENTFLAG_ALT_DOWN.0
                    | F::EVENTFLAG_SHIFT_DOWN.0))
                == 0;
            let ctrl_ob = (mods.0 & F::EVENTFLAG_CONTROL_DOWN.0) != 0
                && (mods.0 & (F::EVENTFLAG_COMMAND_DOWN.0 | F::EVENTFLAG_ALT_DOWN.0)) == 0
                && match physical {
                    PhysicalKey::Code(KeyCode::BracketLeft) => true,
                    _ => false,
                };
            if (esc && no_mod) || ctrl_ob {
                self.vim.exit_ux_to_browse();
                return true;
            }
            return false;
        }

        if self.vim.is_find(bid) {
            return self.handle_find_mode_pressed(window_id, event);
        }

        if self.vim.is_visual(bid) {
            use cef::sys::cef_event_flags_t as F;
            let esc = match physical {
                PhysicalKey::Code(KeyCode::Escape) => true,
                _ => false,
            };
            let no_mod = (mods.0
                & (F::EVENTFLAG_CONTROL_DOWN.0
                    | F::EVENTFLAG_COMMAND_DOWN.0
                    | F::EVENTFLAG_ALT_DOWN.0
                    | F::EVENTFLAG_SHIFT_DOWN.0))
                == 0;
            if esc && no_mod {
                if let Some(b) = self.browser_for_window(window_id) {
                    vim_modes::visual_hint_hide(&b);
                }
                self.vim.exit_ux_to_browse();
                self.nudge_after_vim_action(window_id);
                return true;
            }
            let y_plain = (mods.0
                & (F::EVENTFLAG_SHIFT_DOWN.0
                    | F::EVENTFLAG_CONTROL_DOWN.0
                    | F::EVENTFLAG_COMMAND_DOWN.0
                    | F::EVENTFLAG_ALT_DOWN.0))
                == 0;
            if y_plain {
                match physical {
                    PhysicalKey::Code(KeyCode::KeyY) => {
                        if let Some(b) = self.browser_for_window(window_id) {
                            vim_modes::yank_selection(&b);
                            self.nudge_after_vim_action(window_id);
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
        // - `osr_may_handle_history_shortcuts`: block when probe/IME says **sure** text focus.
        // - First keydown uses `!editable_hint_fresh` → defer + DOM probe, then replay once.
        // - After replay, require `osr_vim_keys_safe_for_page` (probe **sure** not in an editable)
        //   before arming — avoids (a) an infinite defer loop when the hint never becomes
        //   `Some(false)`, and (b) arming hints while Google's search box has focus but the probe
        //   still said "page" for a moment. If we're not sure, pass `f` to the page.
        if let Some(ref chord) = km.hint_links {
            if chord_matches(chord, mods, physical) {
                if !VmuxHandler::osr_may_handle_history_shortcuts(bid) {
                    return false;
                }
                if !editable_hint_fresh {
                    return self.defer_vim_key_after_editable_probe(window_id, bid, event);
                }
                if !VmuxHandler::osr_vim_keys_safe_for_page(bid) {
                    return false;
                }
                VmuxHandler::link_hints_show(bid);
                self.vim.arm_link_hints(bid, now);
                self.nudge_after_vim_action(window_id);
                return true;
            }
        }

        // Browse: when the hint says focus is in a text control, pass unmodified keys to CEF.
        // Do **not** use `KeyEvent::text` here — winit sets it for almost every letter (`j`/`k`/…),
        // which would block all vim scrolling. Printable-text guarding is applied only where needed
        // (e.g. find-next/prev) and inside `page_ok` via `letter_press_no_winit_text` for IME.
        {
            use cef::sys::cef_event_flags_t as F;
            let plain = (mods.0
                & (F::EVENTFLAG_CONTROL_DOWN.0
                    | F::EVENTFLAG_COMMAND_DOWN.0
                    | F::EVENTFLAG_ALT_DOWN.0))
                == 0;
            if plain && VmuxHandler::osr_editable_focus_is_typing(bid) {
                return false;
            }
        }

        if let Some(ref chord) = km.history_back {
            if chord_matches(chord, mods, physical) {
                if !editable_hint_fresh {
                    return self.defer_vim_key_after_editable_probe(window_id, bid, event);
                }
                if VmuxHandler::osr_vim_keys_safe_for_page(bid) {
                    self.vim.scroll_g_pending = None;
                    VmuxHandler::set_active_browser(bid);
                    VmuxHandler::navigate_osr_browser(bid, false);
                    return true;
                }
                return false;
            }
        }
        if let Some(ref chord) = km.history_forward {
            if chord_matches(chord, mods, physical) {
                if !editable_hint_fresh {
                    return self.defer_vim_key_after_editable_probe(window_id, bid, event);
                }
                if VmuxHandler::osr_vim_keys_safe_for_page(bid) {
                    self.vim.scroll_g_pending = None;
                    VmuxHandler::set_active_browser(bid);
                    VmuxHandler::navigate_osr_browser(bid, true);
                    return true;
                }
                return false;
            }
        }

        if let Some(browser) = self.browser_for_window(window_id) {
            if !self.vim.find_committed.is_empty() {
                if let Some(ref chord) = km.find_next {
                    if chord_matches(chord, mods, physical) {
                        if VmuxHandler::osr_editable_focus_is_typing(bid)
                            || key_sends_printable_text
                        {
                            return false;
                        }
                        self.vim.scroll_g_pending = None;
                        vim_modes::cef_find(&browser, &self.vim.find_committed, true, true);
                        self.nudge_after_vim_action(window_id);
                        return true;
                    }
                }
                if let Some(ref chord) = km.find_prev {
                    if chord_matches(chord, mods, physical) {
                        if VmuxHandler::osr_editable_focus_is_typing(bid)
                            || key_sends_printable_text
                        {
                            return false;
                        }
                        self.vim.scroll_g_pending = None;
                        vim_modes::cef_find(&browser, &self.vim.find_committed, false, true);
                        self.nudge_after_vim_action(window_id);
                        return true;
                    }
                }
            }
        }

        // Avoid `refresh_osr_editable_focus_hint_for_history` on every key: it runs a DOM visit +
        // message-loop pump and can crash or corrupt CEF when re-entered while typing in an `<input>`.
        let might_mode_chord = [
            km.mode_insert.as_ref(),
            km.mode_find_open.as_ref(),
            km.mode_visual.as_ref(),
            km.yank_url.as_ref(),
        ]
        .into_iter()
        .flatten()
        .any(|c| chord_matches(c, mods, physical));

        if might_mode_chord {
            if !editable_hint_fresh {
                return self.defer_vim_key_after_editable_probe(window_id, bid, event);
            }
            // When `text` is missing, the real character may arrive only via `Ime::Commit`; do not
            // trust a stale "not editable" probe for mode chords (same class of bug as `d`/`g`/`r`).
            let page_ok_modes = VmuxHandler::osr_vim_keys_safe_for_page(bid)
                && !letter_press_no_winit_text;

            if let Some(browser) = self.browser_for_window(window_id) {
                if let Some(ref chord) = km.mode_insert {
                    if chord_matches(chord, mods, physical) && page_ok_modes {
                        self.clear_vim_ux_if_other_browser(bid);
                        self.teardown_vim_ux_at_window_keep_committed(window_id, bid);
                        self.vim.enter_insert(bid);
                        self.nudge_after_vim_action(window_id);
                        return true;
                    }
                }
                if let Some(ref chord) = km.mode_find_open {
                    if chord_matches(chord, mods, physical) && page_ok_modes {
                        self.clear_vim_ux_if_other_browser(bid);
                        self.teardown_vim_ux_at_window_keep_committed(window_id, bid);
                        self.vim.enter_find(bid);
                        vim_modes::find_ui_show(&browser);
                        vim_modes::find_ui_set_query(&browser, "");
                        self.nudge_after_vim_action(window_id);
                        return true;
                    }
                }
                if let Some(ref chord) = km.mode_visual {
                    if chord_matches(chord, mods, physical)
                        && page_ok_modes
                        && !self.osr_primary_mouse_down
                        && !self.vim.link_hints_active()
                    {
                        self.clear_vim_ux_if_other_browser(bid);
                        self.teardown_vim_ux_at_window_keep_committed(window_id, bid);
                        self.vim.enter_visual(bid);
                        vim_modes::visual_hint_show(&browser);
                        self.nudge_after_vim_action(window_id);
                        return true;
                    }
                }
                if let Some(ref chord) = km.yank_url {
                    if chord_matches(chord, mods, physical) && page_ok_modes {
                        self.vim.scroll_g_pending = None;
                        vim_modes::yank_page_url(&browser);
                        self.nudge_after_vim_action(window_id);
                        return true;
                    }
                }
            }
        }

        let matches_vim_content = [
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
        .any(|c| chord_matches(c, mods, physical));

        let page_ok = if matches_vim_content {
            if !editable_hint_fresh {
                return self.defer_vim_key_after_editable_probe(window_id, bid, event);
            }
            VmuxHandler::osr_vim_keys_safe_for_page(bid) && !letter_press_no_winit_text
        } else {
            false
        };
        if let Some(browser) = self.browser_for_window(window_id) {
            if let Some(ref chord) = km.scroll_line_down {
                if chord_matches(chord, mods, physical) {
                    if page_ok {
                        self.vim.scroll_g_pending = None;
                        super::vim_scroll::scroll_line_down(&browser);
                        self.nudge_after_vim_action(window_id);
                    }
                    return page_ok;
                }
            }
            if let Some(ref chord) = km.scroll_line_up {
                if chord_matches(chord, mods, physical) {
                    if page_ok {
                        self.vim.scroll_g_pending = None;
                        super::vim_scroll::scroll_line_up(&browser);
                        self.nudge_after_vim_action(window_id);
                    }
                    return page_ok;
                }
            }
            if let Some(ref chord) = km.scroll_page_down {
                if chord_matches(chord, mods, physical) {
                    if page_ok {
                        self.vim.scroll_g_pending = None;
                        super::vim_scroll::scroll_page_down(&browser);
                        self.nudge_after_vim_action(window_id);
                    }
                    return page_ok;
                }
            }
            if let Some(ref chord) = km.scroll_page_up {
                if chord_matches(chord, mods, physical) {
                    if page_ok {
                        self.vim.scroll_g_pending = None;
                        super::vim_scroll::scroll_page_up(&browser);
                        self.nudge_after_vim_action(window_id);
                    }
                    return page_ok;
                }
            }
            if let Some(ref chord) = km.scroll_bottom {
                if chord_matches(chord, mods, physical) {
                    if page_ok {
                        self.vim.scroll_g_pending = None;
                        super::vim_scroll::scroll_bottom(&browser);
                        self.nudge_after_vim_action(window_id);
                    }
                    return page_ok;
                }
            }
            if let Some(ref chord) = km.reload {
                if chord_matches(chord, mods, physical) {
                    if page_ok {
                        self.vim.scroll_g_pending = None;
                        VmuxHandler::set_active_browser(bid);
                        VmuxHandler::reload_osr_browser(bid);
                    }
                    return page_ok;
                }
            }

            if let Some(ref prefix) = km.scroll_top_prefix {
                if chord_matches(prefix, mods, physical) {
                    if !page_ok {
                        return false;
                    }
                    if let Some(prev) = self.vim.scroll_g_pending {
                        if now.duration_since(prev) <= Duration::from_millis(window_ms) {
                            self.vim.scroll_g_pending = None;
                            super::vim_scroll::scroll_top(&browser);
                            self.nudge_after_vim_action(window_id);
                            return true;
                        }
                    }
                    self.vim.scroll_g_pending = Some(now);
                    return true;
                }
            }
        }

        let prefix_matches = km
            .scroll_top_prefix
            .as_ref()
            .is_some_and(|p| chord_matches(p, mods, physical));
        if !prefix_matches {
            self.vim.scroll_g_pending = None;
        }

        false
    }
}

impl ApplicationHandler<VmuxUserEvent> for VmuxOsrApp {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: VmuxUserEvent) {
        match event {
            VmuxUserEvent::VimKeyReplay { window_id, event } => {
                let _ = self.try_handle_vim_keys_after_editable_probe(window_id, &event);
            }
            VmuxUserEvent::LinkHintFeed {
                window_id,
                browser_id,
                ch,
                still_active,
                hint_label_width,
                ..
            } => self.apply_link_hint_feed_command(
                window_id,
                browser_id,
                ch,
                still_active,
                hint_label_width,
            ),
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.started {
            return;
        }
        self.started = true;
        launch_trace(&format!(
            "winit ApplicationHandler::resumed (single OSR window → {})",
            demo_pages::STARTUP_URL
        ));
        self.spawn_osr_window(
            event_loop,
            demo_pages::STARTUP_URL,
            demo_pages::WINDOW_TITLE,
        );
        launch_trace("resumed: startup window queued (CEF attaches on next main-loop tick)");
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        self.apply_pending_titles();
        self.apply_link_hints_navigation_resets();

        // Update modifier flags without holding the windows_store lock.
        if let WindowEvent::ModifiersChanged(m) = &event {
            self.update_mods_from_winit(m.state());
        }

        // Vim-style bindings (`settings.toml` `[vim]`, defaults like j/k/d/u, shift+h/l, gg, shift+g, r).
        // Find mode swallows key-up here so CEF does not get unmatched KEYUP.
        if let WindowEvent::KeyboardInput { event, .. } = &event {
            if self.try_handle_vim_keys(window_id, event) {
                return;
            }
        }

        // History: **Cmd+[** / **Cmd+]** (macOS) or **Ctrl+[** / **Ctrl+]** (Windows/Linux), same as
        // Chromium window shortcuts. Unlike Shift+H/L, we do **not** consult the editable-focus hint
        // so back/forward still run from search fields and other inputs.
        if let WindowEvent::KeyboardInput { event, .. } = &event {
            if event.state == ElementState::Pressed {
                let cmd = (self.mods.0 & cef_event_flags_t::EVENTFLAG_COMMAND_DOWN.0) != 0;
                let ctrl = (self.mods.0 & cef_event_flags_t::EVENTFLAG_CONTROL_DOWN.0) != 0;
                let primary = if cfg!(target_os = "macos") {
                    cmd
                } else {
                    ctrl
                };
                if primary {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        let go_forward = match code {
                            KeyCode::BracketLeft => Some(false),
                            KeyCode::BracketRight => Some(true),
                            _ => None,
                        };
                        if let Some(go_forward) = go_forward {
                            let bid = self
                                .osr_attach
                                .windows_store
                                .lock()
                                .ok()
                                .and_then(|w| {
                                    w.get(&window_id)
                                        .map(|e| e.browser.identifier())
                                });
                            if let Some(bid) = bid {
                                VmuxHandler::set_active_browser(bid);
                                VmuxHandler::navigate_osr_browser(bid, go_forward);
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
                let alt = (self.mods.0 & cef_event_flags_t::EVENTFLAG_ALT_DOWN.0) != 0;
                let cmd = (self.mods.0 & cef_event_flags_t::EVENTFLAG_COMMAND_DOWN.0) != 0;
                let ctrl = (self.mods.0 & cef_event_flags_t::EVENTFLAG_CONTROL_DOWN.0) != 0;
                if alt && !cmd && !ctrl {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        let go_forward = match code {
                            KeyCode::ArrowLeft => Some(false),
                            KeyCode::ArrowRight => Some(true),
                            _ => None,
                        };
                        if let Some(go_forward) = go_forward {
                            let bid = self
                                .osr_attach
                                .windows_store
                                .lock()
                                .ok()
                                .and_then(|w| {
                                    w.get(&window_id)
                                        .map(|e| e.browser.identifier())
                                });
                            if let Some(bid) = bid {
                                VmuxHandler::set_active_browser(bid);
                                VmuxHandler::navigate_osr_browser(bid, go_forward);
                                return;
                            }
                        }
                    }
                }
            }
        }

        // Global app shortcuts that shouldn't depend on the current browser/window entry.
        if let WindowEvent::KeyboardInput { event: key, .. } = &event {
            let cmd = (self.mods.0 & cef_event_flags_t::EVENTFLAG_COMMAND_DOWN.0) != 0;
            if cmd && key.state == ElementState::Pressed {
                match key.physical_key {
                    PhysicalKey::Code(KeyCode::KeyQ) => {
                        // Quit: force-close all browsers. The normal shutdown path is driven by
                        // `on_before_close` setting the shutdown flag once the last browser closes.
                        self.quit_requested = true;
                        if let Some(handler) = crate::shared::vmux_handler::VmuxHandler::instance() {
                            crate::shared::vmux_handler::VmuxHandler::close_all_browsers(
                                &handler, true,
                            );
                        }
                        return;
                    }
                    _ => {}
                }
            }
        }

        let Ok(mut windows) = self.osr_attach.windows_store.lock() else {
            return;
        };

        match event {
            WindowEvent::KeyboardInput { event, .. } => {
                let Some(entry) = windows.get(&window_id) else {
                    return;
                };
                let browser = entry.browser.clone();
                let bid = browser.identifier();
                let ctrl = (self.mods.0 & cef_event_flags_t::EVENTFLAG_CONTROL_DOWN.0) != 0;
                let cmd = (self.mods.0 & cef_event_flags_t::EVENTFLAG_COMMAND_DOWN.0) != 0;
                let alt = (self.mods.0 & cef_event_flags_t::EVENTFLAG_ALT_DOWN.0) != 0;
                let _shift = (self.mods.0 & cef_event_flags_t::EVENTFLAG_SHIFT_DOWN.0) != 0;

                // Drop the store before CEF / follow-ups: `request_redraw_after_new_texture` may
                // touch the compositor and re-lock `windows_store`. Same pattern as `MouseWheel`.
                drop(windows);

                if let Some(h) = browser.host() {
                    h.set_focus(1);
                }

                let hint_ch = keyboard::lowercase_letter_from_physical(&event.physical_key);

                // While `LinkHints` is armed, do **not** run editable-focus dismiss from here.
                // Google (and similar) often keeps a search `<input>` in the tree; probing after
                // `do_message_loop_work` can flip to "editable" mid-hint-sequence, clear Rust hint
                // mode, and the **next** letter is delivered to CEF — so you "suddenly type in the
                // search box" after a few hint keys. Dismiss hints via Esc (`try_handle_vim_keys`)
                // or when the feed reports hints ended / JS cleans up.
                // Hint letters are routed only while Rust `LinkHints` mode is active (armed by `f`).
                // Feeding runs on the CEF UI thread; completion is posted as [`super::event_loop::VmuxUserEvent::LinkHintFeed`].
                if self.vim.link_hints_active() {
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
                                    let prior = self.vim.link_hints_typed_prefix().len();
                                    VmuxHandler::link_hints_feed_key_deferred(bid, ch, prior);
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

                let Ok(windows) = self.osr_attach.windows_store.lock() else {
                    return;
                };
                let Some(entry) = windows.get(&window_id) else {
                    return;
                };
                let Some(host) = entry.browser.host() else {
                    return;
                };

                let hints_active = self.vim.link_hints_active();

                // Emacs-style Ctrl bindings for text fields (plus Cmd+A select-all).
                // We implement these at the OSR layer because web pages don't always get native
                // Cocoa text-system bindings when driven via synthetic key events.
                if !hints_active && event.state == ElementState::Pressed {
                    if let PhysicalKey::Code(code) = event.physical_key {
                        // Cmd+A => Select All
                        if cmd && code == KeyCode::KeyA {
                            if let Some(frame) = entry.browser.focused_frame().or_else(|| entry.browser.main_frame()) {
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
                            // On macOS shortcuts (e.g. Cmd+A) often require KEYDOWN delivery,
                            // while some navigation expects RAWKEYDOWN. Send both.
                            for type_ in [KeyEventType::RAWKEYDOWN, KeyEventType::KEYDOWN] {
                                let kev = KeyEvent {
                                    type_,
                                    modifiers: self.mods.0,
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
                                modifiers: self.mods.0,
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
                    let has_shortcut_mod = (self.mods.0
                        & (cef_event_flags_t::EVENTFLAG_COMMAND_DOWN.0
                            | cef_event_flags_t::EVENTFLAG_CONTROL_DOWN.0))
                        != 0;
                    if !hints_active && !has_shortcut_mod {
                        if let Some(text) = &event.text {
                            let mut any = false;
                            for ch in text.chars() {
                                keyboard::send_char(&host, self.mods, ch);
                                any = true;
                            }
                            // Sites like Ledger use search UIs that our DOM probe often misses; once
                            // printable text is injected, treat focus as typing so `d`/`g`/`r` vim
                            // bindings do not eat the rest of the word (e.g. "ledger" → "lee").
                            if any {
                                VmuxHandler::set_osr_editable_focus_hint(bid, true);
                            }
                        }
                    }
                }

                if event.state == ElementState::Pressed && !hints_active {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::Tab) => {
                            let bid = entry.browser.identifier();
                            VmuxHandler::invalidate_osr_editable_focus_hint(bid);
                            VmuxHandler::schedule_osr_editable_focus_probe(bid);
                        }
                        _ => {}
                    }
                }
            }
            WindowEvent::Ime(ime) => {
                let Some(entry) = windows.get(&window_id) else {
                    return;
                };
                #[cfg(target_os = "macos")]
                let window = entry.surface.window.clone();
                let Some(host) = entry.browser.host() else {
                    return;
                };
                host.set_focus(1);
                let bid = entry.browser.identifier();
                // `KeyboardInput` for hint letters is swallowed, but macOS still emits `Ime::Commit`
                // for the same physical key; forwarding it would inject into the page (e.g. second
                // letter of hint "by") and sites like Ledger show "press / for search" when focus
                // churns.
                match &ime {
                    winit::event::Ime::Commit(_) => {
                        if self.vim.link_hints_active()
                            && self.vim.link_hints_browser_id() == Some(bid)
                        {
                            return;
                        }
                        VmuxHandler::set_osr_editable_focus_hint(bid, true);
                    }
                    winit::event::Ime::Disabled => {
                        VmuxHandler::invalidate_osr_editable_focus_hint(bid);
                        VmuxHandler::schedule_osr_editable_focus_probe(bid);
                    }
                    _ => {
                        VmuxHandler::set_osr_editable_focus_hint(bid, true);
                    }
                }
                if let winit::event::Ime::Commit(text) = ime {
                    for ch in text.chars() {
                        keyboard::send_char(&host, self.mods, ch);
                    }
                }
                // macOS can hand first responder to IME helpers in another activation context;
                // keep the winit shell key so typing stays in this window.
                #[cfg(target_os = "macos")]
                {
                    if !window.has_focus() {
                        window.focus_window();
                    }
                    host.set_focus(1);
                    self.macos_shell_refocus_window = Some(window_id);
                    self.macos_shell_refocus_ticks = self.macos_shell_refocus_ticks.max(8);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(entry) = windows.get(&window_id) {
                    if let Some(host) = entry.browser.host() {
                        // CEF OSR expects DIP (logical) coordinates.
                        self.last_cursor_pos =
                            mouse::cursor_moved_dip(position, entry.surface.window.scale_factor());
                        let ev = MouseEvent {
                            x: self.last_cursor_pos.0,
                            y: self.last_cursor_pos.1,
                            modifiers: self.mods.0,
                        };
                        host.send_mouse_move_event(Some(&ev), 0);
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.osr_primary_mouse_down = false;
                if let Some(entry) = windows.get(&window_id) {
                    if let Some(host) = entry.browser.host() {
                        let ev = MouseEvent {
                            x: self.last_cursor_pos.0,
                            y: self.last_cursor_pos.1,
                            modifiers: self.mods.0,
                        };
                        host.send_mouse_move_event(Some(&ev), 1);
                    }
                }
            }
            WindowEvent::Focused(focused) => {
                if !focused {
                    self.osr_primary_mouse_down = false;
                }
                #[cfg(target_os = "macos")]
                if !focused {
                    // User left this window (or transient IME blur) — stop the pump from calling
                    // `focus_window` in a loop, which would steal key window back from other apps.
                    self.macos_shell_refocus_ticks = 0;
                    self.macos_shell_refocus_window = None;
                    // Winit/AppKit often emits a transient blur while the shell window is still the
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
                            VmuxHandler::set_active_browser(entry.browser.identifier());
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
                        VmuxHandler::set_active_browser(bid);
                        VmuxHandler::navigate_osr_browser(bid, false);
                        return;
                    }
                    (ElementState::Pressed, MouseButton::Forward) => {
                        let bid = entry.browser.identifier();
                        drop(windows);
                        VmuxHandler::set_active_browser(bid);
                        VmuxHandler::navigate_osr_browser(bid, true);
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
                        VmuxHandler::set_active_browser(entry.browser.identifier());
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
                        self.osr_primary_mouse_down = true;
                    }
                    (MouseButton::Left, ElementState::Released) => {
                        self.osr_primary_mouse_down = false;
                    }
                    _ => {}
                }
                let mouse_up = match state {
                    ElementState::Released => 1,
                    _ => 0,
                };
                let ev = MouseEvent {
                    x: self.last_cursor_pos.0,
                    y: self.last_cursor_pos.1,
                    modifiers: self.mods.0,
                };
                host.send_mouse_click_event(Some(&ev), cef_button, mouse_up, 1);
                // Re-hit-test hover/cursor after click so `on_cursor_change` runs (I-beam on inputs).
                if cef_button == MouseButtonType::LEFT && mouse_up == 1 {
                    host.send_mouse_move_event(Some(&ev), 0);
                    let bid = entry.browser.identifier();
                    drop(windows);
                    VmuxHandler::invalidate_osr_editable_focus_hint(bid);
                    // Async probe alone can finish after the first keystroke, so `d`/`g`/`r` vim
                    // bindings still run with a stale "not editable" hint — sync refresh before typing.
                    VmuxHandler::refresh_osr_editable_focus_hint_for_history(bid);
                    return;
                }
                // Windowless CEF often has no real NSView for the page; after focusing an `<input>`,
                // Chromium can resign our shell window’s key status and another app becomes active.
                // Only arm refocus on press — release would re-steal key after drag-release outside.
                #[cfg(target_os = "macos")]
                match state {
                    ElementState::Pressed => {
                        if !window.has_focus() {
                            window.focus_window();
                        }
                        self.macos_shell_refocus_window = Some(window_id);
                        self.macos_shell_refocus_ticks = self.macos_shell_refocus_ticks.max(10);
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

                let mods = self.mods;
                let (dx, dy, mods) = mouse::wheel_to_cef(delta, mods);

                let Some(host) = browser.host() else {
                    return;
                };
                let Some((dx_i, dy_i)) = mouse::take_wheel_deltas_i32(&mut self.wheel_residual, dx, dy) else {
                    return;
                };
                let ev = MouseEvent {
                    x: self.last_cursor_pos.0,
                    y: self.last_cursor_pos.1,
                    modifiers: mods.0,
                };
                host.send_mouse_wheel_event(Some(&ev), dx_i, dy_i);
            }
            WindowEvent::CloseRequested => {
                if windows.contains_key(&window_id) {
                    drop(windows);
                    VmuxHandler::arm_windowless_close_from_winit();
                    let Ok(windows) = self.osr_attach.windows_store.lock() else {
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
                if let Some(entry) = windows.get_mut(&window_id) {
                    #[cfg(all(
                        any(target_os = "macos", target_os = "windows", target_os = "linux"),
                        feature = "accelerated_osr"
                    ))]
                    if let Some(host) = entry.browser.host() {
                        host.send_external_begin_frame();
                    }
                    let id = entry.browser.identifier();
                    let gpu = bootstrap::gpu();
                    let painted = {
                        let surface = &mut entry.surface;
                        bootstrap::hub()
                            .with_bind_group(id, |bg| {
                                surface.render(&*gpu, Some(bg));
                            })
                            .is_some()
                    };
                    if !painted {
                        entry.surface.render(&*gpu, None);
                    }
                    entry.surface.window.request_redraw();
                } else {
                    // Shell exists but CEF has not run `on_after_created` yet — still must paint.
                    // Otherwise macOS can leave the `NSWindow` blank / effectively invisible.
                    let gpu = bootstrap::gpu();
                    if let Some(p) = self
                        .pending_browser_hosts
                        .iter_mut()
                        .find(|p| p.surface.window.id() == window_id)
                    {
                        p.surface.render(&*gpu, None);
                        p.surface.window.request_redraw();
                    }
                }
            }
            WindowEvent::Resized(physical) => {
                if let Some(entry) = windows.get_mut(&window_id) {
                    entry.surface.resize(&*bootstrap::gpu(), physical);
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
                } else if let Some(p) = self
                    .pending_browser_hosts
                    .iter_mut()
                    .find(|p| p.surface.window.id() == window_id)
                {
                    p.surface.resize(&*bootstrap::gpu(), physical);
                    bootstrap::set_device_scale_factor(p.surface.window.scale_factor() as f32);
                    p.logical = physical.to_logical(p.surface.window.scale_factor());
                    p.surface.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor: _, inner_size_writer: _ } => {
                // macOS: this fires on backing scale changes (and sometimes during resizes).
                // Treat it like a resize to keep CEF's view rect and our surface in sync.
                let new_physical = if let Some(entry) = windows.get(&window_id) {
                    entry.surface.window.inner_size()
                } else if let Some(p) = self
                    .pending_browser_hosts
                    .iter_mut()
                    .find(|p| p.surface.window.id() == window_id)
                {
                    p.surface.window.inner_size()
                } else {
                    return;
                };
                if let Some(entry) = windows.get_mut(&window_id) {
                    entry.surface.resize(&*bootstrap::gpu(), new_physical);
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
                } else if let Some(p) = self
                    .pending_browser_hosts
                    .iter_mut()
                    .find(|p| p.surface.window.id() == window_id)
                {
                    p.surface.resize(&*bootstrap::gpu(), new_physical);
                    bootstrap::set_device_scale_factor(p.surface.window.scale_factor() as f32);
                    p.logical = new_physical.to_logical(p.surface.window.scale_factor());
                    p.surface.window.request_redraw();
                }
            }
            WindowEvent::Destroyed => {
                // The NSWindow can go away while `VmuxHandler::browser_list` still holds a `Browser`
                // clone (we never called `try_close_browser`, or the system closed the window). Then
                // CEF keeps helpers alive and `on_before_close` may not run with an empty list, so
                // `shutdown` stays false and the main loop spins forever. Force-close the browser to
                // drive `on_before_close` and helper teardown.
                let removed_attached = if let Some(entry) = windows.remove(&window_id) {
                    let bid = entry.browser.identifier();
                    launch_trace(&format!(
                        "WindowEvent::Destroyed: winit window lost, force CEF close browser_id={bid}"
                    ));
                    if let Some(host) = entry.browser.host() {
                        host.close_browser(1);
                    }
                    bootstrap::hub().unregister_browser(bid);
                    true
                } else {
                    false
                };
                if !removed_attached {
                    drop(windows);
                    let before = self.pending_browser_hosts.len();
                    self.pending_browser_hosts
                        .retain(|p| p.surface.window.id() != window_id);
                    let removed = before.saturating_sub(self.pending_browser_hosts.len());
                    for _ in 0..removed {
                        self.osr_attach
                            .unpaired_osr_shells
                            .fetch_sub(1, Ordering::Release);
                    }
                    if removed > 0 {
                        launch_trace(
                            "WindowEvent::Destroyed: removed pending shell (browser not attached yet)",
                        );
                    } else {
                        launch_trace(&format!(
                            "WindowEvent::Destroyed: unknown WindowId (no map entry, no pending shell) {window_id:?}"
                        ));
                    }
                    return;
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        self.apply_pending_titles();
        // Keep nudging windows visible while we're still debugging “no window”.
        show_all_windows();

        // If quit was requested (Cmd+Q) but CEF doesn't deliver `on_before_close` for some reason,
        // fall back to exiting once all OSR windows/shells are gone.
        if self.quit_requested {
            let ws_empty = self
                .osr_attach
                .windows_store
                .lock()
                .map(|m| m.is_empty())
                .unwrap_or(false);
            let fifo_empty = self
                .osr_attach
                .shell_fifo
                .lock()
                .map(|q| q.is_empty())
                .unwrap_or(false);
            let pending_empty = self.pending_browser_hosts.is_empty();
            if ws_empty && fifo_empty && pending_empty {
                if let Some(flag) = crate::shared::vmux_osr::shutdown::shutdown_flag() {
                    flag.store(true, std::sync::atomic::Ordering::Release);
                }
            }
        }

        let Ok(windows) = self.osr_attach.windows_store.lock() else {
            return;
        };
        for entry in windows.values() {
            entry.surface.window.request_redraw();
        }
        drop(windows);
        for p in &self.pending_browser_hosts {
            p.surface.window.request_redraw();
        }
    }
}
