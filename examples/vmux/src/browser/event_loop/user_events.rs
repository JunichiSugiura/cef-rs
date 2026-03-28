//! Winit user-event taxonomy and pump-related [`Resource`]s ([`UserEvent`], [`RuntimeState`], …).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use bevy_ecs::event::EventReader;
use bevy_ecs::prelude::{Event, ResMut, Resource};
use cef::sys::cef_event_flags_t;
use winit::event::KeyEvent;
use winit::keyboard::ModifiersState;
use winit::window::WindowId;

use crate::browser::backend::osr::hub::PendingCefWindow;
use crate::browser::browser_entity::{BrowserDespawnEvent, BrowserSpawnEvent};
use crate::browser::events;

/// Browsers whose document navigated while link hints may be armed; drained at window-event dispatch.
#[derive(Resource, Default)]
pub struct LinkHintsNavPending(pub Vec<i32>);

pub fn ingest_link_hints_nav_invalidate_events_system(
    mut events: EventReader<events::LinkHintsNavInvalidateBrowser>,
    mut pending: ResMut<LinkHintsNavPending>,
) {
    for ev in events.read() {
        pending.0.push(ev.browser_id);
    }
}

/// Winit + CEF pump scratch state (modifiers, pending hosts, find-mode keys, …).
#[derive(Resource)]
pub struct RuntimeState {
    pub primary_mouse_down: bool,
    pub last_cursor_pos: (i32, i32),
    pub mods_winit: ModifiersState,
    pub mods: cef_event_flags_t,
    pub wheel_residual: (f64, f64),
    pub started: bool,
    pub quit_requested: bool,
    /// Set when [`Self::quit_requested`] becomes true; used to force [`ShutdownFlag`] if CEF/OSR
    /// teardown stalls (non-empty `windows_store` after `close_browser`).
    pub quit_started_at: Option<Instant>,
    #[cfg(target_os = "macos")]
    pub macos_shell_refocus_ticks: u8,
    #[cfg(target_os = "macos")]
    pub macos_shell_refocus_window: Option<WindowId>,
    pub cef_post_create_pumps_remaining: u8,
    #[cfg(target_os = "macos")]
    pub macos_poll_attach_after_create: bool,
    pub pending_browser_hosts: VecDeque<PendingCefWindow>,
    pub pending_find_mode_keys: VecDeque<(WindowId, KeyEvent)>,
    /// [`WindowEvent::RedrawRequested`](winit::event::WindowEvent::RedrawRequested) targets drained by [`crate::browser::renderer::vmux_render::runner::flush_osr_redraw_for_window`] after OSR dispatches.
    pub vmux_osr_redraw_queue: VecDeque<WindowId>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            primary_mouse_down: false,
            last_cursor_pos: (0, 0),
            mods_winit: ModifiersState::default(),
            mods: cef_event_flags_t::EVENTFLAG_NONE,
            wheel_residual: (0.0, 0.0),
            started: false,
            quit_requested: false,
            quit_started_at: None,
            #[cfg(target_os = "macos")]
            macos_shell_refocus_ticks: 0,
            #[cfg(target_os = "macos")]
            macos_shell_refocus_window: None,
            cef_post_create_pumps_remaining: 0,
            #[cfg(target_os = "macos")]
            macos_poll_attach_after_create: false,
            pending_browser_hosts: VecDeque::new(),
            pending_find_mode_keys: VecDeque::new(),
            vmux_osr_redraw_queue: VecDeque::new(),
        }
    }
}

impl RuntimeState {
    /// After `OsrHostState::finish_next_pending_browser_if_any` returns true, extend the multi-frame settle budget.
    pub fn bump_cef_post_create_pumps(&mut self, extra: u8) {
        self.cef_post_create_pumps_remaining = self
            .cef_post_create_pumps_remaining
            .saturating_add(extra)
            .min(64);
    }

    /// Drain up to `cap_per_frame` toward [`Self::cef_post_create_pumps_remaining`]; used by the main loop.
    pub fn drain_cef_post_create_pumps(&mut self, cap_per_frame: u8) -> u32 {
        let take = self.cef_post_create_pumps_remaining.min(cap_per_frame);
        self.cef_post_create_pumps_remaining -= take;
        take as u32
    }
}

#[derive(Event, Debug, Clone)]
pub struct VimiumKeyReplayEvent {
    pub window_id: WindowId,
    pub event: KeyEvent,
}

#[derive(Event, Debug, Clone)]
pub struct LinkHintFeedEvent {
    pub window_id: WindowId,
    pub browser_id: i32,
    pub ch: char,
    pub prior_typed_len: usize,
    pub still_active: bool,
    pub hint_label_width: u8,
}

/// Top-level payload for winit's user-event slot: app orchestration, CEF bridge, or Vimium.
#[derive(Debug, Clone)]
pub enum UserEvent {
    App(AppEvent),
    Cef(CefEvent),
    Vimium(VimiumEvent),
}

/// Shell / lifecycle / window orchestration (not CEF callback-shaped, not Vimium-only).
#[derive(Debug, Clone)]
pub enum AppEvent {
    LinkHintFeed(LinkHintFeedEvent),
    /// CEF browser attached to a winit shell — spawn ECS entity on the main thread.
    BrowserSpawn(BrowserSpawnEvent),
    /// CEF browser is closing — despawn ECS entity.
    BrowserDespawn(BrowserDespawnEvent),
    /// CEF [`BrowserProcessHandler::on_schedule_message_pump_work`]: wake the loop and merge a pump deadline.
    ScheduleCefPump { deadline: Instant },
    /// AppKit terminate / user quit — becomes [`bevy_app::AppExit`] in the winit user-event handler.
    RequestQuit,
    /// Last browser closed — request runner teardown (mirrors shell idle shutdown).
    ShutdownRequested,
    ShowMainWindowBrowser(events::ShowMainWindowBrowserEvent),
    CloseAllBrowsersBrowser(events::CloseAllBrowsersBrowserEvent),
    ArmWindowlessCloseBrowser(events::ArmWindowlessCloseBrowserEvent),
    QuitCloseAllBrowsersBrowser(events::QuitCloseAllBrowsersBrowserEvent),
}

/// CEF integration: handlers, navigation, and UI-thread mirrors to the shell queue.
#[derive(Debug, Clone)]
pub enum CefEvent {
    /// CEF/UI-thread hint update mirrored into ECS shell-op queue.
    SetEditableFocusHint { browser_id: i32, editable: bool },
    /// CEF/UI-thread browser close mirrored into ECS shell-op queue.
    RemoveBrowserEntries { browser_id: i32 },
    NavigateBrowser(events::NavigateBrowserEvent),
    ReloadBrowser(events::ReloadBrowserEvent),
    DelayedNavigationRepaintBrowser(events::DelayedNavigationRepaintBrowserEvent),
    AddressChangedBrowser(events::AddressChangedBrowserEvent),
    TitleChangedBrowser(events::TitleChangedBrowserEvent),
    LoadingStateChangedBrowser(events::LoadingStateChangedBrowserEvent),
    AfterCreatedBrowserCallback(events::AfterCreatedBrowserCallbackEvent),
    BeforeCloseBrowserCallback(events::BeforeCloseBrowserCallbackEvent),
    DoCloseBrowserCallback(events::DoCloseBrowserCallbackEvent),
    LoadErrorBrowserCallback(events::LoadErrorBrowserCallbackEvent),
}

/// Vimium-style keying and link hints.
#[derive(Debug, Clone)]
pub enum VimiumEvent {
    VimiumKeyReplay(VimiumKeyReplayEvent),
    LinkHintsShowBrowser(events::LinkHintsShowBrowserEvent),
    LinkHintsHideBrowser(events::LinkHintsHideBrowserEvent),
    LinkHintsFeedKeyDeferredBrowser(events::LinkHintsFeedKeyDeferredBrowserEvent),
}

/// Back-compat alias for the winit user-event payload (same as [`UserEvent`]).
pub type AppUserEvent = UserEvent;

/// When the winit runner may exit: last browser is gone and CEF teardown is safe.
/// Unix signals use [`SignalQuitFlag`] and `request_quit()` instead of flipping this flag directly.
#[derive(Resource, Clone)]
pub struct ShutdownFlag(pub Arc<AtomicBool>);

/// Set by `SIGTERM` / `SIGINT` via `signal_hook`; drained each frame into `request_quit()` so shutdown
/// follows the same path as AppKit Quit (close browsers, then set [`ShutdownFlag`]).
#[derive(Resource, Clone)]
pub struct SignalQuitFlag(pub Arc<AtomicBool>);

/// Next time the external CEF message pump should run (main thread only; updated from winit user events).
#[derive(Resource)]
pub struct CefPumpDeadline(pub Option<Instant>);

impl Default for CefPumpDeadline {
    fn default() -> Self {
        Self(None)
    }
}

impl CefPumpDeadline {
    pub fn merge(&mut self, deadline: Instant) {
        self.0 = Some(match self.0 {
            Some(existing) => existing.min(deadline),
            None => deadline,
        });
    }

    pub fn next_wait_timeout(&self, default_idle: Duration) -> Duration {
        let Some(deadline) = self.0 else {
            return default_idle;
        };
        deadline.saturating_duration_since(Instant::now()).min(default_idle)
    }

    /// Returns `true` if a scheduled pump was due and clears the deadline.
    pub fn take_if_due(&mut self) -> bool {
        let Some(deadline) = self.0 else {
            return false;
        };
        if deadline <= Instant::now() {
            self.0 = None;
            return true;
        }
        false
    }
}
