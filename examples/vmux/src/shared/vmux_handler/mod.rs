use cef::ImplBrowser as _;
use cef::ImplFrame as _;
use cef::*;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use winit::event::KeyEvent;
use winit::window::{CursorIcon, WindowId};

use crate::shared::launch_trace;
use crate::shared::vmux_osr::cef_pump;
use crate::shared::vmux_osr::hub::{VmuxOsrAttach, WindowEntry};
use crate::shared::vmux_osr::event_loop;

const VMUX_LINK_HINTS_JS: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/link_hints.js"));

/// After [`VmuxHandler::link_hints_feed_key`]: whether the hint session should stay armed in Rust.
/// `data-vmux-hints` on `<html>` holds the fixed label width (stringified integer); when a DOM read
/// lags after a key, Rust uses [`VmuxHandler::link_hints_feed_key`]'s `prior_typed_len` plus cached width.
/// Typed prefix is tracked in Rust (`OsrVimMachine`) only.
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

fn vmux_osr_cursor_from_cef(ty: CursorType) -> CursorIcon {
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

/// Match the winit resize path: `was_resized` + `notify_screen_info_changed` so CEF actually
/// schedules a new OSR paint. **Expensive** — call when content is ready (e.g. loading finished),
/// not on every input tick.
fn osr_repaint_full_geometry(browser: &Browser) {
    let Some(host) = browser.host() else {
        return;
    };
    host.was_resized();
    host.notify_screen_info_changed();
    host.invalidate(PaintElementType::VIEW);
    #[cfg(all(
        any(target_os = "macos", target_os = "windows", target_os = "linux"),
        feature = "accelerated_osr"
    ))]
    host.send_external_begin_frame();
}

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
    struct VmuxNavEntriesCollector {
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

/// Previous (`back == true`) or next session URL using **order in the session list**, not `index±1`.
/// CEF `index` values are not guaranteed consecutive; adjacent history is the neighboring row
/// after sorting by `index`.
fn history_url_adjacent(browser: &Browser, back: bool) -> Option<String> {
    let host = browser.host()?;
    let visible = visible_navigation_url(browser);
    let entries = Arc::new(Mutex::new(Vec::<(String, i32, bool)>::new()));
    let mut visitor = VmuxNavEntriesCollector::new(Arc::clone(&entries));
    host.navigation_entries(Some(&mut visitor), 0);
    let mut rows = entries.lock().ok()?;
    if rows.len() < 2 {
        return None;
    }
    rows.sort_by(|a, b| a.1.cmp(&b.1));
    let cur_pos = rows
        .iter()
        .position(|(_, _, is_cur)| *is_cur)
        .or_else(|| {
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

/// `go_back` / `go_forward` plus a **load_url** fallback when the visible URL does not move (OSR +
/// BFCache can leave the committed entry stale relative to what we paint).
fn apply_osr_history_navigation(browser: &Browser, go_forward: bool) {
    let before = visible_navigation_url(browser);
    if go_forward {
        browser.go_forward();
    } else {
        browser.go_back();
    }
    cef_pump::pump(40);
    let after = visible_navigation_url(browser);
    let stuck = match (&before, &after) {
        (Some(b), Some(a)) => b == a,
        (Some(_), None) => true,
        _ => false,
    };
    if stuck {
        let back = !go_forward;
        if let Some(url) = history_url_adjacent(browser, back) {
            launch_trace(&format!(
                "apply_osr_history_navigation: URL unchanged after go_{}; loading {url:?}",
                if go_forward { "forward" } else { "back" }
            ));
            let u = CefString::from(url.as_str());
            if let Some(frame) = browser.main_frame() {
                frame.load_url(Some(&u));
                cef_pump::pump(40);
            }
        }
    }
}

fn get_data_uri(data: &[u8], mime_type: &str) -> String {
    let data = CefString::from(&base64_encode(Some(data)));
    let uri = CefString::from(&uriencode(Some(&data), 0)).to_string();
    format!("data:{mime_type};base64,{uri}")
}

/// Whether this element is a control where the user normally types (CEF `is_editable` can miss
/// some React / shadow-DOM / ARIA setups).
fn dom_element_is_text_entry_host(node: &Domnode) -> bool {
    use ImplDomnode as _;
    if node.is_element() == 0 {
        return false;
    }
    let tag = CefStringUtf8::from(&CefStringUtf16::from(&node.element_tag_name())).to_string();
    let tag = tag.to_lowercase();
    match tag.as_str() {
        "textarea" => true,
        "select" => true,
        "input" => {
            let ty = CefString::from("type");
            let raw = node.element_attribute(Some(&ty));
            let t = CefStringUtf8::from(&CefStringUtf16::from(&raw))
                .to_string()
                .to_lowercase();
            match t.trim() {
                "hidden" | "button" | "submit" | "reset" | "checkbox" | "radio" | "file"
                | "image" | "range" | "color" => false,
                _ => true,
            }
        }
        _ => {
            let role = CefString::from("role");
            if node.has_element_attribute(Some(&role)) == 0 {
                return false;
            }
            let raw = node.element_attribute(Some(&role));
            let r = CefStringUtf8::from(&CefStringUtf16::from(&raw))
                .to_string()
                .to_lowercase();
            match r.trim() {
                "textbox" | "searchbox" | "combobox" | "spinbutton" => true,
                _ => false,
            }
        }
    }
}

/// Whether focus is in a context where **Shift+H** / **Shift+L** should go to the page.
///
/// `focused_node()` is often a `#text` node; `is_editable()` may be false there while an ancestor has
/// `contenteditable` or the real control is the parent `<input>`.
fn dom_focused_context_allows_typing(mut node: Domnode) -> bool {
    use ImplDomnode as _;

    for _ in 0..64 {
        if node.is_editable() != 0 {
            return true;
        }
        if dom_element_is_text_entry_host(&node) {
            return true;
        }
        if node.is_element() != 0 {
            let ce = CefString::from("contenteditable");
            if node.has_element_attribute(Some(&ce)) != 0 {
                let raw = node.element_attribute(Some(&ce));
                let v = CefStringUtf8::from(&CefStringUtf16::from(&raw)).to_string();
                let v = v.trim().to_lowercase();
                if v.is_empty() || v == "true" || v == "plaintext-only" {
                    return true;
                }
                if v != "false" && v != "inherit" {
                    return true;
                }
            }
        }
        let Some(parent) = node.parent() else {
            break;
        };
        node = parent;
    }
    false
}

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac::*;

#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "windows")]
use win::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux::*;

#[cfg(not(target_os = "macos"))]
fn platform_show_window(_browser: Option<&mut Browser>) {
    todo!("Implement platform_show_window for non-macOS platforms");
}

static VMUX_HANDLER_INSTANCE: OnceLock<Weak<Mutex<VmuxHandler>>> = OnceLock::new();

pub struct VmuxHandler {
    browser_list: Vec<Browser>,
    is_closing: bool,
    weak_self: Weak<Mutex<Self>>,
    active_browser_id: Option<i32>,
    /// When set, each `browser_host_create_browser` is paired with a `PendingOsrWindow` in the fifo.
    osr_attach: Option<VmuxOsrAttach>,
    /// Windowless OSR: CEF destroys the browser immediately if `do_close` returns **false**. We only
    /// return false when the user closed the winit window (`arm_windowless_close_from_winit`) or
    /// during `close_all_browsers` (`bypass_windowless_do_close_guard`). Otherwise return **true**
    /// to cancel spurious close requests (stops the window flashing away on startup).
    allow_next_windowless_do_close: bool,
    bypass_windowless_do_close_guard: bool,
    /// `None` = never invalidated (shell **may** use **Shift+H**/**Shift+L**). `Some(None)` = unknown
    /// after click/Tab — still **allow** history shortcuts: winit is often not the CEF UI thread, so
    /// the DOM probe may not run before the key is handled; blocking here left back/forward dead after
    /// focus churn. `Some(true)` = text field (do not steal). `Some(false)` = probe says not editable.
    osr_editable_focus_hint: HashMap<i32, Option<bool>>,
    /// Last `on_address_change` URL per OSR browser; used to OSR-repaint only on real navigations,
    /// not redundant callbacks.
    last_osr_address_url: HashMap<i32, String>,
    /// Last `data-vmux-hints` label width per OSR browser (seeded at inject / refreshed while overlay reads active).
    osr_link_hints_label_width: HashMap<i32, u8>,
}

impl VmuxHandler {
    pub fn instance() -> Option<Arc<Mutex<Self>>> {
        VMUX_HANDLER_INSTANCE.get().and_then(|weak| weak.upgrade())
    }

    pub fn new(osr_attach: Option<VmuxOsrAttach>) -> Arc<Mutex<Self>> {
        Arc::new_cyclic(|weak| {
            if let Err(instance) = VMUX_HANDLER_INSTANCE.set(weak.clone()) {
                assert_eq!(instance.strong_count(), 0, "Replacing a viable instance");
            }

            Mutex::new(Self {
                browser_list: Vec::new(),
                is_closing: false,
                weak_self: weak.clone(),
                active_browser_id: None,
                osr_attach,
                allow_next_windowless_do_close: false,
                bypass_windowless_do_close_guard: false,
                osr_editable_focus_hint: HashMap::new(),
                last_osr_address_url: HashMap::new(),
                osr_link_hints_label_width: HashMap::new(),
            })
        })
    }

    /// Whether vmux may handle **Shift+H** / **Shift+L** as back/forward.
    ///
    /// Only **`Some(true)`** (probe / IME says editable) blocks interception.
    ///
    /// Prefer calling [`Self::refresh_osr_editable_focus_hint_for_history`] first from winit; for
    /// **j/k/r**-style keys use [`Self::osr_vim_keys_safe_for_page`] instead so unknown hints do not
    /// steal typing.
    pub fn osr_may_handle_history_shortcuts(browser_id: i32) -> bool {
        let Some(handler) = Self::instance() else {
            return false;
        };
        let Ok(inner) = handler.lock() else {
            return false;
        };
        match inner.osr_editable_focus_hint.get(&browser_id) {
            None | Some(None) | Some(Some(false)) => true,
            Some(Some(true)) => false,
        }
    }

    /// After [`Self::refresh_osr_editable_focus_hint_for_history`], use this for **vim-style**
    /// single-key bindings (`j`, `k`, `r`, `g`, …): only **`Some(false)`** means the probe is sure
    /// focus is **not** in an editable — safe to intercept. **`None` / unknown** does **not** steal
    /// keys (passes them to the page so typing is not eaten when the hint was stale).
    pub fn osr_vim_keys_safe_for_page(browser_id: i32) -> bool {
        let Some(handler) = Self::instance() else {
            return false;
        };
        let Ok(inner) = handler.lock() else {
            return false;
        };
        match inner.osr_editable_focus_hint.get(&browser_id) {
            Some(Some(false)) => true,
            _ => false,
        }
    }

    /// Probe / `send_char` / IME marked this browser as having focus in a text control.
    pub fn osr_editable_focus_is_typing(browser_id: i32) -> bool {
        let Some(handler) = Self::instance() else {
            return false;
        };
        let Ok(inner) = handler.lock() else {
            return false;
        };
        matches!(inner.osr_editable_focus_hint.get(&browser_id), Some(Some(true)))
    }

    pub fn invalidate_osr_editable_focus_hint(browser_id: i32) {
        let Some(handler) = Self::instance() else {
            return;
        };
        if let Ok(mut inner) = handler.lock() {
            inner.osr_editable_focus_hint.insert(browser_id, None);
        }
    }

    pub fn set_osr_editable_focus_hint(browser_id: i32, editable: bool) {
        let Some(handler) = Self::instance() else {
            return;
        };
        if let Ok(mut inner) = handler.lock() {
            inner
                .osr_editable_focus_hint
                .insert(browser_id, Some(editable));
        }
    }

    pub fn schedule_osr_editable_focus_probe(browser_id: i32) {
        let thread_id = ThreadId::UI;
        if currently_on(thread_id) == 0 {
            let Some(handler) = Self::instance() else {
                return;
            };
            let mut task = ProbeOsrEditableFocus::new(handler, browser_id);
            post_task(thread_id, Some(&mut task));
            return;
        }
        Self::run_osr_editable_focus_probe_on_ui(browser_id);
    }

    pub fn run_osr_editable_focus_probe_on_ui(browser_id: i32) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let Some(handler_arc) = Self::instance() else {
            return;
        };
        let browser = {
            let Ok(inner) = handler_arc.lock() else {
                return;
            };
            inner
                .browser_list
                .iter()
                .find(|b| b.identifier() == browser_id)
                .cloned()
        };
        let Some(browser) = browser else {
            return;
        };
        let Some(frame) = browser.focused_frame().or_else(|| browser.main_frame()) else {
            return;
        };
        let mut visitor = VmuxEditableFocusDomVisitor::new(browser_id, handler_arc);
        frame.visit_dom(Some(&mut visitor));
    }

    /// Refresh editable-focus hint on the CEF UI thread (async; does not block the winit thread).
    ///
    /// `pump_app_events` runs without CEF’s UI-thread marker even when both share the main thread,
    /// so `run_osr_editable_focus_probe_on_ui` would otherwise be skipped and the hint can stay
    /// stale (e.g. still “in a text field” after focus moved to the page).
    pub fn refresh_osr_editable_focus_hint_for_history(browser_id: i32) {
        Self::schedule_osr_editable_focus_probe(browser_id);
    }

    /// Run the editable-focus probe then post [`event_loop::VmuxUserEvent::VimKeyReplay`] to the winit loop.
    pub fn post_editable_probe_for_vim_replay(
        browser_id: i32,
        window_id: WindowId,
        event: KeyEvent,
    ) {
        if currently_on(ThreadId::UI) != 0 {
            Self::run_osr_editable_focus_probe_on_ui(browser_id);
            event_loop::event_loop().send(event_loop::VmuxUserEvent::VimKeyReplay {
                window_id,
                event,
            });
            return;
        }
        let mut task = EditableProbeWakeTask::new(browser_id, window_id, event);
        if post_task(ThreadId::UI, Some(&mut task)) == 0 {
            launch_trace("post_editable_probe_for_vim_replay: post_task failed");
        }
    }

    pub fn set_active_browser(browser_id: i32) {
        let Some(handler) = Self::instance() else {
            return;
        };
        if let Ok(mut inner) = handler.lock() {
            inner.active_browser_id = Some(browser_id);
        }
    }

    /// Navigate a **specific** OSR browser (e.g. the window that received **Shift+H** / **Shift+L**).
    pub fn navigate_osr_browser(browser_id: i32, go_forward: bool) {
        let thread_id = ThreadId::UI;
        if currently_on(thread_id) == 0 {
            let Some(handler) = Self::instance() else {
                return;
            };
            let mut task = NavigateOsrBrowser::new(handler, browser_id, go_forward);
            if post_task(thread_id, Some(&mut task)) == 0 {
                launch_trace("navigate_osr_browser: post_task to UI thread failed");
            }
            return;
        }

        Self::navigate_osr_browser_on_ui(browser_id, go_forward);
    }

    pub fn reload_osr_browser(browser_id: i32) {
        let thread_id = ThreadId::UI;
        if currently_on(thread_id) == 0 {
            let Some(handler) = Self::instance() else {
                return;
            };
            let mut task = ReloadOsrBrowser::new(handler, browser_id);
            if post_task(thread_id, Some(&mut task)) == 0 {
                launch_trace("reload_osr_browser: post_task to UI thread failed");
            }
            return;
        }

        Self::reload_osr_browser_on_ui(browser_id);
    }

    fn osr_browser_by_id(browser_id: i32) -> Option<Browser> {
        let handler = Self::instance()?;
        let attach = handler.lock().ok().and_then(|h| h.osr_attach.clone());
        let from_store = attach.as_ref().and_then(|a| {
            a.windows_store.lock().ok().and_then(|ws| {
                ws.values()
                    .find(|e| e.browser.identifier() == browser_id)
                    .map(|e| e.browser.clone())
            })
        });
        from_store.or_else(|| {
            handler.lock().ok().and_then(|inner| {
                inner
                    .browser_list
                    .iter()
                    .find(|b| b.identifier() == browser_id)
                    .cloned()
            })
        })
    }

    fn link_hints_clear_on_ui(browser_id: i32) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let Some(browser) = Self::osr_browser_by_id(browser_id) else {
            return;
        };
        // Always target the **main** frame: hints are injected there. If we used
        // `focused_frame` first, focus could move into an iframe (e.g. Google ads);
        // then probe/feed would run in the wrong document, return false, and Rust
        // would clear `LinkHints` while the overlay still showed — second letter dead.
        let Some(frame) = browser.main_frame().or_else(|| browser.focused_frame()) else {
            return;
        };
        let code = CefString::from(
            "try{window.__vmux_hints_cleanup&&window.__vmux_hints_cleanup();}catch(e){}",
        );
        let url = CefString::from("vmux://link-hints-clear");
        frame.execute_java_script(Some(&code), Some(&url), 0);
        if let Some(h) = Self::instance() {
            if let Ok(mut inner) = h.lock() {
                inner.osr_link_hints_label_width.remove(&browser_id);
            }
        }
    }

    fn link_hints_inject_on_ui(browser_id: i32) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let Some(browser) = Self::osr_browser_by_id(browser_id) else {
            return;
        };
        let Some(frame) = browser.main_frame().or_else(|| browser.focused_frame()) else {
            return;
        };
        let code = CefString::from(VMUX_LINK_HINTS_JS);
        let url = CefString::from("vmux://link-hints");
        frame.execute_java_script(Some(&code), Some(&url), 0);
        cef_pump::pump(8);
        let snap = Self::link_hints_read_session_with_browser(&browser);
        if let Some(h) = Self::instance() {
            if let Ok(mut inner) = h.lock() {
                if snap.still_active {
                    inner
                        .osr_link_hints_label_width
                        .insert(browser_id, snap.hint_label_width.max(1));
                }
                inner.request_osr_window_redraw_for_browser(browser_id);
            }
        }
    }

    /// Show Vimium-style link hints in the given OSR browser (UI thread; pumps from winit if needed).
    pub fn link_hints_show(browser_id: i32) {
        let thread_id = ThreadId::UI;
        if currently_on(thread_id) == 0 {
            let Some(handler) = Self::instance() else {
                return;
            };
            let mut task = LinkHintsRun::new(handler, browser_id, true);
            if post_task(thread_id, Some(&mut task)) == 0 {
                launch_trace("link_hints_show: post_task to UI thread failed");
            }
            return;
        }
        Self::link_hints_inject_on_ui(browser_id);
    }

    /// Remove link-hint overlay / listeners (same threading as `link_hints_show`).
    pub fn link_hints_hide(browser_id: i32) {
        let thread_id = ThreadId::UI;
        if currently_on(thread_id) == 0 {
            let Some(handler) = Self::instance() else {
                return;
            };
            let mut task = LinkHintsRun::new(handler, browser_id, false);
            if post_task(thread_id, Some(&mut task)) == 0 {
                launch_trace("link_hints_hide: post_task to UI thread failed");
            }
            return;
        }
        Self::link_hints_clear_on_ui(browser_id);
    }

    /// Post link-hint feed to the CEF UI thread; completion is delivered via winit user events.
    pub fn link_hints_feed_key_deferred(browser_id: i32, ch: char, prior_typed_len: usize) {
        let thread_id = ThreadId::UI;
        if currently_on(thread_id) != 0 {
            let outcome = Self::link_hints_feed_key_on_ui(browser_id, ch, prior_typed_len);
            if let Some(wid) = crate::shared::vmux_osr::bootstrap::hub().window_id_for_browser(browser_id)
            {
                event_loop::event_loop().send(event_loop::VmuxUserEvent::LinkHintFeed {
                    window_id: wid,
                    browser_id,
                    ch,
                    prior_typed_len,
                    still_active: outcome.still_active,
                    hint_label_width: outcome.hint_label_width,
                });
            }
            return;
        }
        let Some(handler) = Self::instance() else {
            return;
        };
        let mut task = LinkHintsFeedTask::new(handler, browser_id, ch, prior_typed_len);
        if post_task(thread_id, Some(&mut task)) == 0 {
            launch_trace("link_hints_feed_key_deferred: post_task to UI thread failed");
        }
    }

    /// DOM snapshot for link hints — **does not** lock [`VmuxHandler`]; safe while `inner` is held.
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
        let Some(browser) = Self::osr_browser_by_id(browser_id) else {
            return LinkHintsFeedOutcome::default();
        };
        Self::link_hints_read_session_with_browser(&browser)
    }

    /// `prior_typed_len`: Rust hint prefix length **before** this key (see [`OsrVimMachine::link_hints_typed_prefix`]).
    fn link_hints_finalize_feed_outcome(
        snap: LinkHintsFeedOutcome,
        browser_id: i32,
        prior_typed_len: usize,
    ) -> LinkHintsFeedOutcome {
        let w_from_dom = snap.hint_label_width.max(1);
        if snap.still_active {
            if let Some(h) = Self::instance() {
                if let Ok(mut inner) = h.lock() {
                    inner
                        .osr_link_hints_label_width
                        .insert(browser_id, w_from_dom);
                }
            }
            return LinkHintsFeedOutcome {
                still_active: true,
                hint_label_width: w_from_dom,
            };
        }
        let w_cached = Self::instance()
            .and_then(|h| h.lock().ok().and_then(|inner| {
                inner.osr_link_hints_label_width.get(&browser_id).copied()
            }))
            .unwrap_or(2)
            .max(1);
        let still = prior_typed_len > 0 && prior_typed_len + 1 < w_cached as usize;
        LinkHintsFeedOutcome {
            still_active: still,
            hint_label_width: w_cached,
        }
    }

    fn link_hints_feed_key_on_ui(browser_id: i32, ch: char, prior_typed_len: usize) -> LinkHintsFeedOutcome {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        if !ch.is_ascii_lowercase() {
            return Self::link_hints_read_session_on_ui(browser_id);
        }
        let Some(browser) = Self::osr_browser_by_id(browser_id) else {
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
        cef_pump::pump(12);
        if let Some(h) = Self::instance() {
            if let Ok(inner) = h.lock() {
                inner.request_osr_window_redraw_for_browser(browser_id);
            }
        }
        let snap = Self::link_hints_read_session_with_browser(&browser);
        Self::link_hints_finalize_feed_outcome(snap, browser_id, prior_typed_len)
    }

    /// Feed one hint letter (UI thread only). From the winit thread use [`Self::link_hints_feed_key_deferred`].
    pub fn link_hints_feed_key(
        browser_id: i32,
        ch: char,
        prior_typed_len: usize,
    ) -> LinkHintsFeedOutcome {
        let thread_id = ThreadId::UI;
        if currently_on(thread_id) != 0 {
            return Self::link_hints_feed_key_on_ui(browser_id, ch, prior_typed_len);
        }
        Self::link_hints_feed_key_deferred(browser_id, ch, prior_typed_len);
        LinkHintsFeedOutcome {
            still_active: true,
            ..Default::default()
        }
    }

    fn reload_osr_browser_on_ui(browser_id: i32) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let Some(handler) = Self::instance() else {
            return;
        };
        let attach = handler.lock().ok().and_then(|h| h.osr_attach.clone());
        let from_store = attach.as_ref().and_then(|a| {
            a.windows_store.lock().ok().and_then(|ws| {
                ws.values()
                    .find(|e| e.browser.identifier() == browser_id)
                    .map(|e| e.browser.clone())
            })
        });
        let browser = from_store.or_else(|| {
            handler.lock().ok().and_then(|inner| {
                inner
                    .browser_list
                    .iter()
                    .find(|b| b.identifier() == browser_id)
                    .cloned()
            })
        });
        let Some(browser) = browser else {
            return;
        };
        browser.reload();
        let bid = browser.identifier();
        let hub = crate::shared::vmux_osr::bootstrap::hub();
        hub.reset_paint_redraw_throttle(bid);
        cef_pump::pump(24);
        if let Ok(inner) = handler.lock() {
            inner.request_osr_window_redraw_for_browser(bid);
        }
    }

    pub fn navigate_active(go_forward: bool) {
        let Some(handler) = Self::instance() else {
            return;
        };
        let Some(browser_id) = handler.lock().ok().and_then(|inner| {
            inner
                .active_browser_id
                .or_else(|| inner.browser_list.first().map(|b| b.identifier()))
        }) else {
            return;
        };
        Self::navigate_osr_browser(browser_id, go_forward);
    }

    fn navigate_osr_browser_on_ui(browser_id: i32, go_forward: bool) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let Some(handler) = Self::instance() else {
            return;
        };
        let attach = handler.lock().ok().and_then(|h| h.osr_attach.clone());
        let from_store = attach.as_ref().and_then(|a| {
            a.windows_store.lock().ok().and_then(|ws| {
                ws.values()
                    .find(|e| e.browser.identifier() == browser_id)
                    .map(|e| e.browser.clone())
            })
        });
        let browser = from_store.or_else(|| {
            handler.lock().ok().and_then(|inner| {
                inner
                    .browser_list
                    .iter()
                    .find(|b| b.identifier() == browser_id)
                    .cloned()
            })
        });
        let Some(browser) = browser else {
            return;
        };
        apply_osr_history_navigation(&browser, go_forward);
        if let Some(host) = browser.host() {
            host.set_focus(1);
        }
        let bid = browser.identifier();
        let hub = crate::shared::vmux_osr::bootstrap::hub();
        hub.reset_paint_redraw_throttle(bid);
        osr_repaint_full_geometry(&browser);
        cef_pump::pump(10);
        hub.reset_paint_redraw_throttle(bid);
        osr_repaint_full_geometry(&browser);
        cef_pump::pump(10);
        for _ in 0..3 {
            if let Ok(inner) = handler.lock() {
                inner.request_osr_window_redraw_for_browser(bid);
            }
            cef_pump::pump(4);
        }
        Self::osr_navigation_repaint_pass_on_ui(bid);
        if let Ok(mut inner) = handler.lock() {
            // So the next `on_address_change` is not skipped as "unchanged" vs `last_osr_address_url`.
            inner.last_osr_address_url.remove(&bid);
            inner.sync_osr_title_from_visible_navigation(&browser);
            // Skip `nudge_osr_compositor_after_address_change`: we already ran two full repaints and
            // an extra repaint pass; another `was_resized`/`invalidate` pass dominated history navigation latency.
            inner.request_osr_window_redraw_for_browser(bid);
        }
        cef_pump::pump(3);
        let mut delayed = OsrDelayedNavigationRepaint::new(bid);
        let _ = post_delayed_task(ThreadId::UI, Some(&mut delayed), 75);
    }

    fn osr_navigation_repaint_pass_on_ui(browser_id: i32) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        let Some(handler) = Self::instance() else {
            return;
        };
        let browser = handler
            .lock()
            .ok()
            .and_then(|inner| {
                inner
                    .browser_list
                    .iter()
                    .find(|b| b.identifier() == browser_id)
                    .cloned()
            });
        let Some(browser) = browser else {
            return;
        };
        crate::shared::vmux_osr::bootstrap::hub().reset_paint_redraw_throttle(browser_id);
        osr_repaint_full_geometry(&browser);
        cef_pump::pump(14);
        if let Ok(inner) = handler.lock() {
            inner.request_osr_window_redraw_for_browser(browser_id);
        }
    }

    fn request_osr_window_redraw_for_browser(&self, browser_id: i32) {
        let Some(ref attach) = self.osr_attach else {
            return;
        };
        let Some(wid) =
            crate::shared::vmux_osr::bootstrap::hub().window_id_for_browser(browser_id)
        else {
            return;
        };
        if let Ok(mut ws) = attach.windows_store.lock() {
            if let Some(entry) = ws.get_mut(&wid) {
                entry.surface.window.request_redraw();
            }
        }
    }

    /// Push winit window title from `visible_navigation_entry` (reliable after BFCache back/forward).
    fn sync_osr_title_from_visible_navigation(&mut self, browser: &Browser) {
        if self.osr_attach.is_none() {
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
            crate::shared::vmux_osr::titles::push_title(bid, title_str);
        }
    }

    fn nudge_osr_compositor_after_address_change(&mut self, browser: &Browser) {
        if self.osr_attach.is_none() {
            return;
        }
        let bid = browser.identifier();
        crate::shared::vmux_osr::bootstrap::hub().reset_paint_redraw_throttle(bid);
        osr_repaint_full_geometry(browser);
        self.request_osr_window_redraw_for_browser(bid);
        cef_pump::pump(4);
    }

    fn on_osr_address_changed(&mut self, browser: &Browser, url: Option<&CefString>) {
        if self.osr_attach.is_none() {
            return;
        }
        let bid = browser.identifier();
        let url_str = url.map(CefString::to_string).unwrap_or_default();
        let changed = self
            .last_osr_address_url
            .get(&bid)
            .map(|p| p != &url_str)
            .unwrap_or(true);
        self.last_osr_address_url.insert(bid, url_str);
        self.sync_osr_title_from_visible_navigation(browser);
        if changed {
            self.nudge_osr_compositor_after_address_change(browser);
            // SPAs often call `pushState` / update the visible URL on unrelated activity. That used
            // to queue hint invalidation here; the next winit key event applied it *before* handling
            // the second hint letter, so Rust dropped `LinkHints` while the overlay was still up.
            //
            // If the hint marker is still on `<html>`, keep the session; real navigations replace
            // the document and the probe goes false (invalidate then). When not on the UI thread,
            // fall back to always invalidating.
            // Must not call `link_hints_probe_active_on_ui` here: it locks `VmuxHandler` while we
            // already hold `inner` from the display handler → deadlock / abort.
            let overlay_likely_up = currently_on(ThreadId::UI) != 0
                && Self::link_hints_read_session_with_browser(browser).still_active;
            if !overlay_likely_up {
                crate::shared::vmux_osr::bootstrap::hub().invalidate_link_hints_for_browser(bid);
            }
        } else {
            // Same URL string again (redirect noise, BFCache, etc.) — still schedule a frame; OSR can
            // otherwise keep presenting an old texture after in-session navigations.
            self.request_osr_window_redraw_for_browser(bid);
            if let Some(host) = browser.host() {
                #[cfg(all(
                    any(target_os = "macos", target_os = "windows", target_os = "linux"),
                    feature = "accelerated_osr"
                ))]
                host.send_external_begin_frame();
            }
            cef_pump::pump(4);
        }
    }

    /// Call from the winit `CloseRequested` path **before** `try_close_browser` so `do_close` can
    /// return false (allow CEF to destroy this browser) without treating the request as spurious.
    pub fn arm_windowless_close_from_winit() {
        let Some(handler) = Self::instance() else {
            return;
        };
        let Ok(mut inner) = handler.lock() else {
            return;
        };
        inner.allow_next_windowless_do_close = true;
    }

    fn on_title_change(&mut self, browser: Option<&mut Browser>, title: Option<&CefString>) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);

        let mut browser = browser.cloned();
        let browser_id = browser.as_ref().map(|b| b.identifier());
        if let Some(browser_view) = browser_view_get_for_browser(browser.as_mut()) {
            if let Some(window) = browser_view.window() {
                window.set_title(title);
                return;
            }
        }
        if let (Some(id), Some(t)) = (browser_id, title) {
            crate::shared::vmux_osr::titles::push_title(id, t.to_string());
        }
        platform_title_change(browser.as_mut(), title);
        // History / BFCache: title updates when the active entry changes, often before the next
        // `RedrawRequested` picks up a new OSR texture — nudge CEF + winit so the page catches up.
        if self.osr_attach.is_some() {
            if let Some(ref b) = browser {
                let bid = b.identifier();
                if let Some(host) = b.host() {
                    host.invalidate(PaintElementType::default());
                    #[cfg(all(
                        any(target_os = "macos", target_os = "windows", target_os = "linux"),
                        feature = "accelerated_osr"
                    ))]
                    host.send_external_begin_frame();
                }
                self.request_osr_window_redraw_for_browser(bid);
                cef_pump::pump(4);
            }
        }
    }

    fn on_after_created(&mut self, browser: Option<&mut Browser>) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);
        launch_trace("on_after_created: entered (CEF UI thread)");

        let browser = browser.cloned().expect("Browser is None");
        launch_trace(&format!(
            "on_after_created: browser_id={}",
            browser.identifier()
        ));

        // Sanity-check the configured runtime style.
        assert_eq!(
            browser.host().expect("BrowserHost is None").runtime_style(),
            RuntimeStyle::ALLOY
        );

        if let Some(ref attach) = self.osr_attach {
            self.browser_list.push(browser.clone());
            let shell = attach
                .shell_fifo
                .lock()
                .expect("vmux shell_fifo")
                .pop_front();
            let Some(shell) = shell else {
                launch_trace("FATAL: on_after_created: no pending OSR shell (fifo empty)");
                std::process::exit(1);
            };
            let browser_id = browser.identifier();
            let wid = shell.surface.window.id();
            let size = crate::shared::vmux_osr::bootstrap::hub().register_browser(
                browser_id,
                wid,
                shell.logical,
            );
            let mut ws = attach.windows_store.lock().expect("vmux windows_store");
            ws.insert(
                wid,
                WindowEntry {
                    surface: shell.surface,
                    browser,
                    size,
                },
            );
            if let Some(entry) = ws.get(&wid) {
                entry.surface.window.request_redraw();
            }
            attach.unpaired_osr_shells.fetch_sub(1, Ordering::Release);
            launch_trace("on_after_created: OSR shell attached to browser");
        } else {
            self.browser_list.push(browser);
        }
    }

    /// CEF / C++: `false` (0) = proceed with close; for windowless, that destroys the browser
    /// immediately. `true` (non-zero) = cancel / defer (non-standard owner window).
    fn do_close(&mut self, _browser: Option<&mut Browser>) -> i32 {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);

        if self.osr_attach.is_some() {
            let allow = self.bypass_windowless_do_close_guard || self.allow_next_windowless_do_close;
            if self.allow_next_windowless_do_close {
                self.allow_next_windowless_do_close = false;
            }
            if !allow {
                launch_trace("do_close: OSR cancel (no winit/force arm)");
                return 1;
            }
            if self.browser_list.len() == 1 {
                self.is_closing = true;
            }
            launch_trace("do_close: OSR allow destroy");
            return 0;
        }

        if self.browser_list.len() == 1 {
            self.is_closing = true;
        }
        0
    }

    fn on_before_close(&mut self, browser: Option<&mut Browser>) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);

        // Remove from the list of existing browsers.
        let mut browser = browser.cloned().expect("Browser is None");
        let removed_id = browser.identifier();
        self.osr_editable_focus_hint.remove(&removed_id);
        self.last_osr_address_url.remove(&removed_id);
        let wid_for_removed = if self.osr_attach.is_some() {
            crate::shared::vmux_osr::bootstrap::hub().window_id_for_browser(removed_id)
        } else {
            None
        };
        if let Some(index) = self
            .browser_list
            .iter()
            .position(move |elem| elem.is_same(Some(&mut browser)) != 0)
        {
            self.browser_list.remove(index);
        }
        crate::shared::vmux_osr::bootstrap::hub().unregister_browser(removed_id);

        if let (Some(attach), Some(wid)) = (self.osr_attach.as_ref(), wid_for_removed) {
            let removed = attach
                .windows_store
                .lock()
                .expect("vmux windows_store")
                .remove(&wid);
            if removed.is_some() {
                launch_trace(&format!(
                    "on_before_close: dropped OSR WindowEntry for browser_id={removed_id} (winit window closes with entry)"
                ));
            }
        }

        if self.browser_list.is_empty() {
            if let Some(ref attach) = self.osr_attach {
                let ws_empty = attach
                    .windows_store
                    .lock()
                    .expect("vmux windows_store")
                    .is_empty();
                let fifo_empty = attach
                    .shell_fifo
                    .lock()
                    .expect("vmux shell_fifo")
                    .is_empty();
                let unpaired = attach.unpaired_osr_shells.load(Ordering::Acquire);
                if !ws_empty || !fifo_empty || unpaired > 0 {
                    launch_trace(&format!(
                        "on_before_close: skip shutdown (OSR active: store_empty={ws_empty} fifo_empty={fifo_empty} unpaired={unpaired})"
                    ));
                    return;
                }
            }
            // All browsers are gone; safe to clear any OSR close bypass.
            self.bypass_windowless_do_close_guard = false;
            launch_trace("on_before_close: browser list empty, setting shutdown (no quit_message_loop here)");
            if let Some(flag) = crate::shared::vmux_osr::shutdown::shutdown_flag() {
                flag.store(true, Ordering::Release);
            }
            // Do not call `quit_message_loop()` here: with AppKit + winit `pump_app_events`, that can
            // tear down the NS run loop and make the window vanish immediately. We call it once in
            // `run_main` immediately before `cef::shutdown()`.
        }
    }

    fn on_load_error(
        &mut self,
        _browser: Option<&mut Browser>,
        frame: Option<&mut Frame>,
        error_code: Errorcode,
        error_text: Option<&CefString>,
        failed_url: Option<&CefString>,
    ) {
        debug_assert_ne!(currently_on(ThreadId::UI), 0);

        // Don't display an error for downloaded files.
        let error_code = sys::cef_errorcode_t::from(error_code);
        if error_code == sys::cef_errorcode_t::ERR_ABORTED {
            return;
        }
        let error_code = error_code as i32;

        let frame = frame.expect("Frame is None");

        // Display a load error message using a data: URI.
        let error_text = error_text.map(CefString::to_string).unwrap_or_default();
        let failed_url = failed_url.map(CefString::to_string).unwrap_or_default();
        let data = format!(
            r#"
            <html>
                <body bgcolor="white">
                    <h2>Failed to load URL {failed_url} with error {error_text} ({error_code}).</h2>
                </body>
            </html>
            "#
        );

        let uri = get_data_uri(data.as_bytes(), "text/html");
        let uri = CefString::from(uri.as_str());
        frame.load_url(Some(&uri));
    }

    pub fn show_main_window(&mut self) {
        let thread_id = ThreadId::UI;
        if currently_on(thread_id) == 0 {
            // Execute on the UI thread.
            let this = self
                .weak_self
                .upgrade()
                .expect("Weak reference to VmuxHandler is None");
            let mut task = ShowMainWindow::new(this);
            post_task(thread_id, Some(&mut task));
            return;
        }

        let Some(mut main_browser) = self.browser_list.first().cloned() else {
            return;
        };

        if let Some(browser_view) = browser_view_get_for_browser(Some(&mut main_browser)) {
            // Show the window using the Views framework.
            if let Some(window) = browser_view.window() {
                window.show();
            }
        } else {
            crate::shared::vmux_osr::show_all_windows();
            platform_show_window(Some(&mut main_browser));
        }
    }

    /// Request close on every tracked browser. Does **not** hold the handler mutex while
    /// calling `close_browser`: CEF may synchronously invoke `LifeSpanHandler::do_close`,
    /// which locks the same mutex (deadlock if we kept the lock — e.g. Cmd+Q on macOS).
    pub fn close_all_browsers(handler: &Arc<Mutex<Self>>, force_close: bool) {
        let thread_id = ThreadId::UI;
        if currently_on(thread_id) == 0 {
            let h = handler.clone();
            let mut task = CloseAllBrowsers::new(h, force_close);
            post_task(thread_id, Some(&mut task));
            return;
        }

        let (browsers, has_osr): (Vec<Browser>, bool) = {
            let mut inner = handler.lock().expect("Failed to lock VmuxHandler");
            if inner.is_closing {
                return;
            }
            let has_osr = inner.osr_attach.is_some();
            if has_osr {
                // When quitting (Cmd+Q / terminate:), `close_browser()` may lead to `do_close`
                // being evaluated slightly later. Keep this bypass enabled until the last browser
                // is actually closed; otherwise the OSR `do_close` guard can cancel the first quit
                // attempt, requiring Cmd+Q twice.
                inner.bypass_windowless_do_close_guard = true;
                if force_close {
                    inner.is_closing = true;
                }
            }
            (inner.browser_list.clone(), has_osr)
        };

        for browser in browsers {
            if let Some(browser_host) = browser.host() {
                browser_host.close_browser(force_close.into());
            }
        }

        // For OSR we leave `bypass_windowless_do_close_guard` enabled while quitting; it will be
        // reset when the last browser closes in `on_before_close`.
        if has_osr && !force_close {
            if let Ok(mut inner) = handler.lock() {
                inner.bypass_windowless_do_close_guard = false;
            }
        }
    }

    pub fn is_closing(&self) -> bool {
        self.is_closing
    }
}

wrap_client! {
    pub struct VmuxHandlerClient {
        inner: Arc<Mutex<VmuxHandler>>,
        render: RenderHandler,
    }

    impl Client {
        fn render_handler(&self) -> Option<RenderHandler> {
            Some(self.render.clone())
        }

        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(VmuxHandlerDisplayHandler::new(self.inner.clone()))
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(VmuxHandlerLifeSpanHandler::new(self.inner.clone()))
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            Some(VmuxHandlerLoadHandler::new(self.inner.clone()))
        }
    }
}

wrap_display_handler! {
    struct VmuxHandlerDisplayHandler {
        inner: Arc<Mutex<VmuxHandler>>,
    }

    impl DisplayHandler {
        fn on_address_change(
            &self,
            browser: Option<&mut Browser>,
            _frame: Option<&mut Frame>,
            url: Option<&CefString>,
        ) {
            let Some(browser) = browser.cloned() else {
                return;
            };
            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.on_osr_address_changed(&browser, url);
        }

        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.on_title_change(browser, title);
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
            let attach = {
                let Ok(inner) = self.inner.lock() else {
                    return 0;
                };
                inner.osr_attach.clone()
            };
            let Some(attach) = attach else {
                return 0;
            };
            let bid = browser.identifier();
            let Some(wid) = crate::shared::vmux_osr::bootstrap::hub().window_id_for_browser(bid)
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
                .set_cursor(vmux_osr_cursor_from_cef(type_));
            1
        }
    }
}

wrap_life_span_handler! {
    struct VmuxHandlerLifeSpanHandler {
        inner: Arc<Mutex<VmuxHandler>>,
    }

    impl LifeSpanHandler {
        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.on_after_created(browser);
        }

        fn do_close(&self, browser: Option<&mut Browser>) -> i32 {
            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.do_close(browser)
        }

        fn on_before_close(&self, browser: Option<&mut Browser>) {
            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.on_before_close(browser);
        }
    }
}

wrap_load_handler! {
    struct VmuxHandlerLoadHandler {
        inner: Arc<Mutex<VmuxHandler>>,
    }

    impl LoadHandler {
        fn on_load_error(
            &self,
            browser: Option<&mut Browser>,
            frame: Option<&mut Frame>,
            error_code: Errorcode,
            error_text: Option<&CefString>,
            failed_url: Option<&CefString>,
        ) {
            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.on_load_error(browser, frame, error_code, error_text, failed_url);
        }

        fn on_loading_state_change(
            &self,
            browser: Option<&mut Browser>,
            is_loading: std::os::raw::c_int,
            _can_go_back: std::os::raw::c_int,
            _can_go_forward: std::os::raw::c_int,
        ) {
            let Some(browser) = browser.cloned() else {
                return;
            };
            let bid = browser.identifier();
            let needs_osr = self
                .inner
                .lock()
                .ok()
                .is_some_and(|inner| inner.osr_attach.is_some());
            if !needs_osr {
                return;
            }
            if is_loading != 0 {
                crate::shared::vmux_osr::bootstrap::hub().invalidate_link_hints_for_browser(bid);
                if let Some(host) = browser.host() {
                    host.invalidate(PaintElementType::VIEW);
                    #[cfg(all(
                        any(target_os = "macos", target_os = "windows", target_os = "linux"),
                        feature = "accelerated_osr"
                    ))]
                    host.send_external_begin_frame();
                }
                if let Ok(inner) = self.inner.lock() {
                    inner.request_osr_window_redraw_for_browser(bid);
                }
                cef_pump::pump(5);
                return;
            }
            osr_repaint_full_geometry(&browser);
            if let Ok(mut inner) = self.inner.lock() {
                inner
                    .osr_editable_focus_hint
                    .insert(bid, Some(false));
                inner.request_osr_window_redraw_for_browser(bid);
            }
            VmuxHandler::schedule_osr_editable_focus_probe(bid);
        }
    }
}

wrap_domvisitor! {
    struct LinkHintsSessionDomVisitor {
        out: Arc<Mutex<LinkHintsFeedOutcome>>,
    }

    impl Domvisitor {
        fn visit(&self, document: Option<&mut Domdocument>) {
            use ImplDomdocument as _;
            use ImplDomnode as _;
            let (active, width) = match document {
                None => (false, 1u8),
                Some(doc) => doc
                    .document()
                    .map(|root| {
                        let hints = CefString::from("data-vmux-hints");
                        let active = root.has_element_attribute(Some(&hints)) != 0;
                        let width = if active {
                            let raw = root.element_attribute(Some(&hints));
                            let s = CefStringUtf8::from(&CefStringUtf16::from(&raw))
                                .to_string();
                            s.trim()
                                .parse::<u8>()
                                .unwrap_or(2)
                                .clamp(1, 32)
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

wrap_domvisitor! {
    struct VmuxEditableFocusDomVisitor {
        browser_id: i32,
        handler: Arc<Mutex<VmuxHandler>>,
    }

    impl Domvisitor {
        fn visit(&self, document: Option<&mut Domdocument>) {
            let editable = match document {
                None => false,
                Some(doc) => doc
                    .focused_node()
                    .map(dom_focused_context_allows_typing)
                    .unwrap_or(false),
            };
            if let Ok(mut inner) = self.handler.lock() {
                // A late probe must not clear `Some(true)` set by `send_char` / IME while the DOM
                // visitor still misses the control (e.g. custom search UIs). Only `invalidate` or a
                // successful `true` from the probe should establish typing; we never downgrade true→false here.
                let merged = match inner.osr_editable_focus_hint.get(&self.browser_id) {
                    Some(Some(true)) if !editable => true,
                    _ => editable,
                };
                inner
                    .osr_editable_focus_hint
                    .insert(self.browser_id, Some(merged));
            }
        }
    }
}

wrap_task! {
    struct ProbeOsrEditableFocus {
        handler: Arc<Mutex<VmuxHandler>>,
        browser_id: i32,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let _ = &self.handler;
            VmuxHandler::run_osr_editable_focus_probe_on_ui(self.browser_id);
        }
    }
}

wrap_task! {
    struct EditableProbeWakeTask {
        browser_id: i32,
        window_id: WindowId,
        event: KeyEvent,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            VmuxHandler::run_osr_editable_focus_probe_on_ui(self.browser_id);
            event_loop::event_loop().send(event_loop::VmuxUserEvent::VimKeyReplay {
                window_id: self.window_id,
                event: self.event.clone(),
            });
        }
    }
}

wrap_task! {
    struct NavigateOsrBrowser {
        inner: Arc<Mutex<VmuxHandler>>,
        browser_id: i32,
        go_forward: bool,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let _ = &self.inner;
            VmuxHandler::navigate_osr_browser_on_ui(self.browser_id, self.go_forward);
        }
    }
}

wrap_task! {
    struct ReloadOsrBrowser {
        inner: Arc<Mutex<VmuxHandler>>,
        browser_id: i32,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let _ = &self.inner;
            VmuxHandler::reload_osr_browser_on_ui(self.browser_id);
        }
    }
}

wrap_task! {
    struct LinkHintsRun {
        inner: Arc<Mutex<VmuxHandler>>,
        browser_id: i32,
        show: bool,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let _ = &self.inner;
            if self.show {
                VmuxHandler::link_hints_inject_on_ui(self.browser_id);
            } else {
                VmuxHandler::link_hints_clear_on_ui(self.browser_id);
            }
        }
    }
}

wrap_task! {
    struct LinkHintsFeedTask {
        inner: Arc<Mutex<VmuxHandler>>,
        browser_id: i32,
        ch: char,
        prior_typed_len: usize,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let _ = &self.inner;
            let v = VmuxHandler::link_hints_feed_key_on_ui(
                self.browser_id,
                self.ch,
                self.prior_typed_len,
            );
            let Some(wid) =
                crate::shared::vmux_osr::bootstrap::hub().window_id_for_browser(self.browser_id)
            else {
                return;
            };
            event_loop::event_loop().send(event_loop::VmuxUserEvent::LinkHintFeed {
                window_id: wid,
                browser_id: self.browser_id,
                ch: self.ch,
                prior_typed_len: self.prior_typed_len,
                still_active: v.still_active,
                hint_label_width: v.hint_label_width,
            });
        }
    }
}

wrap_task! {
    struct OsrDelayedNavigationRepaint {
        browser_id: i32,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            let Some(handler) = VmuxHandler::instance() else {
                return;
            };
            let attach = handler.lock().ok().and_then(|h| h.osr_attach.clone());
            let from_store = attach.as_ref().and_then(|a| {
                a.windows_store.lock().ok().and_then(|ws| {
                    ws.values()
                        .find(|e| e.browser.identifier() == self.browser_id)
                        .map(|e| e.browser.clone())
                })
            });
            let browser = from_store.or_else(|| {
                handler.lock().ok().and_then(|inner| {
                    inner
                        .browser_list
                        .iter()
                        .find(|b| b.identifier() == self.browser_id)
                        .cloned()
                })
            });
            let Some(browser) = browser else {
                return;
            };
            let bid = self.browser_id;
            crate::shared::vmux_osr::bootstrap::hub().reset_paint_redraw_throttle(bid);
            osr_repaint_full_geometry(&browser);
            cef_pump::pump(10);
            if let Ok(inner) = handler.lock() {
                inner.request_osr_window_redraw_for_browser(bid);
            }
        }
    }
}

wrap_task! {
    struct ShowMainWindow {
        inner: Arc<Mutex<VmuxHandler>>,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);

            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.show_main_window();
        }
    }
}

wrap_task! {
    struct CloseAllBrowsers {
        inner: Arc<Mutex<VmuxHandler>>,
        force_close: bool,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);

            VmuxHandler::close_all_browsers(&self.inner, self.force_close);
        }
    }
}
