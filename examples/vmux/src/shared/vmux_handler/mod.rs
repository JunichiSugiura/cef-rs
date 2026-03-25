use cef::*;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::shared::launch_trace;
use crate::shared::vmux_osr::hub::{VmuxOsrAttach, WindowEntry};

fn get_data_uri(data: &[u8], mime_type: &str) -> String {
    let data = CefString::from(&base64_encode(Some(data)));
    let uri = CefString::from(&uriencode(Some(&data), 0)).to_string();
    format!("data:{mime_type};base64,{uri}")
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
    /// When set, each `browser_host_create_browser` is paired with a `PendingOsrWindow` in the fifo.
    osr_attach: Option<VmuxOsrAttach>,
    /// Windowless OSR: CEF destroys the browser immediately if `do_close` returns **false**. We only
    /// return false when the user closed the winit window (`arm_windowless_close_from_winit`) or
    /// during `close_all_browsers` (`bypass_windowless_do_close_guard`). Otherwise return **true**
    /// to cancel spurious close requests (stops the window flashing away on startup).
    allow_next_windowless_do_close: bool,
    bypass_windowless_do_close_guard: bool,
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
                osr_attach,
                allow_next_windowless_do_close: false,
                bypass_windowless_do_close_guard: false,
            })
        })
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
        fn on_title_change(&self, browser: Option<&mut Browser>, title: Option<&CefString>) {
            let mut inner = self.inner.lock().expect("Failed to lock inner");
            inner.on_title_change(browser, title);
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
