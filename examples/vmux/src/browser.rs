pub mod events;
pub mod request_context;
pub mod backend;
pub mod active_browser;
pub mod browser_entity;
pub mod editable_focus;
pub mod handler_runtime;
pub mod shell_ops;
pub mod view_state;
pub mod event_loop;
pub mod renderer;
pub use crate::browser::active_browser::ActiveBrowserId;
pub use crate::browser::backend::cef::{CefPlugin, CefStartupState};
pub use crate::browser::event_loop::WinitPlugin;

use bevy_app::{App as BevyApp, Plugin, Update};
use bevy_ecs::schedule::IntoSystemConfigs;
use ::cef::ImplBrowser as _;
use ::cef::*;
use std::sync::{Arc, Mutex};
use winit::window::WindowId;
#[cfg(not(target_os = "macos"))]
use winit::window::CursorIcon;

use crate::browser::browser_entity::CefBrowserHandlesInner;
use crate::browser::handler_runtime::{BrowserCloseGuardsInner, BrowserLifecycleInner};
use crate::browser::backend::osr::hub::CefAttach;
pub struct BrowserPlugin;

impl BrowserPlugin {
    pub const fn new() -> Self {
        Self
    }
}

impl Plugin for BrowserPlugin {
    fn build(&self, app: &mut BevyApp) {
        app.init_resource::<backend::cef::threads::CefThreadContext>()
            .init_resource::<events::OsrAfterCreatedAttachQueue>()
            .add_event::<events::NavigateBrowserEvent>()
            .add_event::<events::ReloadBrowserEvent>()
            .add_event::<events::DelayedNavigationRepaintBrowserEvent>()
            .add_event::<events::ShowMainWindowBrowserEvent>()
            .add_event::<events::CloseAllBrowsersBrowserEvent>()
            .add_event::<events::LinkHintsShowBrowserEvent>()
            .add_event::<events::LinkHintsHideBrowserEvent>()
            .add_event::<events::LinkHintsFeedKeyDeferredBrowserEvent>()
            .add_event::<events::ArmWindowlessCloseBrowserEvent>()
            .add_event::<events::QuitCloseAllBrowsersBrowserEvent>()
            .add_event::<events::AddressChangedBrowserEvent>()
            .add_event::<events::TitleChangedBrowserEvent>()
            .add_event::<events::LoadingStateChangedBrowserEvent>()
            .add_event::<events::AfterCreatedBrowserCallbackEvent>()
            .add_event::<events::BeforeCloseBrowserCallbackEvent>()
            .add_event::<events::DoCloseBrowserCallbackEvent>()
            .add_event::<events::LoadErrorBrowserCallbackEvent>()
            .add_systems(
                Update,
                (
                    (
                        events::apply_navigate_browser_events_system,
                        events::apply_reload_browser_events_system,
                        events::apply_delayed_navigation_repaint_on_ui_events_system,
                        events::apply_show_main_window_on_ui_events_system,
                        events::apply_close_all_browsers_on_ui_events_system
                            .after(handler_runtime::apply_close_all_browsers_requests_system),
                        events::apply_link_hints_show_browser_events_system,
                        events::apply_link_hints_hide_browser_events_system,
                    ),
                    (
                        events::apply_link_hints_feed_key_deferred_browser_events_system,
                        events::apply_arm_windowless_close_browser_events_system,
                        events::apply_quit_close_all_browsers_browser_events_system
                            .after(crate::window::pending_window_events::apply_osr_host_window_dispatches_system)
                            .before(handler_runtime::apply_close_all_browsers_requests_system),
                        events::apply_address_changed_browser_events_system,
                        events::apply_title_changed_browser_events_system,
                        events::apply_loading_state_changed_browser_events_system,
                        (
                            events::enqueue_after_created_osr_attach_system,
                            events::apply_osr_browser_attach_system,
                        )
                            .chain(),
                        events::apply_before_close_browser_callback_events_system,
                        events::apply_do_close_browser_callback_events_system,
                        events::apply_load_error_browser_callback_events_system,
                    ),
                ),
            );
    }
}

/// After [`BrowserHandler::link_hints_feed_key`]: whether the hint session should stay armed in Rust.
/// `data-vmux-hints` on `<html>` holds the fixed label width (stringified integer); when a DOM read
/// lags after a key, Rust uses [`BrowserHandler::link_hints_feed_key`]'s `prior_typed_len` plus cached width.
/// Typed prefix is tracked in Rust (`VimiumState`) only.
#[derive(Debug, Clone)]
pub struct LinkHintsFeedOutcome {
    pub still_active: bool,
    /// Fixed code length for this page (from `data-vmux-hints` when the overlay is present; else best known).
    pub hint_label_width: u8,
}

impl Default for LinkHintsFeedOutcome {
    fn default() -> Self {
        Self {
            still_active: false,
            hint_label_width: 1,
        }
    }
}

pub struct BrowserHandler {}

pub(crate) fn browser_cef_attach() -> Option<CefAttach> {
    event_loop::foreign_browser_cef_attach()
}

pub(crate) fn browser_lifecycle() -> Arc<Mutex<BrowserLifecycleInner>> {
    event_loop::foreign_browser_lifecycle()
}

pub(crate) fn browser_cef_handles() -> Arc<Mutex<CefBrowserHandlesInner>> {
    event_loop::foreign_browser_cef_handles()
}

pub(crate) fn browser_close_guards() -> Arc<Mutex<BrowserCloseGuardsInner>> {
    event_loop::foreign_browser_close_guards()
}

pub(crate) fn init_browser_runtime_globals(
    cef_attach: Option<CefAttach>,
    cef_handles: Arc<Mutex<CefBrowserHandlesInner>>,
    lifecycle: Arc<Mutex<BrowserLifecycleInner>>,
    close_guards: Arc<Mutex<BrowserCloseGuardsInner>>,
) {
    event_loop::register_browser_runtime_for_foreign_callbacks(cef_handles, lifecycle, close_guards);
    event_loop::register_cef_attach_for_foreign_callbacks(cef_attach);
}

/// Resolve a CEF [`Browser`] by id for editable-focus probing (CEF UI thread and Bevy
/// [`crate::browser::editable_focus::dispatch_editable_focus_probe_requests_system`]).
pub(crate) fn resolve_browser_for_editable_probe(
    browser_id: i32,
) -> Option<Browser> {
    crate::browser::events::cef_browser_by_id(browser_id)
}

impl BrowserHandler {
    fn send_link_hint_feed_to_main(
        window_id: WindowId,
        browser_id: i32,
        ch: char,
        prior_typed_len: usize,
        still_active: bool,
        hint_label_width: u8,
    ) {
        event_loop::send_user_event(event_loop::UserEvent::App(
            event_loop::AppEvent::LinkHintFeed(event_loop::LinkHintFeedEvent {
                window_id,
                browser_id,
                ch,
                prior_typed_len,
                still_active,
                hint_label_width,
            }),
        ));
    }

    #[cfg(not(target_os = "macos"))]
    fn cursor_from_cef(ty: CursorType) -> CursorIcon {
        match ty {
            CursorType::POINTER => CursorIcon::Default,
            CursorType::IBEAM | CursorType::VERTICALTEXT => CursorIcon::Text,
            CursorType::HAND => CursorIcon::Pointer,
            CursorType::CROSS => CursorIcon::Crosshair,
            CursorType::WAIT => CursorIcon::Wait,
            CursorType::HELP => CursorIcon::Help,
            CursorType::PROGRESS => CursorIcon::Progress,
            CursorType::MOVE => CursorIcon::Move,
            CursorType::EASTRESIZE => CursorIcon::EResize,
            CursorType::WESTRESIZE => CursorIcon::WResize,
            CursorType::NORTHRESIZE => CursorIcon::NResize,
            CursorType::SOUTHRESIZE => CursorIcon::SResize,
            CursorType::NORTHEASTRESIZE => CursorIcon::NeResize,
            CursorType::NORTHWESTRESIZE => CursorIcon::NwResize,
            CursorType::SOUTHEASTRESIZE => CursorIcon::SeResize,
            CursorType::SOUTHWESTRESIZE => CursorIcon::SwResize,
            CursorType::EASTWESTRESIZE => CursorIcon::EwResize,
            CursorType::NORTHSOUTHRESIZE => CursorIcon::NsResize,
            CursorType::NORTHEASTSOUTHWESTRESIZE => CursorIcon::NeswResize,
            CursorType::NORTHWESTSOUTHEASTRESIZE => CursorIcon::NwseResize,
            CursorType::COLUMNRESIZE => CursorIcon::ColResize,
            CursorType::ROWRESIZE => CursorIcon::RowResize,
            CursorType::CONTEXTMENU => CursorIcon::ContextMenu,
            CursorType::NODROP | CursorType::NOTALLOWED => CursorIcon::NotAllowed,
            CursorType::COPY => CursorIcon::Copy,
            CursorType::ALIAS => CursorIcon::Alias,
            CursorType::CELL => CursorIcon::Cell,
            CursorType::GRAB => CursorIcon::Grab,
            CursorType::GRABBING => CursorIcon::Grabbing,
            CursorType::ZOOMIN => CursorIcon::ZoomIn,
            CursorType::ZOOMOUT => CursorIcon::ZoomOut,
            CursorType::NONE => CursorIcon::Default,
            _ => CursorIcon::Default,
        }
    }

    /// Navigate a **specific** CEF browser (e.g. the window that received **Shift+H** / **Shift+L**).
    pub fn navigate_cef_browser(browser_id: i32, go_forward: bool) {
        let tid = backend::cef::threads::cef_ui_thread_id();
        if !backend::cef::threads::on_cef_ui_thread() {
            let mut task = NavigateCefBrowser::new(browser_id, go_forward);
            if post_task(tid, Some(&mut task)) == 0 {
                bevy_log::warn!(
                    target: "vmux",
                    pid = std::process::id(),
                    "navigate_cef_browser: post_task to UI thread failed"
                );
            }
            return;
        }

        event_loop::send_user_event(event_loop::UserEvent::Cef(
            event_loop::CefEvent::NavigateBrowser(crate::browser::events::NavigateBrowserEvent {
                browser_id,
                go_forward,
            }),
        ));
    }

    /// Show Vimium-style link hints in the given CEF browser (UI thread; pumps from winit if needed).
    pub fn link_hints_show(browser_id: i32) {
        let tid = backend::cef::threads::cef_ui_thread_id();
        if !backend::cef::threads::on_cef_ui_thread() {
            let mut task = LinkHintsRun::new(browser_id, true);
            if post_task(tid, Some(&mut task)) == 0 {
                bevy_log::warn!(
                    target: "vmux",
                    pid = std::process::id(),
                    "link_hints_show: post_task to UI thread failed"
                );
            }
            return;
        }
        event_loop::send_user_event(event_loop::UserEvent::Vimium(
            event_loop::VimiumEvent::LinkHintsShowBrowser(
                crate::browser::events::LinkHintsShowBrowserEvent { browser_id },
            ),
        ));
    }

    /// Remove link-hint overlay / listeners (same threading as `link_hints_show`).
    pub fn link_hints_hide(browser_id: i32) {
        let tid = backend::cef::threads::cef_ui_thread_id();
        if !backend::cef::threads::on_cef_ui_thread() {
            let mut task = LinkHintsRun::new(browser_id, false);
            if post_task(tid, Some(&mut task)) == 0 {
                bevy_log::warn!(
                    target: "vmux",
                    pid = std::process::id(),
                    "link_hints_hide: post_task to UI thread failed"
                );
            }
            return;
        }
        event_loop::send_user_event(event_loop::UserEvent::Vimium(
            event_loop::VimiumEvent::LinkHintsHideBrowser(
                crate::browser::events::LinkHintsHideBrowserEvent { browser_id },
            ),
        ));
    }

    /// Post link-hint feed to the CEF UI thread; completion is delivered via winit user events.
    pub fn link_hints_feed_key_deferred(browser_id: i32, ch: char, prior_typed_len: usize) {
        let tid = backend::cef::threads::cef_ui_thread_id();
        if backend::cef::threads::on_cef_ui_thread() {
            let outcome = crate::browser::events::link_hints_feed_key_on_ui(
                browser_id,
                ch,
                prior_typed_len,
            );
            if let Some(wid) =
                crate::browser::backend::osr::foreign_index::window_id_for_browser(
                    crate::browser::event_loop::foreign_osr_index().as_ref(),
                    browser_id,
                )
            {
                Self::send_link_hint_feed_to_main(
                    wid,
                    browser_id,
                    ch,
                    prior_typed_len,
                    outcome.still_active,
                    outcome.hint_label_width,
                );
            }
            return;
        }
        let mut task = LinkHintsFeedTask::new(browser_id, ch, prior_typed_len);
        if post_task(tid, Some(&mut task)) == 0 {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "link_hints_feed_key_deferred: post_task to UI thread failed"
            );
        }
    }

    /// Feed one hint letter (UI thread only). From the winit thread use [`Self::link_hints_feed_key_deferred`].
    ///
    /// **Exception:** On the CEF UI thread this runs synchronously (JS + DOM + pump) so the next
    /// Vimium key sees updated hint state; all other [`BrowserHandler`] entrypoints only enqueue
    /// user events for Bevy `apply_*` systems.
    pub fn link_hints_feed_key(
        browser_id: i32,
        ch: char,
        prior_typed_len: usize,
    ) -> LinkHintsFeedOutcome {
        if backend::cef::threads::on_cef_ui_thread() {
            return crate::browser::events::link_hints_feed_key_on_ui(
                browser_id,
                ch,
                prior_typed_len,
            );
        }
        Self::link_hints_feed_key_deferred(browser_id, ch, prior_typed_len);
        LinkHintsFeedOutcome {
            still_active: true,
            ..Default::default()
        }
    }

    /// Call from the winit `CloseRequested` path **before** `try_close_browser` so `do_close` can
    /// return false (allow CEF to destroy this browser) without treating the request as spurious.
    pub fn arm_windowless_close_from_winit() {
        event_loop::send_user_event(event_loop::UserEvent::App(
            event_loop::AppEvent::ArmWindowlessCloseBrowser(
                crate::browser::events::ArmWindowlessCloseBrowserEvent,
            ),
        ));
    }

    pub fn show_main_window() {
        let tid = backend::cef::threads::cef_ui_thread_id();
        if !backend::cef::threads::on_cef_ui_thread() {
            let mut task = ShowMainWindow::new();
            post_task(tid, Some(&mut task));
            return;
        }
        event_loop::send_user_event(event_loop::UserEvent::App(
            event_loop::AppEvent::ShowMainWindowBrowser(
                crate::browser::events::ShowMainWindowBrowserEvent,
            ),
        ));
    }

    /// Request close on every tracked browser. Does **not** hold the handler mutex while
    /// calling `close_browser`: CEF may synchronously invoke `LifeSpanHandler::do_close`,
    /// which locks the same mutex (deadlock if we kept the lock — e.g. Cmd+Q on macOS).
    pub fn close_all_browsers(force_close: bool) {
        let tid = backend::cef::threads::cef_ui_thread_id();
        if !backend::cef::threads::on_cef_ui_thread() {
            let mut task = CloseAllBrowsers::new(force_close);
            post_task(tid, Some(&mut task));
            return;
        }
        crate::browser::events::close_all_browsers_on_ui_thread(force_close);
    }

    pub fn is_closing(&self) -> bool {
        browser_lifecycle()
            .lock()
            .map(|g| g.is_closing)
            .unwrap_or(false)
    }
}

wrap_client! {
    pub struct BrowserClient {
        render: RenderHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            #[cfg(target_os = "macos")]
            {
                // `examples/osr` client is render-only; vmux display/load run extra UI-thread work during
                // first frames. Keep address/title proofs on mac via this minimal handler (no cursor path).
                Some(BrowserDisplayHandlerMacOsr::new())
            }
            #[cfg(not(target_os = "macos"))]
            {
                Some(BrowserDisplayHandler::new())
            }
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            #[cfg(target_os = "macos")]
            {
                // No Rust `LifeSpanHandler` — avoids traps unwinding from `on_after_created`. Attach:
                // [`OsrHostState::macos_poll_cef_browser_attach`] after async create + pumps.
                None
            }
            #[cfg(not(target_os = "macos"))]
            {
                Some(BrowserLifeSpanHandler::new())
            }
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(BrowserLoadHandler::new())
        }
    }
}

#[cfg(not(target_os = "macos"))]
wrap_display_handler! {
    struct BrowserDisplayHandler {}

    impl DisplayHandler {
        fn on_address_change(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            let Some(browser_id) = browser.as_ref().map(|b| b.identifier()) else {
                return;
            };
            let url_str = url.map(CefString::to_string);
            if let Some(ref s) = url_str {
                if s.contains("google.com") {
                    bevy_log::info!(
                        target: "vmux",
                        pid = std::process::id(),
                        "proof: cef_navigated_url_has_google browser_id={browser_id} url={s}"
                    );
                }
            }
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::AddressChangedBrowser(
                    crate::browser::events::AddressChangedBrowserEvent {
                        browser_id,
                        url: url_str,
                    },
                ),
            ));
        }

        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let Some(browser_id) = browser.as_ref().map(|b| b.identifier()) else {
                return;
            };
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::TitleChangedBrowser(
                    crate::browser::events::TitleChangedBrowserEvent {
                        browser_id,
                        title: title.map(CefString::to_string),
                    },
                ),
            ));
        }

        fn on_cursor_change(
            &self,
            browser: Option<&mut Browser>,
            _cursor: *mut u8,
            type_: CursorType,
            _custom_cursor_info: Option<&CursorInfo>,
        ) -> i32 {
            let Some(browser) = browser else {
                return 0;
            };
            let attach = browser_cef_attach();
            let Some(attach) = attach else {
                return 0;
            };
            let bid = browser.identifier();
            let Some(wid) = crate::browser::backend::osr::foreign_index::window_id_for_browser(
                crate::browser::event_loop::foreign_osr_index().as_ref(),
                bid,
            )
            else {
                return 0;
            };
            let Ok(windows) = attach.windows_store.lock() else {
                return 0;
            };
            let Some(entry) = windows.get(&wid) else {
                return 0;
            };
            entry
                .surface
                .window
                .set_cursor(BrowserHandler::cursor_from_cef(type_));
            1
        }
    }
}

// macOS: address/title only — no `on_cursor_change` (winit + windows_store from CEF thread during early frames).
#[cfg(target_os = "macos")]
wrap_display_handler! {
    struct BrowserDisplayHandlerMacOsr {}

    impl DisplayHandler {
        fn on_address_change(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            let Some(browser_id) = browser.as_ref().map(|b| b.identifier()) else {
                return;
            };
            let url_str = url.map(CefString::to_string);
            if let Some(ref s) = url_str {
                if s.contains("google.com") {
                    bevy_log::info!(
                        target: "vmux",
                        pid = std::process::id(),
                        "proof: cef_navigated_url_has_google browser_id={browser_id} url={s}"
                    );
                }
            }
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::AddressChangedBrowser(
                    crate::browser::events::AddressChangedBrowserEvent {
                        browser_id,
                        url: url_str,
                    },
                ),
            ));
        }

        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let Some(browser_id) = browser.as_ref().map(|b| b.identifier()) else {
                return;
            };
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::TitleChangedBrowser(
                    crate::browser::events::TitleChangedBrowserEvent {
                        browser_id,
                        title: title.map(CefString::to_string),
                    },
                ),
            ));
        }
    }
}

#[cfg(not(target_os = "macos"))]
wrap_life_span_handler! {
    struct BrowserLifeSpanHandler {}

    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            crate::lifecycle_trace::record_startup_milestone("cef_on_after_created_enter");
            crate::lifecycle_trace::trace("on_after_created_enter");
            let Some(browser) = browser else {
                return;
            };
            let browser_id = browser.identifier();
            let ev = crate::browser::events::AfterCreatedBrowserCallbackEvent { browser_id };
            let mut task = AfterCreatedSendToWinitTask::new(browser_id);
            if post_task(ThreadId::UI, Some(&mut task)) == 0 {
                crate::lifecycle_trace::record_startup_milestone(
                    "cef_on_after_created_post_task_failed_sync_fallback",
                );
                crate::lifecycle_trace::trace("post_task_fail_fallback_sync");
                bevy_log::warn!(
                    target: "vmux",
                    pid = std::process::id(),
                    "LifeSpan: on_after_created: post_task failed — sync send_user_event fallback"
                );
                event_loop::send_user_event(event_loop::UserEvent::Cef(
                    event_loop::CefEvent::AfterCreatedBrowserCallback(ev),
                ));
            } else {
                crate::lifecycle_trace::record_startup_milestone("cef_on_after_created_post_task_ok");
                crate::lifecycle_trace::trace("post_task_ok");
                bevy_log::info!(
                    target: "vmux",
                    pid = std::process::id(),
                    "LifeSpan: on_after_created: deferred AfterCreated notify (post_task) browser_id={browser_id}"
                );
            }
        }

        fn do_close(&self, browser: Option<&mut Browser>) -> i32 {
            let _ = browser;
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::DoCloseBrowserCallback(
                    crate::browser::events::DoCloseBrowserCallbackEvent,
                ),
            ));
            crate::browser::events::do_close_from_event()
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::BeforeCloseBrowserCallback(
                    crate::browser::events::BeforeCloseBrowserCallbackEvent {
                        browser: browser.cloned(),
                    },
                ),
            ));
        }
    }
}

wrap_load_handler! {
    struct BrowserLoadHandler {}

    impl LoadHandler {
        fn on_load_error(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::LoadErrorBrowserCallback(
                    crate::browser::events::LoadErrorBrowserCallbackEvent {
                        browser: browser.cloned(),
                        frame: frame.cloned(),
                        error_code,
                        error_text: error_text.map(CefString::to_string),
                        failed_url: failed_url.map(CefString::to_string),
                    },
                ),
            ));
        }

        fn on_loading_state_change(
            &self,
            browser: Option<&mut Browser>,
            is_loading: std::os::raw::c_int,
            _can_go_back: std::os::raw::c_int,
            _can_go_forward: std::os::raw::c_int,
        ) {
            let Some(browser_id) = browser.as_ref().map(|b| b.identifier()) else {
                return;
            };
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::LoadingStateChangedBrowser(
                    crate::browser::events::LoadingStateChangedBrowserEvent {
                        browser_id,
                        is_loading,
                    },
                ),
            ));
        }
    }
}

wrap_task! {
    struct NavigateCefBrowser {
        browser_id: i32,
        go_forward: bool,
    }

    impl Task {
        fn execute(&self) {
            debug_assert!(backend::cef::threads::on_cef_ui_thread());
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::NavigateBrowser(crate::browser::events::NavigateBrowserEvent {
                    browser_id: self.browser_id,
                    go_forward: self.go_forward,
                }),
            ));
        }
    }
}

wrap_task! {
    struct LinkHintsRun {
        browser_id: i32,
        show: bool,
    }

    impl Task {
        fn execute(&self) {
            debug_assert!(backend::cef::threads::on_cef_ui_thread());
            if self.show {
                event_loop::send_user_event(event_loop::UserEvent::Vimium(
                    event_loop::VimiumEvent::LinkHintsShowBrowser(
                        crate::browser::events::LinkHintsShowBrowserEvent {
                            browser_id: self.browser_id,
                        },
                    ),
                ));
            } else {
                event_loop::send_user_event(event_loop::UserEvent::Vimium(
                    event_loop::VimiumEvent::LinkHintsHideBrowser(
                        crate::browser::events::LinkHintsHideBrowserEvent {
                            browser_id: self.browser_id,
                        },
                    ),
                ));
            }
        }
    }
}

wrap_task! {
    struct LinkHintsFeedTask {
        browser_id: i32,
        ch: char,
        prior_typed_len: usize,
    }

    impl Task {
        fn execute(&self) {
            debug_assert!(backend::cef::threads::on_cef_ui_thread());
            event_loop::send_user_event(event_loop::UserEvent::Vimium(
                event_loop::VimiumEvent::LinkHintsFeedKeyDeferredBrowser(
                    crate::browser::events::LinkHintsFeedKeyDeferredBrowserEvent {
                        browser_id: self.browser_id,
                        ch: self.ch,
                        prior_typed_len: self.prior_typed_len,
                    },
                ),
            ));
        }
    }
}

wrap_task! {
    struct DelayedNavigationRepaint {
        browser_id: i32,
    }

    impl Task {
        fn execute(&self) {
            debug_assert!(backend::cef::threads::on_cef_ui_thread());
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::DelayedNavigationRepaintBrowser(
                    crate::browser::events::DelayedNavigationRepaintBrowserEvent {
                        browser_id: self.browser_id,
                    },
                ),
            ));
        }
    }
}

wrap_task! {
    struct ShowMainWindow {}

    impl Task {
        fn execute(&self) {
            debug_assert!(backend::cef::threads::on_cef_ui_thread());
            event_loop::send_user_event(event_loop::UserEvent::App(
                event_loop::AppEvent::ShowMainWindowBrowser(
                    crate::browser::events::ShowMainWindowBrowserEvent,
                ),
            ));
        }
    }
}

wrap_task! {
    struct CloseAllBrowsers {
        force_close: bool,
    }

    impl Task {
        fn execute(&self) {
            debug_assert!(backend::cef::threads::on_cef_ui_thread());
            crate::browser::events::close_all_browsers_on_ui_thread(self.force_close);
        }
    }
}

#[cfg(not(target_os = "macos"))]
wrap_task! {
    struct AfterCreatedSendToWinitTask {
        browser_id: i32,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            crate::lifecycle_trace::record_startup_milestone("after_created_task_before_send_user_event");
            crate::lifecycle_trace::trace("after_created_task_execute_before_send_user_event");
            bevy_log::info!(
                target: "vmux",
                pid = std::process::id(),
                "AfterCreatedSendToWinitTask: execute browser_id={}",
                self.browser_id
            );
            event_loop::send_user_event(event_loop::UserEvent::Cef(
                event_loop::CefEvent::AfterCreatedBrowserCallback(
                    crate::browser::events::AfterCreatedBrowserCallbackEvent {
                        browser_id: self.browser_id,
                    },
                ),
            ));
        }
    }
}

