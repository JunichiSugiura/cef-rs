use cef::*;
use std::cell::RefCell;

use super::vmux_handler::*;

wrap_window_delegate! {
    struct VmuxWindowDelegate {
        browser_view: RefCell<Option<BrowserView>>,
        initial_show_state: ShowState,
        window_slot: Option<usize>,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            Size {
                width: 800,
                height: 600,
            }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            // Add the browser view and show the window.
            let browser_view = self.browser_view.borrow();
            let (Some(window), Some(browser_view)) = (window, browser_view.as_ref()) else {
                return;
            };
            let mut view = View::from(browser_view);
            window.add_child_view(Some(&mut view));

            if self.initial_show_state != ShowState::HIDDEN {
                window.show();
            }
        }

        fn on_window_destroyed(&self, _window: Option<&mut Window>) {
            let mut browser_view = self.browser_view.borrow_mut();
            *browser_view = None;
        }

        fn can_close(&self, _window: Option<&mut Window>) -> i32 {
            // Allow the window to close if the browser says it's OK.
            let browser_view = self.browser_view.borrow();
            let browser_view = browser_view.as_ref().expect("BrowserView is None");
            if let Some(browser) = browser_view.browser() {
                let browser_host = browser.host().expect("BrowserHost is None");
                browser_host.try_close_browser()
            } else {
                1
            }
        }

        fn initial_show_state(&self, _window: Option<&mut Window>) -> ShowState {
            self.initial_show_state
        }

        fn initial_bounds(&self, _window: Option<&mut Window>) -> Rect {
            if let Some(slot) = self.window_slot {
                let width = 700;
                let height = 1000;
                let x = if slot % 2 == 0 { 0 } else { width };
                return Rect {
                    x,
                    y: 0,
                    width,
                    height,
                };
            }

            Rect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            }
        }

        fn window_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

wrap_browser_view_delegate! {
    struct VmuxBrowserViewDelegate {
        runtime_style: RuntimeStyle,
    }

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        fn on_popup_browser_view_created(
            &self,
            _browser_view: Option<&mut BrowserView>,
            popup_browser_view: Option<&mut BrowserView>,
            _is_devtools: i32,
        ) -> i32 {
            // Create a new top-level Window for the popup. It will show itself after
            // creation.
            let mut window_delegate = VmuxWindowDelegate::new(
                RefCell::new(popup_browser_view.cloned()),
                ShowState::NORMAL,
                None,
            );
            window_create_top_level(Some(&mut window_delegate));

            // We created the Window.
            1
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            self.runtime_style
        }
    }
}

wrap_app! {
    pub struct VmuxApp;

    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(VmuxBrowserProcessHandler::new(RefCell::new(None)))
        }
    }
}

wrap_browser_process_handler! {
    struct VmuxBrowserProcessHandler {
        client: RefCell<Option<Client>>,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);

            // Check if Alloy style will be used.
            let command_line = command_line_get_global().expect("Failed to get command line");
            let runtime_style = RuntimeStyle::ALLOY;

            {
                // VmuxHandler implements browser-level callbacks.
                let mut client = self.client.borrow_mut();
                *client = Some(VmuxHandlerClient::new(VmuxHandler::new()));
            }

            // Specify CEF browser settings here.
            let settings = BrowserSettings::default();

            // Always start both windows on Google.
            let url = CefString::from("https://www.google.com/");

            // Views is enabled by default (add `--use-native` to disable).
            let use_views = command_line.has_switch(Some(&CefString::from("use-native"))) == 0;

            // If using Views create the browser using the Views framework, otherwise
            // create the browser using the native platform framework.
            if use_views {
                // Create the BrowserView.
                let mut client = self.default_client();
                let mut delegate = VmuxBrowserViewDelegate::new(runtime_style);
                let browser_view = browser_view_create(
                    client.as_mut(),
                    Some(&url),
                    Some(&settings),
                    None,
                    None,
                    Some(&mut delegate),
                );
                // Optionally configure the initial show state.
                let initial_show_state = CefString::from(
                    &command_line.switch_value(Some(&CefString::from("initial-show-state"))),
                )
                .to_string();
                let initial_show_state = match initial_show_state.as_str() {
                    "minimized" => ShowState::MINIMIZED,
                    "maximized" => ShowState::MAXIMIZED,
                    // Hidden show state is only supported on MacOS.
                    #[cfg(target_os = "macos")]
                    "hidden" => ShowState::HIDDEN,
                    _ => ShowState::NORMAL,
                };

                // Create the Window. It will show itself after creation.
                let mut delegate = VmuxWindowDelegate::new(
                    RefCell::new(browser_view),
                    initial_show_state,
                    Some(0),
                );
                window_create_top_level(Some(&mut delegate));

                // Create a second startup window in the right tile.
                let mut second_client = self.default_client();
                let mut second_delegate = VmuxBrowserViewDelegate::new(runtime_style);
                let second_browser_view = browser_view_create(
                    second_client.as_mut(),
                    Some(&url),
                    Some(&settings),
                    None,
                    None,
                    Some(&mut second_delegate),
                );
                let mut second_window_delegate = VmuxWindowDelegate::new(
                    RefCell::new(second_browser_view),
                    ShowState::NORMAL,
                    Some(1),
                );
                window_create_top_level(Some(&mut second_window_delegate));
            } else {
                // Information used when creating the native window.
                let window_info = WindowInfo {
                    runtime_style,
                    ..Default::default()
                };

                #[cfg(target_os = "windows")]
                let window_info = window_info.set_as_popup(Default::default(), "vmux");

                let mut client = self.default_client();
                browser_host_create_browser(
                    Some(&window_info),
                    client.as_mut(),
                    Some(&url),
                    Some(&settings),
                    None,
                    None,
                );
            }
        }

        fn default_client(&self) -> Option<Client> {
            self.client.borrow().clone()
        }
    }
}
