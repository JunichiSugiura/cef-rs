//! Winit runner / CEF pump integration: startup window, pending browser creation, redraw nudges.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cef::*;
use winit::event_loop::ActiveEventLoop;
use winit::window::WindowAttributes;

use crate::browser::backend::cef::bootstrap;
use crate::browser::event_loop::{foreign_osr_index, RuntimeState};
use crate::browser::request_context::{VmuxRequestContextHandler, VmuxRequestContextHandlerBuilder};
use crate::browser::event_loop::ShutdownFlag;
use crate::browser::backend::osr::hub::PendingCefWindow;
use crate::browser::renderer::gpu::device::WindowSurface;
use crate::window::registry::{show_all_windows, track_window};

use super::state::OsrHostState;
use super::titles;

const WINDOW_TITLE: &str = "vmux";
/// If orderly quit never clears OSR maps (stuck CEF close), still exit the runner so the process can
/// run `cef::shutdown()` — otherwise Cmd+Q spins forever with `close_all count=1` every press.
const FORCE_QUIT_AFTER: Duration = Duration::from_millis(1200);

impl OsrHostState {
    /// Initial URL for `browser_host_create_browser` plus optional follow-up navigation.
    ///
    /// `http`/`https` startup URLs load **directly** so session history does not retain a leading
    /// `about:blank` entry (Back would otherwise leave the user on a blank page).
    ///
    /// Empty config still uses `about:blank` first, then the default startup URL — same deferred
    /// path as before (`deferred_url_after_blank`).
    pub fn staged_initial_navigation_url(startup_url: &str) -> (String, Option<String>) {
        let t = startup_url.trim();
        if t.is_empty() {
            return (
                "about:blank".to_string(),
                Some(crate::settings::DEFAULT_STARTUP_URL.to_string()),
            );
        }
        if t.eq_ignore_ascii_case("about:blank") {
            return ("about:blank".to_string(), None);
        }
        if t.starts_with("http://") || t.starts_with("https://") {
            return (t.to_string(), None);
        }
        (t.to_string(), None)
    }

    pub(crate) fn apply_pending_titles(&mut self) {
        let Ok(mut windows) = self.cef_attach.windows_store.lock() else {
            return;
        };
        for (browser_id, title) in titles::drain_titles() {
            if let Some(wid) = crate::browser::backend::osr::foreign_index::window_id_for_browser(
                crate::browser::event_loop::foreign_osr_index().as_ref(),
                browser_id,
            ) {
                if let Some(entry) = windows.get_mut(&wid) {
                    entry.surface.window.set_title(&title);
                }
            }
        }
    }

    /// Create winit window + wgpu surface and queue async CEF browser for `url` / window title.
    pub(crate) fn spawn_cef_browser_window(
        &mut self,
        rt: &mut RuntimeState,
        event_loop: &ActiveEventLoop,
        url: &str,
        title: &str,
    ) {
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "spawn_cef_browser_window: create_window title={title:?}"
        );
        let window = match event_loop.create_window(
            WindowAttributes::default()
                .with_title(title.to_string())
                .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0)),
        ) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                bevy_log::error!(
                    target: "vmux",
                    pid = std::process::id(),
                    "FATAL: winit create_window failed: {e:?}"
                );
                eprintln!("vmux: create_window failed: {e:?}");
                std::process::exit(1);
            }
        };
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "spawn_cef_browser_window: create_window OK"
        );
        #[cfg(target_os = "macos")]
        bootstrap::init_browser_client_macos_after_first_window(
            self.client_holder.as_ref(),
            self.cef_attach.clone(),
            window.clone(),
        );
        if self.client_holder.borrow().is_none() {
            bevy_log::error!(
                target: "vmux",
                pid = std::process::id(),
                "FATAL: spawn_cef_browser_window: Client is None (bootstrap did not run)"
            );
            eprintln!("vmux: fix OSR bootstrap or check RUST_LOG=vmux=info for earlier errors.");
            std::process::exit(1);
        }
        track_window(&window);
        window.set_visible(true);
        window.set_minimized(false);
        let _ = window.set_outer_position(winit::dpi::PhysicalPosition::new(80i32, 80i32));
        window.request_user_attention(None);
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "proof: winit_window_mapped_visible_requested title={title:?} (native NSWindow should appear even if CEF dies next)"
        );
        crate::lifecycle_trace::record_startup_milestone("proof_winit_window_visible_requested");
        bootstrap::set_device_scale_factor(window.scale_factor() as f32);
        #[cfg(target_os = "macos")]
        {
            window.set_ime_allowed(false);
        }

        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "spawn_cef_browser_window: WindowSurface::new (wgpu surface)"
        );
        let surface = match WindowSurface::new(
            &*crate::browser::event_loop::foreign_gpu(),
            window.clone(),
        ) {
            Ok(s) => s,
            Err(e) => {
                bevy_log::error!(
                    target: "vmux",
                    pid = std::process::id(),
                    "FATAL: WindowSurface (wgpu): {e}"
                );
                eprintln!("vmux: WindowSurface failed: {e}");
                std::process::exit(1);
            }
        };
        let logical = surface
            .window
            .inner_size()
            .to_logical::<f32>(surface.window.scale_factor());

        surface.window.request_redraw();
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "proof: wgpu_surface_ready_for_osr logical={}x{}",
            logical.width,
            logical.height
        );
        crate::lifecycle_trace::record_startup_milestone("proof_wgpu_surface_ready_for_osr");

        let (initial_url, deferred_url) = Self::staged_initial_navigation_url(url);
        rt.pending_browser_hosts.push_back(PendingCefWindow {
            surface,
            logical,
            url: initial_url,
            deferred_url,
        });
        self.cef_attach
            .unpaired_cef_shells
            .fetch_add(1, Ordering::Release);
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "spawn_cef_browser_window: queued pending CEF browser"
        );
    }

    /// **macOS:** after async [`browser_host_create_browser`], poll
    /// [`browser_host_get_browser_by_identifier`] (no Rust `LifeSpanHandler::on_after_created`).
    #[cfg(target_os = "macos")]
    pub(crate) fn macos_poll_cef_browser_attach(&self, rt: &mut RuntimeState) {
        if !rt.macos_poll_attach_after_create {
            return;
        }
        {
            let fifo = self
                .cef_attach
                .shell_fifo
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if fifo.is_empty() {
                return;
            }
        }
        for bid in 1..=32i32 {
            if browser_host_get_browser_by_identifier(bid).is_some() {
                rt.macos_poll_attach_after_create = false;
                crate::lifecycle_trace::record_startup_milestone("macos_polled_after_created_dispatch");
                bevy_log::info!(
                    target: "vmux",
                    pid = std::process::id(),
                    "macos: poll discovered browser_id={bid} — synthetic AfterCreated (no Rust lifespan handler)"
                );
                crate::browser::event_loop::send_user_event(
                    crate::browser::event_loop::UserEvent::Cef(
                        crate::browser::event_loop::CefEvent::AfterCreatedBrowserCallback(
                            crate::browser::events::AfterCreatedBrowserCallbackEvent { browser_id: bid },
                        ),
                    ),
                );
                return;
            }
        }
    }

    pub(crate) fn pump_macos_shell_refocus(&mut self, rt: &mut RuntimeState) {
        #[cfg(target_os = "macos")]
        {
            if rt.macos_shell_refocus_ticks == 0 {
                return;
            }
            let Some(wid) = rt.macos_shell_refocus_window else {
                rt.macos_shell_refocus_ticks = 0;
                return;
            };
            if let Ok(windows) = self.cef_attach.windows_store.lock() {
                if let Some(entry) = windows.get(&wid) {
                    if !entry.surface.window.has_focus() {
                        entry.surface.window.focus_window();
                    }
                    if let Some(host) = entry.browser.host() {
                        host.set_focus(1);
                    }
                }
            }
            rt.macos_shell_refocus_ticks -= 1;
            if rt.macos_shell_refocus_ticks == 0 {
                rt.macos_shell_refocus_window = None;
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (self, rt);
        }
    }

    /// Call once per main-loop iteration **after** the leading CEF pump and **before** `pump_app_events`.
    ///
    /// **Pre-refactor parity / `examples/osr`:** async [`browser_host_create_browser`] everywhere; **macOS**
    /// uses no Rust `LifeSpanHandler` and attaches via [`Self::macos_poll_cef_browser_attach`] after pumps
    /// (`browser_host_create_browser_sync` traps inside Chromium from this stack on some macOS 26 + CEF 146 builds).
    pub(crate) fn finish_next_pending_browser_if_any(&mut self, rt: &mut RuntimeState) -> bool {
        if rt.pending_browser_hosts.is_empty() {
            return false;
        }
        {
            let fifo = self
                .cef_attach
                .shell_fifo
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if !fifo.is_empty() {
                return false;
            }
        }

        let Some(pending) = rt.pending_browser_hosts.pop_front() else {
            return false;
        };

        let Some(mut client) = self.client_holder.borrow().as_ref().map(Client::clone) else {
            bevy_log::error!(
                target: "vmux",
                pid = std::process::id(),
                "FATAL: finish_pending: Client is None"
            );
            std::process::exit(1);
        };

        let url = CefString::from(pending.url.as_str());
        let pre_attach_sz = pending.logical;
        self.cef_attach
            .shell_fifo
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_back(pending);
        if let Ok(mut g) = foreign_osr_index().pre_attach_view_logical.lock() {
            *g = Some(pre_attach_sz);
        }

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

        let mut req_ctx_handler = VmuxRequestContextHandlerBuilder::build(VmuxRequestContextHandler {});
        let Some(mut request_context) = request_context_create_context(
            Some(&RequestContextSettings::default()),
            Some(&mut req_ctx_handler),
        ) else {
            let _ = self
                .cef_attach
                .shell_fifo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_back();
            bevy_log::error!(
                target: "vmux",
                pid = std::process::id(),
                "FATAL: request_context_create_context returned None"
            );
            std::process::exit(1);
        };
        let browser_settings = BrowserSettings {
            windowless_frame_rate: 60,
            ..Default::default()
        };

        crate::lifecycle_trace::record_startup_milestone("finish_pending_before_create_browser");

        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "finish_pending: browser_host_create_browser (async)"
        );
        let rc = browser_host_create_browser(
            Some(&window_info),
            Some(&mut client),
            Some(&url),
            Some(&browser_settings),
            None,
            Some(&mut request_context),
        );

        if rc == 0 {
            let _ = self
                .cef_attach
                .shell_fifo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_back();
            if let Ok(mut g) = foreign_osr_index().pre_attach_view_logical.lock() {
                g.take();
            }
            bevy_log::error!(
                target: "vmux",
                pid = std::process::id(),
                "FATAL: browser_host_create_browser returned {rc} (failure)"
            );
            std::process::exit(1);
        }

        if rc != 1 {
            bevy_log::info!(
                target: "vmux",
                pid = std::process::id(),
                "finish_pending: browser_host_create_browser returned {rc} (continuing; expected 1 on some CEF builds)"
            );
        }

        #[cfg(target_os = "macos")]
        {
            rt.macos_poll_attach_after_create = true;
        }

        self.cef_request_contexts.push(request_context);
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "finish_pending: browser create accepted (on_after_created next or completed)"
        );
        crate::lifecycle_trace::record_startup_milestone("finish_pending_create_browser_accepted");
        true
    }

    pub(crate) fn handle_resumed(
        &mut self,
        rt: &mut RuntimeState,
        event_loop: &ActiveEventLoop,
    ) {
        if rt.started {
            return;
        }
        rt.started = true;
        let startup_url = self.key_settings.startup_url.clone();
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "winit ApplicationHandler::resumed (single OSR window → {})",
            startup_url.as_str()
        );
        crate::lifecycle_trace::record_startup_milestone("winit_resumed_spawn_window");
        self.spawn_cef_browser_window(rt, event_loop, startup_url.as_str(), WINDOW_TITLE);
        #[cfg(target_os = "macos")]
        {
            // Match `examples/osr`: `browser_host_create_browser*` runs in `ApplicationHandler::resumed`
            // after wgpu + window (not on the following `run_winit` head before `pump_app_events`).
            if self.finish_next_pending_browser_if_any(rt) {
                rt.bump_cef_post_create_pumps(24);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            if self.finish_next_pending_browser_if_any(rt) {
                rt.bump_cef_post_create_pumps(24);
            }
        }
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "resumed: startup window ready"
        );
    }

    /// Call from the main pump after the CEF tick and after `pump_app_events`.
    pub(crate) fn handle_about_to_wait(
        &mut self,
        rt: &mut RuntimeState,
        shutdown: &ShutdownFlag,
    ) {
        self.apply_pending_titles();
        show_all_windows();

        if rt.quit_requested {
            rt.quit_started_at.get_or_insert(Instant::now());

            // Orphan `windows_store` rows (no matching `CefBrowserHandles` entry) block shutdown:
            // `before_close` may have missed `WindowId` from the foreign index, so the map never
            // shrank while handles were already cleared.
            let handles_empty = crate::browser::browser_cef_handles()
                .lock()
                .map(|g| g.is_empty())
                .unwrap_or(true);
            if handles_empty {
                if let Ok(mut ws) = self.cef_attach.windows_store.lock() {
                    let before = ws.len();
                    if before > 0 {
                        ws.retain(|_, entry| {
                            let bid = entry.browser.identifier();
                            crate::browser::browser_cef_handles()
                                .lock()
                                .map(|g| g.get(bid).is_some())
                                .unwrap_or(false)
                        });
                        let removed = before.saturating_sub(ws.len());
                        if removed > 0 {
                            bevy_log::info!(
                                target: "vmux",
                                pid = std::process::id(),
                                "quit_requested: dropped {removed} orphan windows_store entr(y/ies) (handles empty)"
                            );
                        }
                    }
                }
            }

            let ws_empty = self
                .cef_attach
                .windows_store
                .lock()
                .map(|m| m.is_empty())
                .unwrap_or(false);
            let fifo_empty = self
                .cef_attach
                .shell_fifo
                .lock()
                .map(|q| q.is_empty())
                .unwrap_or(false);
            let pending_empty = rt.pending_browser_hosts.is_empty();
            let orderly = ws_empty && fifo_empty && pending_empty;
            let force_deadline = rt
                .quit_started_at
                .is_some_and(|t| t.elapsed() >= FORCE_QUIT_AFTER);

            if orderly {
                crate::lifecycle_trace::record_runtime_event(
                    "handle_about_to_wait shutdown orderly ws_empty fifo_empty pending_empty",
                );
                shutdown
                    .0
                    .store(true, std::sync::atomic::Ordering::Release);
            } else if force_deadline {
                crate::lifecycle_trace::record_runtime_event(
                    "handle_about_to_wait shutdown FORCE_QUIT_AFTER (stalled CEF/OSR quit)",
                );
                bevy_log::warn!(
                    target: "vmux",
                    pid = std::process::id(),
                    "quit: forcing runner shutdown after {:?} — CEF close did not idle OSR maps (ws_empty={ws_empty} fifo_empty={fifo_empty} pending_empty={pending_empty})",
                    FORCE_QUIT_AFTER
                );
                shutdown
                    .0
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }

        let Ok(windows) = self.cef_attach.windows_store.lock() else {
            return;
        };
        for entry in windows.values() {
            entry.surface.window.request_redraw();
        }
        drop(windows);
        for p in &rt.pending_browser_hosts {
            p.surface.window.request_redraw();
        }
    }
}

#[cfg(test)]
mod staged_initial_navigation_url_tests {
    use super::OsrHostState;

    #[test]
    fn https_loads_directly_without_blank_history_staging() {
        let (u, d) = OsrHostState::staged_initial_navigation_url("https://www.google.com");
        assert_eq!(u, "https://www.google.com");
        assert!(d.is_none());
    }

    #[test]
    fn http_loads_directly() {
        let (u, d) = OsrHostState::staged_initial_navigation_url("http://example.com/");
        assert_eq!(u, "http://example.com/");
        assert!(d.is_none());
    }

    #[test]
    fn explicit_about_blank_no_deferred() {
        let (u, d) = OsrHostState::staged_initial_navigation_url("about:blank");
        assert_eq!(u, "about:blank");
        assert!(d.is_none());
    }

    #[test]
    fn whitespace_falls_back_to_blank_plus_default() {
        let (u, d) = OsrHostState::staged_initial_navigation_url("   ");
        assert_eq!(u, "about:blank");
        assert_eq!(d.as_deref(), Some(crate::settings::DEFAULT_STARTUP_URL));
    }
}
