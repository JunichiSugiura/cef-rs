use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use cef::*;
use cef::sys::cef_event_flags_t;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::ModifiersState;
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{WindowAttributes, WindowId};

use super::bootstrap;
use super::demo_pages;
use super::gpu::WindowSurface;
use super::hub::{PendingOsrWindow, VmuxOsrAttach};
use super::input::{keyboard, mouse};
use super::{show_all_windows, track_window, titles};
use crate::shared::launch_trace;
use crate::shared::vmux_handler::VmuxHandler;

pub struct VmuxOsrApp {
    client_holder: Rc<RefCell<Option<Client>>>,
    osr_attach: VmuxOsrAttach,
    started: bool,
    last_cursor_pos: (i32, i32),
    mods: cef_event_flags_t,
    wheel_residual: (f64, f64),
    quit_requested: bool,
    /// Window shells waiting for async `browser_host_create_browser` + `on_after_created`.
    pending_browser_hosts: VecDeque<PendingOsrWindow>,
}

impl VmuxOsrApp {
    pub fn new(client_holder: Rc<RefCell<Option<Client>>>, osr_attach: VmuxOsrAttach) -> Self {
        Self {
            client_holder,
            osr_attach,
            started: false,
            last_cursor_pos: (0, 0),
            mods: cef_event_flags_t::EVENTFLAG_NONE,
            wheel_residual: (0.0, 0.0),
            quit_requested: false,
            pending_browser_hosts: VecDeque::new(),
        }
    }

    fn update_mods_from_winit(&mut self, m: ModifiersState) {
        self.mods = keyboard::update_mods_from_winit(m);
    }

    /// Call once per main-loop iteration **after** `cef::do_message_loop_work()` and **before**
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

}

impl ApplicationHandler for VmuxOsrApp {
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

        // Update modifier flags without holding the windows_store lock.
        if let WindowEvent::ModifiersChanged(m) = &event {
            self.update_mods_from_winit(m.state());
        }

        // Global app shortcuts that shouldn't depend on the current browser/window entry.
        if let WindowEvent::KeyboardInput { event: key, .. } = &event {
            let cmd = (self.mods.0 & cef_event_flags_t::EVENTFLAG_COMMAND_DOWN.0) != 0;
            if cmd
                && key.state == ElementState::Pressed
                && matches!(key.physical_key, PhysicalKey::Code(KeyCode::KeyQ))
            {
                // Quit: force-close all browsers. The normal shutdown path is driven by
                // `on_before_close` setting the shutdown flag once the last browser closes.
                self.quit_requested = true;
                if let Some(handler) = crate::shared::vmux_handler::VmuxHandler::instance() {
                    crate::shared::vmux_handler::VmuxHandler::close_all_browsers(&handler, true);
                }
                return;
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
                let Some(host) = entry.browser.host() else {
                    return;
                };

                host.set_focus(1);

                // Emacs-style Ctrl bindings for text fields (plus Cmd+A select-all).
                // We implement these at the OSR layer because web pages don't always get native
                // Cocoa text-system bindings when driven via synthetic key events.
                let ctrl = (self.mods.0 & cef_event_flags_t::EVENTFLAG_CONTROL_DOWN.0) != 0;
                let cmd = (self.mods.0 & cef_event_flags_t::EVENTFLAG_COMMAND_DOWN.0) != 0;
                if event.state == ElementState::Pressed {
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
                    if !has_shortcut_mod {
                        if let Some(text) = &event.text {
                        for ch in text.chars() {
                            keyboard::send_char(&host, self.mods, ch);
                        }
                        }
                    }
                }
            }
            WindowEvent::Ime(ime) => {
                let Some(entry) = windows.get(&window_id) else {
                    return;
                };
                let Some(host) = entry.browser.host() else {
                    return;
                };
                host.set_focus(1);
                if let winit::event::Ime::Commit(text) = ime {
                    for ch in text.chars() {
                        keyboard::send_char(&host, self.mods, ch);
                    }
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
                if let Some(entry) = windows.get(&window_id) {
                    if let Some(host) = entry.browser.host() {
                        host.set_focus(focused.into());
                    }
                }
            }
            WindowEvent::ModifiersChanged(_m) => {}
            WindowEvent::MouseInput { state, button, .. } => {
                let Some(entry) = windows.get(&window_id) else {
                    return;
                };
                let Some(host) = entry.browser.host() else {
                    return;
                };
                if matches!(state, ElementState::Pressed) {
                    host.set_focus(1);
                }
                let cef_button = match button {
                    MouseButton::Left => MouseButtonType::LEFT,
                    MouseButton::Right => MouseButtonType::RIGHT,
                    MouseButton::Middle => MouseButtonType::MIDDLE,
                    _ => return,
                };
                let mouse_up = matches!(state, ElementState::Released) as i32;
                let ev = MouseEvent {
                    x: self.last_cursor_pos.0,
                    y: self.last_cursor_pos.1,
                    modifiers: self.mods.0,
                };
                host.send_mouse_click_event(Some(&ev), cef_button, mouse_up, 1);
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                // Ignore inertial scroll completion; only send active scroll.
                if matches!(phase, TouchPhase::Cancelled | TouchPhase::Ended) {
                    return;
                }
                let Some(entry) = windows.get(&window_id) else {
                    return;
                };
                let browser = entry.browser.clone();
                let scale_factor = entry.surface.window.scale_factor();
                drop(windows);
                let Some(host) = browser.host() else {
                    return;
                };
                let mods = self.mods;
                let (dx, dy, mods) = mouse::wheel_to_cef(delta, mods);
                let _ = scale_factor; // maintained for clarity re: winit units
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
