//! Bevy [`Event`]s and systems for deferred browser work.
//!
//! Producers push into a [`Vec`] during winit / vimium dispatch, then flush with [`EventWriter`];
//! these systems run on `Update` after those producer systems.

use bevy_ecs::event::{EventReader, EventWriter};
use bevy_ecs::prelude::{Event, Res, ResMut, Resource};
use std::collections::VecDeque;
use std::fmt;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::Ordering;
use cef::rc::Rc;
use cef::{
    Browser, CefString, CefStringUtf16, CefStringUtf8, Domdocument, Domvisitor, Errorcode, Frame,
    ImplBrowser, ImplBrowserHost, ImplDomdocument, ImplDomnode, ImplDomvisitor, ImplFrame,
    ImplNavigationEntry, ImplNavigationEntryVisitor, ImplTask, ImplView, ImplWindow, NavigationEntry,
    NavigationEntryVisitor, Task, ThreadId, WrapDomvisitor, WrapNavigationEntryVisitor, WrapTask,
    base64_encode, browser_host_get_browser_by_identifier, browser_view_get_for_browser,
    currently_on, post_task, uriencode, wrap_domvisitor, wrap_navigation_entry_visitor, wrap_task,
};
use winit::window::WindowId;

use crate::browser::{
    LinkHintsFeedOutcome, browser_cef_handles, browser_close_guards, browser_lifecycle,
    browser_cef_attach,
};
use crate::browser::backend::cef::bootstrap::ForeignOsrIndexResource;
use crate::browser::backend::osr::foreign_index::{self, ForeignOsrIndex};
use crate::browser::handler_runtime::RequestCloseAllBrowsersEvent;

#[derive(Event, Debug, Clone, Copy)]
pub struct NavigateBrowserEvent {
    pub browser_id: i32,
    pub go_forward: bool,
}

#[derive(Event, Debug, Clone, Copy)]
pub struct ReloadBrowserEvent {
    pub browser_id: i32,
}

#[derive(Event, Debug, Clone, Copy)]
pub struct DelayedNavigationRepaintBrowserEvent {
    pub browser_id: i32,
}

#[derive(Event, Debug, Clone, Copy, Default)]
pub struct ShowMainWindowBrowserEvent;

#[derive(Event, Debug, Clone, Copy)]
pub struct CloseAllBrowsersBrowserEvent {
    pub force_close: bool,
}

#[derive(Event, Debug, Clone, Copy)]
pub struct LinkHintsShowBrowserEvent {
    pub browser_id: i32,
}

#[derive(Event, Debug, Clone, Copy)]
pub struct LinkHintsHideBrowserEvent {
    pub browser_id: i32,
}

#[derive(Event, Debug, Clone, Copy)]
pub struct LinkHintsFeedKeyDeferredBrowserEvent {
    pub browser_id: i32,
    pub ch: char,
    pub prior_typed_len: usize,
}

#[derive(Event, Debug, Clone, Copy)]
pub struct ArmWindowlessCloseBrowserEvent;

#[derive(Event, Debug, Clone, Copy)]
pub struct QuitCloseAllBrowsersBrowserEvent;

#[derive(Event, Debug, Clone)]
pub struct AddressChangedBrowserEvent {
    pub browser_id: i32,
    pub url: Option<String>,
}

#[derive(Event, Debug, Clone)]
pub struct TitleChangedBrowserEvent {
    pub browser_id: i32,
    pub title: Option<String>,
}

#[derive(Event, Debug, Clone, Copy)]
pub struct LoadingStateChangedBrowserEvent {
    pub browser_id: i32,
    pub is_loading: i32,
}

/// Document navigated while link hints may be armed; drained into [`crate::browser::event_loop::LinkHintsNavPending`].
#[derive(Event, Debug, Clone, Copy)]
pub struct LinkHintsNavInvalidateBrowser {
    pub browser_id: i32,
}

/// Popped OSR shell + browser, queued between
/// [`enqueue_after_created_osr_attach_system`] and [`apply_osr_browser_attach_system`].
/// (`PendingCefWindow` is not `Clone`, so we use a resource queue instead of a Bevy `Event`.)
pub(crate) struct OsrAfterCreatedWork {
    pub browser_id: i32,
    pub browser: Browser,
    pub shell: crate::browser::backend::osr::hub::PendingCefWindow,
}

#[derive(Resource, Default)]
pub(crate) struct OsrAfterCreatedAttachQueue(pub(crate) Mutex<VecDeque<OsrAfterCreatedWork>>);

/// CEF [`LifeSpanHandler::on_after_created`] notifies winit with **`browser_id` only** — no
/// [`Browser::clone`] in the callback (macOS + CEF 146 stability). Bevy resolves the handle via
/// [`cef::browser_host_get_browser_by_identifier`].
#[derive(Event, Clone, Copy)]
pub struct AfterCreatedBrowserCallbackEvent {
    pub browser_id: i32,
}

#[derive(Event, Clone)]
pub struct BeforeCloseBrowserCallbackEvent {
    pub browser: Option<Browser>,
}

#[derive(Event, Debug, Clone, Copy, Default)]
pub struct DoCloseBrowserCallbackEvent;

#[derive(Event, Clone)]
pub struct LoadErrorBrowserCallbackEvent {
    pub browser: Option<Browser>,
    pub frame: Option<Frame>,
    pub error_code: Errorcode,
    pub error_text: Option<String>,
    pub failed_url: Option<String>,
}

impl fmt::Debug for AfterCreatedBrowserCallbackEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AfterCreatedBrowserCallbackEvent")
            .field("browser_id", &self.browser_id)
            .finish()
    }
}

impl fmt::Debug for BeforeCloseBrowserCallbackEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BeforeCloseBrowserCallbackEvent")
            .field("browser", &self.browser.as_ref().map(|b| b.identifier()))
            .finish()
    }
}

impl fmt::Debug for LoadErrorBrowserCallbackEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadErrorBrowserCallbackEvent")
            .field("browser", &self.browser.as_ref().map(|b| b.identifier()))
            .field("has_frame", &self.frame.is_some())
            .field(
                "error_code",
                &((cef::sys::cef_errorcode_t::from(self.error_code)) as i32),
            )
            .field("error_text", &self.error_text)
            .field("failed_url", &self.failed_url)
            .finish()
    }
}

const VMUX_LINK_HINTS_JS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/link_hints.js"
));

fn visible_navigation_url(browser: &Browser) -> Option<String> {
    let host = browser.host()?;
    let e = host.visible_navigation_entry()?;
    if e.is_valid() == 0 {
        return None;
    }
    let u = e.url();
    Some(CefString::from(&u).to_string())
}

wrap_navigation_entry_visitor! {
    struct NavEntriesCollector {
        entries: Arc<Mutex<Vec<(String, i32, bool)>>>,
    }

    impl NavigationEntryVisitor {
        fn visit(
            &self,
            entry: Option<&mut NavigationEntry>,
            current: std::os::raw::c_int,
            index: std::os::raw::c_int,
            _total: std::os::raw::c_int,
        ) -> std::os::raw::c_int {
            let Some(entry) = entry else {
                return 0;
            };
            if entry.is_valid() == 0 {
                return 0;
            }
            let u = entry.url();
            let url = CefString::from(&u).to_string();
            if let Ok(mut rows) = self.entries.lock() {
                rows.push((url, index, current != 0));
            }
            0
        }
    }
}

fn history_url_adjacent(browser: &Browser, back: bool) -> Option<String> {
    let host = browser.host()?;
    let visible = visible_navigation_url(browser);
    let entries = Arc::new(Mutex::new(Vec::<(String, i32, bool)>::new()));
    let mut visitor = NavEntriesCollector::new(Arc::clone(&entries));
    host.navigation_entries(Some(&mut visitor), 0);
    let mut rows = entries.lock().ok()?;
    if rows.len() < 2 {
        return None;
    }
    rows.sort_by(|a, b| a.1.cmp(&b.1));
    let cur_pos = rows.iter().position(|(_, _, is_cur)| *is_cur).or_else(|| {
        let v = visible.as_deref()?;
        rows.iter().position(|(u, _, _)| u == v)
    })?;
    let target_pos = if back {
        cur_pos.checked_sub(1)?
    } else if cur_pos + 1 < rows.len() {
        cur_pos + 1
    } else {
        return None;
    };
    Some(rows[target_pos].0.clone())
}

fn apply_history_navigation(browser: &Browser, go_forward: bool) {
    let before = visible_navigation_url(browser);
    if go_forward {
        browser.go_forward();
    } else {
        browser.go_back();
    }
    crate::browser::backend::cef::pump::pump(40);
    let after = visible_navigation_url(browser);
    let stuck = match (&before, &after) {
        (Some(b), Some(a)) => b == a,
        (Some(_), None) => true,
        _ => false,
    };
    if stuck {
        let back = !go_forward;
        if let Some(url) = history_url_adjacent(browser, back) {
            bevy_log::info!(
                target: "vmux",
                pid = std::process::id(),
                "apply_history_navigation: URL unchanged after go_{}; loading {:?}",
                if go_forward { "forward" } else { "back" },
                url
            );
            let u = CefString::from(url.as_str());
            if let Some(frame) = browser.main_frame() {
                frame.load_url(Some(&u));
                crate::browser::backend::cef::pump::pump(40);
            }
        }
    }
}

wrap_domvisitor! {
    struct LinkHintsSessionDomVisitor {
        out: Arc<Mutex<LinkHintsFeedOutcome>>,
    }

    impl Domvisitor {
        fn visit(&self, document: Option<&mut Domdocument>) {
            let (active, width) = match document {
                None => (false, 1u8),
                Some(doc) => doc
                    .document()
                    .map(|root| {
                        let hints = CefString::from("data-vmux-hints");
                        let active = root.has_element_attribute(Some(&hints)) != 0;
                        let width = if active {
                            let raw = root.element_attribute(Some(&hints));
                            let s = CefStringUtf8::from(&CefStringUtf16::from(&raw)).to_string();
                            s.trim().parse::<u8>().unwrap_or(2).clamp(1, 32)
                        } else {
                            1u8
                        };
                        (active, width)
                    })
                    .unwrap_or((false, 1u8)),
            };
            if let Ok(mut g) = self.out.lock() {
                g.still_active = active;
                g.hint_label_width = width;
            }
        }
    }
}

pub fn apply_navigate_browser_events_system(
    mut events: EventReader<NavigateBrowserEvent>,
    hub: Res<ForeignOsrIndexResource>,
) {
    for ev in events.read() {
        if currently_on(ThreadId::UI) == 0 {
            let mut task = NavigateCefBrowserPerformOnUiTask::new(ev.browser_id, ev.go_forward);
            if post_task(ThreadId::UI, Some(&mut task)) == 0 {
                bevy_log::warn!(
                    target: "vmux",
                    pid = std::process::id(),
                    "apply_navigate_browser_events_system: post_task to UI thread failed"
                );
            }
            continue;
        }
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let Some(browser) = cef_browser_by_id(ev.browser_id) else {
            continue;
        };
        apply_history_navigation(&browser, ev.go_forward);
        if let Some(host) = browser.host() {
            host.set_focus(1);
        }
        let bid = browser.identifier();
        foreign_index::reset_paint_redraw_throttle(hub.0.as_ref(), bid);
        repaint_full_geometry(&browser);
        crate::browser::backend::cef::pump::pump(10);
        foreign_index::reset_paint_redraw_throttle(hub.0.as_ref(), bid);
        repaint_full_geometry(&browser);
        crate::browser::backend::cef::pump::pump(10);
        for _ in 0..3 {
            request_window_redraw_for_browser(hub.0.as_ref(), bid);
            crate::browser::backend::cef::pump::pump(4);
        }
        navigation_repaint_pass_on_ui(hub.0.as_ref(), bid);
        sync_title_from_visible_navigation(&browser);
        request_window_redraw_for_browser(hub.0.as_ref(), bid);
        crate::browser::backend::cef::pump::pump(3);
        let mut delayed = crate::browser::DelayedNavigationRepaint::new(bid);
        let _ = cef::post_delayed_task(ThreadId::UI, Some(&mut delayed), 75);
    }
}

pub fn apply_reload_browser_events_system(
    mut events: EventReader<ReloadBrowserEvent>,
    hub: Res<ForeignOsrIndexResource>,
) {
    for ev in events.read() {
        if currently_on(ThreadId::UI) != 0 {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let attach = browser_cef_attach();
            let from_store = attach.as_ref().and_then(|a| {
                a.windows_store.lock().ok().and_then(|ws| {
                    ws.values()
                        .find(|e| e.browser.identifier() == ev.browser_id)
                        .map(|e| e.browser.clone())
                })
            });
            let browser = from_store.or_else(|| {
                browser_cef_handles()
                    .lock()
                    .ok()
                    .and_then(|g| g.get(ev.browser_id).cloned())
            });
            let Some(browser) = browser else {
                continue;
            };
            browser.reload();
            let bid = browser.identifier();
            foreign_index::reset_paint_redraw_throttle(hub.0.as_ref(), bid);
            crate::browser::backend::cef::pump::pump(24);
            request_window_redraw_for_browser(hub.0.as_ref(), bid);
            continue;
        }
        let mut task = ReloadBrowserPerformOnUiTask::new(ev.browser_id);
        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "apply_reload_browser_events_system: post_task to UI thread failed"
            );
        }
    }
}

pub fn apply_delayed_navigation_repaint_on_ui_events_system(
    mut events: EventReader<DelayedNavigationRepaintBrowserEvent>,
    hub: Res<ForeignOsrIndexResource>,
) {
    for ev in events.read() {
        if currently_on(ThreadId::UI) != 0 {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let attach = browser_cef_attach();
            let from_store = attach.as_ref().and_then(|a| {
                a.windows_store.lock().ok().and_then(|ws| {
                    ws.values()
                        .find(|e| e.browser.identifier() == ev.browser_id)
                        .map(|e| e.browser.clone())
                })
            });
            let browser = from_store.or_else(|| {
                browser_cef_handles()
                    .lock()
                    .ok()
                    .and_then(|g| g.get(ev.browser_id).cloned())
            });
            let Some(browser) = browser else {
                continue;
            };
            let bid = ev.browser_id;
            foreign_index::reset_paint_redraw_throttle(hub.0.as_ref(), bid);
            if let Some(host) = browser.host() {
                host.was_resized();
                host.notify_screen_info_changed();
                host.invalidate(cef::PaintElementType::VIEW);
                #[cfg(all(
                    any(target_os = "macos", target_os = "windows", target_os = "linux"),
                    feature = "accelerated_osr"
                ))]
                host.send_external_begin_frame();
            }
            crate::browser::backend::cef::pump::pump(10);
            request_window_redraw_for_browser(hub.0.as_ref(), bid);
            continue;
        }
        let mut task = DelayedNavigationRepaintPerformOnUiTask::new(ev.browser_id);
        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "apply_delayed_navigation_repaint_on_ui_events_system: post_task failed"
            );
        }
    }
}

pub fn apply_show_main_window_on_ui_events_system(
    mut events: EventReader<ShowMainWindowBrowserEvent>,
) {
    for _ in events.read() {
        if currently_on(ThreadId::UI) != 0 {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let handles = browser_cef_handles();
            let Ok(guard) = handles.lock() else {
                continue;
            };
            let Some(first_id) = guard.first_browser_id() else {
                continue;
            };
            let Some(mut main_browser) = guard.get(first_id).cloned() else {
                continue;
            };
            drop(guard);
            if let Some(browser_view) = browser_view_get_for_browser(Some(&mut main_browser)) {
                if let Some(window) = browser_view.window() {
                    window.show();
                }
            } else {
                crate::windows::show_all_windows();
            }
            continue;
        }
        let mut task = ShowMainWindowPerformOnUiTask::new();
        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "apply_show_main_window_on_ui_events_system: post_task failed"
            );
        }
    }
}

/// Close every browser from the CEF UI thread (`close_browser` is not safe from arbitrary threads).
///
/// [`apply_close_all_browsers_on_ui_events_system`] and [`CloseAllBrowsersPerformOnUiTask`] use this.
/// If [`browser_cef_handles`] is empty (ordering races), falls back to [`browser_cef_attach`] OSR
/// [`windows_store`](crate::browser::backend::osr::hub::CefAttach::windows_store).
pub(crate) fn close_all_browsers_on_ui_thread(force_close: bool) {
    if currently_on(ThreadId::UI) == 0 {
        crate::lifecycle_trace::record_runtime_event(
            "close_all_browsers_on_ui_thread skip not_on_cef_ui_thread",
        );
        return;
    }
    // A prior `close_browser` can set `is_closing` while CEF is still tearing down. If that stalls,
    // the user hits Cmd+Q again — `force_close` must retry, not no-op (see runtime-events log: second
    // quit forwarded CloseAll but never reached `count=`).
    if !force_close && browser_lifecycle().lock().map(|g| g.is_closing).unwrap_or(false) {
        crate::lifecycle_trace::record_runtime_event(
            "close_all_browsers_on_ui_thread skip already_closing_non_force",
        );
        return;
    }

    let has_cef_attach = browser_cef_attach().is_some();
    if has_cef_attach {
        if let Ok(mut g) = browser_close_guards().lock() {
            g.bypass_windowless_do_close_guard = true;
        }
        if force_close {
            if let Ok(mut g) = browser_lifecycle().lock() {
                g.is_closing = true;
            }
        }
    }

    let mut browsers = browser_cef_handles()
        .lock()
        .map(|g| g.values_cloned())
        .unwrap_or_default();
    if browsers.is_empty() {
        if let Some(attach) = browser_cef_attach() {
            if let Ok(ws) = attach.windows_store.lock() {
                browsers = ws.values().map(|e| e.browser.clone()).collect();
            }
        }
    }

    if browsers.is_empty() {
        crate::lifecycle_trace::record_runtime_event(&format!(
            "close_all_browsers_on_ui_thread force_close={force_close} count=0 push_shutdown_unstick",
        ));
        if force_close {
            if let Ok(mut g) = browser_lifecycle().lock() {
                g.is_closing = false;
            }
            crate::browser::event_loop::send_user_event(
                crate::browser::event_loop::UserEvent::App(
                    crate::browser::event_loop::AppEvent::ShutdownRequested,
                ),
            );
        }
        return;
    }

    crate::lifecycle_trace::record_runtime_event(&format!(
        "close_all_browsers_on_ui_thread force_close={force_close} count={}",
        browsers.len()
    ));

    for browser in browsers {
        if let Some(browser_host) = browser.host() {
            browser_host.close_browser(force_close.into());
        }
    }

    if has_cef_attach && !force_close {
        if let Ok(mut g) = browser_close_guards().lock() {
            g.bypass_windowless_do_close_guard = false;
        }
    }
}

pub fn apply_close_all_browsers_on_ui_events_system(
    mut events: EventReader<CloseAllBrowsersBrowserEvent>,
) {
    for ev in events.read() {
        if currently_on(ThreadId::UI) != 0 {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            close_all_browsers_on_ui_thread(ev.force_close);
            continue;
        }
        let mut task = CloseAllBrowsersPerformOnUiTask::new(ev.force_close);
        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "apply_close_all_browsers_on_ui_events_system: post_task failed"
            );
        }
    }
}

pub fn apply_link_hints_show_browser_events_system(
    mut events: EventReader<LinkHintsShowBrowserEvent>,
    hub: Res<ForeignOsrIndexResource>,
) {
    for ev in events.read() {
        if currently_on(ThreadId::UI) != 0 {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let Some(browser) = cef_browser_by_id(ev.browser_id) else {
                continue;
            };
            let Some(frame) = browser.main_frame().or_else(|| browser.focused_frame()) else {
                continue;
            };
            let code = CefString::from(VMUX_LINK_HINTS_JS);
            let url = CefString::from("vmux://link-hints");
            frame.execute_java_script(Some(&code), Some(&url), 0);
            crate::browser::backend::cef::pump::pump(8);
            let _snap = link_hints_read_session_with_browser(&browser);
            request_window_redraw_for_browser(hub.0.as_ref(), ev.browser_id);
            continue;
        }
        let mut task = LinkHintsShowPerformOnUiTask::new(ev.browser_id);
        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "apply_link_hints_show_browser_events_system: post_task failed"
            );
        }
    }
}

pub fn apply_link_hints_hide_browser_events_system(
    mut events: EventReader<LinkHintsHideBrowserEvent>,
) {
    for ev in events.read() {
        if currently_on(ThreadId::UI) != 0 {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let Some(browser) = cef_browser_by_id(ev.browser_id) else {
                continue;
            };
            let Some(frame) = browser.main_frame().or_else(|| browser.focused_frame()) else {
                continue;
            };
            let code = CefString::from(
                "try{window.__vmux_hints_cleanup&&window.__vmux_hints_cleanup();}catch(e){}",
            );
            let url = CefString::from("vmux://link-hints-clear");
            frame.execute_java_script(Some(&code), Some(&url), 0);
            continue;
        }
        let mut task = LinkHintsHidePerformOnUiTask::new(ev.browser_id);
        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "apply_link_hints_hide_browser_events_system: post_task failed"
            );
        }
    }
}

pub fn apply_link_hints_feed_key_deferred_browser_events_system(
    mut events: EventReader<LinkHintsFeedKeyDeferredBrowserEvent>,
    hub: Res<ForeignOsrIndexResource>,
) {
    for ev in events.read() {
        if currently_on(ThreadId::UI) != 0 {
            let outcome = link_hints_feed_key_on_ui(ev.browser_id, ev.ch, ev.prior_typed_len);
            if let Some(wid) = foreign_index::window_id_for_browser(hub.0.as_ref(), ev.browser_id) {
                crate::browser::event_loop::send_user_event(
                    crate::browser::event_loop::UserEvent::App(
                        crate::browser::event_loop::AppEvent::LinkHintFeed(
                            crate::browser::event_loop::LinkHintFeedEvent {
                                window_id: wid,
                                browser_id: ev.browser_id,
                                ch: ev.ch,
                                prior_typed_len: ev.prior_typed_len,
                                still_active: outcome.still_active,
                                hint_label_width: outcome.hint_label_width,
                            },
                        ),
                    ),
                );
            }
            continue;
        }
        let mut task =
            LinkHintsFeedKeyDeferredPerformOnUiTask::new(ev.browser_id, ev.ch, ev.prior_typed_len);
        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "apply_link_hints_feed_key_deferred_browser_events_system: post_task failed"
            );
        }
    }
}

pub fn apply_arm_windowless_close_browser_events_system(
    mut events: EventReader<ArmWindowlessCloseBrowserEvent>,
) {
    for _ in events.read() {
        if let Ok(mut g) = browser_close_guards().lock() {
            g.allow_next_windowless_do_close = true;
        }
    }
}

pub fn apply_quit_close_all_browsers_browser_events_system(
    mut events: EventReader<QuitCloseAllBrowsersBrowserEvent>,
    mut close_all: EventWriter<RequestCloseAllBrowsersEvent>,
) {
    for _ in events.read() {
        close_all.send(RequestCloseAllBrowsersEvent { force_close: true });
    }
}

pub fn apply_address_changed_browser_events_system(
    mut events: EventReader<AddressChangedBrowserEvent>,
    hub: Res<ForeignOsrIndexResource>,
    mut link_hints_nav_invalidate: EventWriter<LinkHintsNavInvalidateBrowser>,
) {
    for ev in events.read() {
        let _ = ev.url.as_deref();
        if browser_cef_attach().is_none() {
            continue;
        }
        let Some(browser) = cef_browser_by_id(ev.browser_id) else {
            continue;
        };
        let bid = browser.identifier();
        let changed = true;
        sync_title_from_visible_navigation(&browser);
        if changed {
            if browser_cef_attach().is_some() {
                foreign_index::reset_paint_redraw_throttle(hub.0.as_ref(), bid);
                if let Some(host) = browser.host() {
                    host.was_resized();
                    host.notify_screen_info_changed();
                    host.invalidate(cef::PaintElementType::VIEW);
                    #[cfg(all(
                        any(target_os = "macos", target_os = "windows", target_os = "linux"),
                        feature = "accelerated_osr"
                    ))]
                    host.send_external_begin_frame();
                }
                request_window_redraw_for_browser(hub.0.as_ref(), bid);
                crate::browser::backend::cef::pump::pump(4);
            }
            let overlay_likely_up = currently_on(ThreadId::UI) != 0
                && link_hints_read_session_with_browser(&browser).still_active;
            if !overlay_likely_up {
                link_hints_nav_invalidate.send(LinkHintsNavInvalidateBrowser { browser_id: bid });
            }
        } else {
            request_window_redraw_for_browser(hub.0.as_ref(), bid);
            #[cfg(all(
                any(target_os = "macos", target_os = "windows", target_os = "linux"),
                feature = "accelerated_osr"
            ))]
            if let Some(host) = browser.host() {
                host.send_external_begin_frame();
            }
            crate::browser::backend::cef::pump::pump(4);
        }
    }
}

pub fn apply_title_changed_browser_events_system(
    mut events: EventReader<TitleChangedBrowserEvent>,
    hub: Res<ForeignOsrIndexResource>,
) {
    for ev in events.read() {
        let browser_id = ev.browser_id;
        let title = ev.title.as_deref();
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let mut browser = cef_browser_by_id(browser_id);
        if let Some(browser_view) = browser_view_get_for_browser(browser.as_mut()) {
            if let Some(window) = browser_view.window() {
                let title_cef = title.map(CefString::from);
                window.set_title(title_cef.as_ref());
                continue;
            }
        }
        if let Some(t) = title {
            crate::browser::renderer::osr_host::titles::push_title(browser_id, t.to_string());
        }
        if browser_cef_attach().is_some() {
            if let Some(ref b) = browser {
                let bid = b.identifier();
                if let Some(host) = b.host() {
                    host.invalidate(cef::PaintElementType::default());
                    #[cfg(all(
                        any(target_os = "macos", target_os = "windows", target_os = "linux"),
                        feature = "accelerated_osr"
                    ))]
                    host.send_external_begin_frame();
                }
                request_window_redraw_for_browser(hub.0.as_ref(), bid);
                crate::browser::backend::cef::pump::pump(4);
            }
        }
    }
}

pub fn apply_loading_state_changed_browser_events_system(
    mut events: EventReader<LoadingStateChangedBrowserEvent>,
    hub: Res<ForeignOsrIndexResource>,
    mut link_hints_nav_invalidate: EventWriter<LinkHintsNavInvalidateBrowser>,
) {
    for ev in events.read() {
        let browser_id = ev.browser_id;
        let Some(browser) = cef_browser_by_id(browser_id) else {
            continue;
        };
        if browser_cef_attach().is_none() {
            continue;
        }
        if ev.is_loading != 0 {
            link_hints_nav_invalidate.send(LinkHintsNavInvalidateBrowser { browser_id });
            if let Some(host) = browser.host() {
                host.invalidate(cef::PaintElementType::VIEW);
                #[cfg(all(
                    any(target_os = "macos", target_os = "windows", target_os = "linux"),
                    feature = "accelerated_osr"
                ))]
                host.send_external_begin_frame();
            }
            request_window_redraw_for_browser(hub.0.as_ref(), browser_id);
            crate::browser::backend::cef::pump::pump(5);
            continue;
        }
        if let Some(attach) = browser_cef_attach() {
            let deferred = attach
                .deferred_url_after_blank
                .lock()
                .ok()
                .and_then(|mut m| m.remove(&browser_id));
            if let Some(next_url) = deferred {
                let mut task = DeferredStartupNavTask::new(browser_id, next_url.clone());
                if post_task(ThreadId::UI, Some(&mut task)) == 0 {
                    bevy_log::warn!(
                        target: "vmux",
                        pid = std::process::id(),
                        "apply_loading_state_changed: post_task failed for deferred startup URL — sync load_url"
                    );
                    let u = CefString::from(next_url.as_str());
                    if let Some(frame) = browser.main_frame() {
                        frame.load_url(Some(&u));
                    }
                    crate::browser::backend::cef::pump::pump(8);
                }
            }
        }
        repaint_full_geometry(&browser);
        crate::browser::event_loop::send_user_event(
            crate::browser::event_loop::UserEvent::Cef(
                crate::browser::event_loop::CefEvent::SetEditableFocusHint {
                    browser_id,
                    editable: false,
                },
            ),
        );
        request_window_redraw_for_browser(hub.0.as_ref(), browser_id);
        crate::browser::editable_focus::schedule_editable_focus_probe(browser_id);
    }
}

/// Handles [`AfterCreatedBrowserCallbackEvent`]: registers the browser in [`CefBrowserHandlesInner`],
/// pops the matching OSR shell from the fifo, and enqueues [`OsrAfterCreatedWork`] for
/// [`apply_osr_browser_attach_system`]. Windowless spawns finish here.
pub(crate) fn enqueue_after_created_osr_attach_system(
    mut events: EventReader<AfterCreatedBrowserCallbackEvent>,
    attach_queue: ResMut<OsrAfterCreatedAttachQueue>,
) {
    for ev in events.read() {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let browser_id = ev.browser_id;
        let Some(browser) = browser_host_get_browser_by_identifier(browser_id) else {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "AfterCreatedBrowserCallbackEvent: browser_host_get_browser_by_identifier({browser_id}) returned None (ignored)"
            );
            continue;
        };
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "AfterCreated: resolved browser_id={browser_id} via get_browser_by_identifier"
        );
        if let Some(ref attach) = browser_cef_attach() {
            if let Ok(mut g) = browser_cef_handles().lock() {
                g.insert_spawn(browser_id, browser.clone());
            }
            let shell = attach
                .shell_fifo
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop_front();
            let Some(shell) = shell else {
                bevy_log::error!(target: "vmux", pid = std::process::id(), "FATAL: on_after_created: no pending OSR shell (fifo empty)");
                std::process::exit(1);
            };
            attach_queue
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push_back(OsrAfterCreatedWork {
                    browser_id,
                    browser,
                    shell,
                });
        } else {
            if let Ok(mut g) = browser_cef_handles().lock() {
                g.insert_spawn(browser_id, browser.clone());
            }
            crate::browser::event_loop::send_user_event(
                crate::browser::event_loop::UserEvent::App(
                    crate::browser::event_loop::AppEvent::BrowserSpawn(
                        crate::browser::event_loop::BrowserSpawnEvent {
                            browser_id,
                            window_id: WindowId::dummy(),
                            browser,
                            osr_view_logical_size: None,
                            osr_paint_bind_group: None,
                        },
                    ),
                ),
            );
        }
    }
}

/// Drains [`OsrAfterCreatedAttachQueue`]: [`foreign_index::register_tab`], `windows_store`, spawn + focus-hint user events.
pub(crate) fn apply_osr_browser_attach_system(
    attach_queue: ResMut<OsrAfterCreatedAttachQueue>,
    hub: Res<ForeignOsrIndexResource>,
) {
    let Some(attach) = browser_cef_attach() else {
        return;
    };
    let mut q = attach_queue
        .0
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    while let Some(work) = q.pop_front() {
        let browser_id = work.browser_id;
        let browser = work.browser;
        let shell = work.shell;
        let wid = shell.surface.window.id();
        let shared = foreign_index::register_tab(
            hub.0.as_ref(),
            browser_id,
            wid,
            shell.logical,
        );
        let browser_for_ecs = browser.clone();
        let mut ws = attach
            .windows_store
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        ws.insert(
            wid,
            crate::browser::backend::osr::hub::WindowEntry {
                surface: shell.surface,
                browser,
                size: shared.view_logical_size.clone(),
            },
        );
        if let Some(entry) = ws.get(&wid) {
            entry.surface.window.request_redraw();
            let vis = entry.surface.window.is_visible();
            bevy_log::info!(
                target: "vmux",
                pid = std::process::id(),
                "proof: osr_attached_browser_live browser_id={browser_id} winit_visible={vis:?} redraw_requested_for_swapchain"
            );
        }
        drop(ws);
        if let Some(next_url) = shell.deferred_url {
            if let Ok(mut m) = attach.deferred_url_after_blank.lock() {
                m.insert(browser_id, next_url);
            }
        }
        crate::browser::event_loop::send_user_event(
            crate::browser::event_loop::UserEvent::App(
                crate::browser::event_loop::AppEvent::BrowserSpawn(
                    crate::browser::event_loop::BrowserSpawnEvent {
                        browser_id,
                        window_id: wid,
                        browser: browser_for_ecs,
                        osr_view_logical_size: Some(shared.view_logical_size.clone()),
                        osr_paint_bind_group: Some(shared.paint_bind_group.clone()),
                    },
                ),
            ),
        );
        crate::browser::event_loop::send_user_event(
            crate::browser::event_loop::UserEvent::Cef(
                crate::browser::event_loop::CefEvent::SetEditableFocusHint {
                    browser_id,
                    editable: false,
                },
            ),
        );
        attach.unpaired_cef_shells.fetch_sub(1, Ordering::Release);
    }
}

pub fn apply_before_close_browser_callback_events_system(
    mut events: EventReader<BeforeCloseBrowserCallbackEvent>,
    hub: Res<ForeignOsrIndexResource>,
) {
    for ev in events.read().cloned() {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let Some(browser) = ev.browser else {
            bevy_log::warn!(
                target: "vmux",
                pid = std::process::id(),
                "BeforeCloseBrowserCallbackEvent: browser is None (ignored)"
            );
            continue;
        };
        let removed_id = browser.identifier();
        crate::browser::event_loop::send_user_event(
            crate::browser::event_loop::UserEvent::Cef(
                crate::browser::event_loop::CefEvent::RemoveBrowserEntries {
                    browser_id: removed_id,
                },
            ),
        );
        let wid_for_removed = if browser_cef_attach().is_some() {
            foreign_index::window_id_for_browser(hub.0.as_ref(), removed_id)
        } else {
            None
        };
        if let Ok(mut g) = browser_cef_handles().lock() {
            g.remove_despawn(removed_id);
        }
        crate::browser::event_loop::send_user_event(
            crate::browser::event_loop::UserEvent::App(
                crate::browser::event_loop::AppEvent::BrowserDespawn(
                    crate::browser::event_loop::BrowserDespawnEvent {
                        browser_id: removed_id,
                    },
                ),
            ),
        );
        foreign_index::unregister_tab(hub.0.as_ref(), removed_id);

        if let Some(attach) = browser_cef_attach().as_ref() {
            let mut ws = attach
                .windows_store
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(wid) = wid_for_removed {
                ws.remove(&wid);
            }
            // If `browser_to_window` missed (race / ordering), `remove(wid)` never ran — then
            // `handle_about_to_wait` keeps seeing a non-empty `windows_store` and never sets
            // [`ShutdownFlag`], so Cmd+Q leaves the process spinning.
            let before_retain = ws.len();
            ws.retain(|_, entry| entry.browser.identifier() != removed_id);
            if wid_for_removed.is_none() && before_retain > 0 && before_retain == ws.len() {
                bevy_log::warn!(
                    target: "vmux",
                    pid = std::process::id(),
                    "before_close: windows_store still has no match for browser_id={removed_id} after foreign-index miss (stale map?)"
                );
            }
        }
        if browser_cef_handles().lock().map(|g| g.is_empty()).unwrap_or(true) {
            if let Some(ref attach) = browser_cef_attach() {
                let ws_empty = attach
                    .windows_store
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_empty();
                let fifo_empty = attach
                    .shell_fifo
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_empty();
                let unpaired = attach.unpaired_cef_shells.load(Ordering::Acquire);
                if !ws_empty || !fifo_empty || unpaired > 0 {
                    continue;
                }
            }
            if let Ok(mut g) = browser_close_guards().lock() {
                g.bypass_windowless_do_close_guard = false;
            }
            crate::lifecycle_trace::record_runtime_event(
                "before_close last browser: send ShutdownRequested",
            );
            crate::browser::event_loop::send_user_event(
                crate::browser::event_loop::UserEvent::App(
                    crate::browser::event_loop::AppEvent::ShutdownRequested,
                ),
            );
        }
    }
}

pub fn apply_do_close_browser_callback_events_system(
    mut events: EventReader<DoCloseBrowserCallbackEvent>,
) {
    for _ in events.read() {
        let _ = do_close_from_event();
    }
}

pub fn apply_load_error_browser_callback_events_system(
    mut events: EventReader<LoadErrorBrowserCallbackEvent>,
) {
    for ev in events.read().cloned() {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let error_code = cef::sys::cef_errorcode_t::from(ev.error_code);
        if error_code == cef::sys::cef_errorcode_t::ERR_ABORTED {
            continue;
        }
        let error_code = error_code as i32;
        let Some(frame) = ev.frame.as_ref() else {
            continue;
        };
        let error_text = ev.error_text.as_deref().unwrap_or_default();
        let failed_url = ev.failed_url.as_deref().unwrap_or_default();
        let data = format!(
            r#"
            <html>
                <body bgcolor="white">
                    <h2>Failed to load URL {failed_url} with error {error_text} ({error_code}).</h2>
                </body>
            </html>
            "#
        );
        let data = CefString::from(&base64_encode(Some(data.as_bytes())));
        let uri = CefString::from(&uriencode(Some(&data), 0)).to_string();
        let uri = CefString::from(format!("data:text/html;base64,{uri}").as_str());
        let frame = frame.clone();
        frame.load_url(Some(&uri));
    }
}

wrap_task! {
    struct ReloadBrowserPerformOnUiTask {
        browser_id: i32,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            crate::browser::event_loop::send_user_event(
                crate::browser::event_loop::UserEvent::Cef(
                    crate::browser::event_loop::CefEvent::ReloadBrowser(
                        ReloadBrowserEvent {
                            browser_id: self.browser_id,
                        },
                    ),
                ),
            );
        }
    }
}

wrap_task! {
    struct LinkHintsShowPerformOnUiTask {
        browser_id: i32,
    }
    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            crate::browser::event_loop::send_user_event(
                crate::browser::event_loop::UserEvent::Vimium(
                    crate::browser::event_loop::VimiumEvent::LinkHintsShowBrowser(
                        LinkHintsShowBrowserEvent {
                            browser_id: self.browser_id,
                        },
                    ),
                ),
            );
        }
    }
}

wrap_task! {
    struct LinkHintsHidePerformOnUiTask {
        browser_id: i32,
    }
    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            crate::browser::event_loop::send_user_event(
                crate::browser::event_loop::UserEvent::Vimium(
                    crate::browser::event_loop::VimiumEvent::LinkHintsHideBrowser(
                        LinkHintsHideBrowserEvent {
                            browser_id: self.browser_id,
                        },
                    ),
                ),
            );
        }
    }
}

wrap_task! {
    struct LinkHintsFeedKeyDeferredPerformOnUiTask {
        browser_id: i32,
        ch: char,
        prior_typed_len: usize,
    }
    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            crate::browser::event_loop::send_user_event(
                crate::browser::event_loop::UserEvent::Vimium(
                    crate::browser::event_loop::VimiumEvent::LinkHintsFeedKeyDeferredBrowser(
                        LinkHintsFeedKeyDeferredBrowserEvent {
                            browser_id: self.browser_id,
                            ch: self.ch,
                            prior_typed_len: self.prior_typed_len,
                        },
                    ),
                ),
            );
        }
    }
}

wrap_task! {
    struct DelayedNavigationRepaintPerformOnUiTask {
        browser_id: i32,
    }
    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            crate::browser::event_loop::send_user_event(
                crate::browser::event_loop::UserEvent::Cef(
                    crate::browser::event_loop::CefEvent::DelayedNavigationRepaintBrowser(
                        DelayedNavigationRepaintBrowserEvent {
                            browser_id: self.browser_id,
                        },
                    ),
                ),
            );
        }
    }
}

wrap_task! {
    struct ShowMainWindowPerformOnUiTask {}
    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            crate::browser::event_loop::send_user_event(
                crate::browser::event_loop::UserEvent::App(
                    crate::browser::event_loop::AppEvent::ShowMainWindowBrowser(
                        ShowMainWindowBrowserEvent,
                    ),
                ),
            );
        }
    }
}

wrap_task! {
    struct CloseAllBrowsersPerformOnUiTask {
        force_close: bool,
    }
    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            // Must call `close_browser` here on the CEF UI thread. Re-sending
            // `CloseAllBrowsersBrowserEvent` to the winit main thread ping-pongs forever when
            // `currently_on(UI)` is false during Bevy `Update` (external message pump).
            close_all_browsers_on_ui_thread(self.force_close);
        }
    }
}

fn repaint_full_geometry(browser: &Browser) {
    let Some(host) = browser.host() else {
        return;
    };
    host.was_resized();
    host.notify_screen_info_changed();
    host.invalidate(cef::PaintElementType::VIEW);
    #[cfg(all(
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        feature = "accelerated_osr"
    ))]
    host.send_external_begin_frame();
}

/// Resolve `cef::Browser` by CEF id: prefer **`windows_store`** (OSR), else **`CefBrowserHandlesInner`**.
/// See [`crate::browser::browser_entity`] module docs for the three-store model.
pub(crate) fn cef_browser_by_id(browser_id: i32) -> Option<Browser> {
    let attach = browser_cef_attach();
    let from_store = attach.as_ref().and_then(|a| {
        a.windows_store.lock().ok().and_then(|ws| {
            ws.values()
                .find(|e| e.browser.identifier() == browser_id)
                .map(|e| e.browser.clone())
        })
    });
    from_store.or_else(|| {
        browser_cef_handles()
            .lock()
            .ok()
            .and_then(|g| g.get(browser_id).cloned())
    })
}

fn request_window_redraw_for_browser(index: &ForeignOsrIndex, browser_id: i32) {
    let Some(ref attach) = browser_cef_attach() else {
        return;
    };
    let Some(wid) = foreign_index::window_id_for_browser(index, browser_id) else {
        return;
    };
    if let Ok(mut ws) = attach.windows_store.lock() {
        if let Some(entry) = ws.get_mut(&wid) {
            entry.surface.window.request_redraw();
        }
    }
}

fn sync_title_from_visible_navigation(browser: &Browser) {
    if browser_cef_attach().is_none() {
        return;
    }
    let bid = browser.identifier();
    let Some(host) = browser.host() else {
        return;
    };
    let Some(entry) = host.visible_navigation_entry() else {
        return;
    };
    if entry.is_valid() == 0 {
        return;
    }
    let title_raw = entry.title();
    let mut title_str = CefStringUtf8::from(&CefStringUtf16::from(&title_raw)).to_string();
    if title_str.is_empty() {
        let disp = entry.display_url();
        title_str = CefStringUtf8::from(&CefStringUtf16::from(&disp)).to_string();
    }
    if !title_str.is_empty() {
        crate::browser::renderer::osr_host::titles::push_title(bid, title_str);
    }
}

fn navigation_repaint_pass_on_ui(index: &ForeignOsrIndex, browser_id: i32) {
    debug_assert_ne!(currently_on(ThreadId::UI), 0);
    let Some(browser) = browser_cef_handles()
        .lock()
        .ok()
        .and_then(|g| g.get(browser_id).cloned())
    else {
        return;
    };
    foreign_index::reset_paint_redraw_throttle(index, browser_id);
    repaint_full_geometry(&browser);
    crate::browser::backend::cef::pump::pump(14);
    request_window_redraw_for_browser(index, browser_id);
}

/// Synchronous link-hint key feed on the CEF UI thread (see [`crate::browser::BrowserHandler::link_hints_feed_key`]).
pub(crate) fn link_hints_feed_key_on_ui(
    browser_id: i32,
    ch: char,
    prior_typed_len: usize,
) -> LinkHintsFeedOutcome {
    debug_assert_ne!(currently_on(ThreadId::UI), 0);
    if !ch.is_ascii_lowercase() {
        return link_hints_read_session_on_ui(browser_id);
    }
    let Some(browser) = cef_browser_by_id(browser_id) else {
        return LinkHintsFeedOutcome::default();
    };
    let Some(frame) = browser.main_frame().or_else(|| browser.focused_frame()) else {
        return LinkHintsFeedOutcome::default();
    };
    let code = format!(
        "try{{if(typeof window.__vmux_hints_feed==='function')window.__vmux_hints_feed('{}');}}catch(e){{}}",
        ch
    );
    let code = CefString::from(code.as_str());
    let url = CefString::from("vmux://link-hints-feed");
    frame.execute_java_script(Some(&code), Some(&url), 0);
    crate::browser::backend::cef::pump::pump(12);
    if let Some(h) = crate::browser::event_loop::try_foreign_osr_index() {
        request_window_redraw_for_browser(h.as_ref(), browser_id);
    }
    let snap = link_hints_read_session_with_browser(&browser);
    link_hints_finalize_feed_outcome(snap, prior_typed_len)
}

fn link_hints_read_session_with_browser(browser: &Browser) -> LinkHintsFeedOutcome {
    debug_assert_ne!(currently_on(ThreadId::UI), 0);
    let out = Arc::new(Mutex::new(LinkHintsFeedOutcome::default()));
    let Some(frame) = browser.main_frame().or_else(|| browser.focused_frame()) else {
        return LinkHintsFeedOutcome::default();
    };
    let mut visitor = LinkHintsSessionDomVisitor::new(Arc::clone(&out));
    frame.visit_dom(Some(&mut visitor));
    out.lock().ok().map(|g| g.clone()).unwrap_or_default()
}

fn link_hints_read_session_on_ui(browser_id: i32) -> LinkHintsFeedOutcome {
    let Some(browser) = cef_browser_by_id(browser_id) else {
        return LinkHintsFeedOutcome::default();
    };
    link_hints_read_session_with_browser(&browser)
}

fn link_hints_finalize_feed_outcome(
    snap: LinkHintsFeedOutcome,
    prior_typed_len: usize,
) -> LinkHintsFeedOutcome {
    let w_from_dom = snap.hint_label_width.max(1);
    if snap.still_active {
        return LinkHintsFeedOutcome {
            still_active: true,
            hint_label_width: w_from_dom,
        };
    }
    let w_cached = 2u8.max(1);
    let typed_len_after = prior_typed_len.saturating_add(1);
    let still = typed_len_after < w_cached as usize;
    LinkHintsFeedOutcome {
        still_active: still,
        hint_label_width: w_cached,
    }
}

wrap_task! {
    struct NavigateCefBrowserPerformOnUiTask {
        browser_id: i32,
        go_forward: bool,
    }
    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            crate::browser::event_loop::send_user_event(
                crate::browser::event_loop::UserEvent::Cef(
                    crate::browser::event_loop::CefEvent::NavigateBrowser(
                        NavigateBrowserEvent {
                            browser_id: self.browser_id,
                            go_forward: self.go_forward,
                        },
                    ),
                ),
            );
        }
    }
}

wrap_task! {
    struct DeferredStartupNavTask {
        browser_id: i32,
        url: String,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let Some(browser) = cef_browser_by_id(self.browser_id) else {
                return;
            };
            let u = CefString::from(self.url.as_str());
            if let Some(frame) = browser.main_frame() {
                frame.load_url(Some(&u));
            }
            crate::browser::backend::cef::pump::pump(8);
        }
    }
}

pub fn do_close_from_event() -> i32 {
    debug_assert_ne!(currently_on(ThreadId::UI), 0);
    // Make closure system-driven: Bevy systems decide when to call `close_browser`,
    // and this synchronous CEF callback simply permits destruction.
    if browser_cef_handles()
        .lock()
        .map(|g| g.len() == 1)
        .unwrap_or(false)
    {
        if let Ok(mut g) = browser_lifecycle().lock() {
            g.is_closing = true;
        }
    }
    0
}
