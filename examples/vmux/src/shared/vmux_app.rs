//! **vmux** uses **off-screen rendering (OSR)** + **winit** + **wgpu** (see `vmux_osr`).
use cef::*;
use std::cell::RefCell;
use std::rc::Rc;

wrap_app! {
    pub struct VmuxApp {
        client_holder: Rc<RefCell<Option<Client>>>,
    }

    impl App {
        fn on_before_command_line_processing(
            &self,
            _process_type: Option<&CefStringUtf16>,
            command_line: Option<&mut CommandLine>,
        ) {
            let Some(command_line) = command_line else {
                return;
            };
            command_line.append_switch(Some(&"no-startup-window".into()));
            command_line.append_switch(Some(&"noerrdialogs".into()));
            command_line.append_switch(Some(&"hide-crash-restore-bubble".into()));
            #[cfg(target_os = "macos")]
            command_line.append_switch(Some(&"use-mock-keychain".into()));
            #[cfg(all(
                any(target_os = "macos", target_os = "windows", target_os = "linux"),
                feature = "accelerated_osr",
            ))]
            {
                // Keeps accelerated OSR in sync with history; avoids constant repaint work from a
                // FrameHandler on the general browsing path.
                command_line.append_switch_with_value(
                    Some(&"disable-features".into()),
                    Some(&"BackForwardCache".into()),
                );
            }
        }

        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(VmuxBrowserProcessHandler::new(self.client_holder.clone()))
        }
    }
}

wrap_browser_process_handler! {
    pub struct VmuxBrowserProcessHandler {
        client_holder: Rc<RefCell<Option<Client>>>,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            // OSR client + wgpu are initialized in `shared::run_main` after `cef::initialize`
            // returns (process main thread), not here — see module doc on `bootstrap::init_client_with_osr`.
        }

        fn default_client(&self) -> Option<Client> {
            self.client_holder.borrow().clone()
        }
    }
}
