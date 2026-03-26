//! Custom winit **user event** for the vmux shell: one enum, delivered with
//! [`VmuxEventLoop::send`] → `EventLoopProxy::send_event` → [`super::VmuxOsrApp::user_event`].

use std::sync::OnceLock;

use winit::event::KeyEvent;
use winit::event_loop::EventLoopProxy;
use winit::window::WindowId;

/// Single enum for vmux → winit application-handler delivery (not a separate command queue).
#[derive(Debug, Clone)]
pub enum VmuxUserEvent {
    /// Re-dispatch a key after `run_osr_editable_focus_probe_on_ui` has updated the hint map.
    VimKeyReplay {
        window_id: WindowId,
        event: KeyEvent,
    },
    /// Apply link-hint feed outcome (from [`crate::shared::vmux_handler::VmuxHandler::link_hints_feed_key_on_ui`]).
    LinkHintFeed {
        window_id: WindowId,
        browser_id: i32,
        ch: char,
        prior_typed_len: usize,
        still_active: bool,
        hint_label_width: u8,
    },
}

/// Handle returned by [`event_loop()`][`fn@event_loop`]; forwards to the process-wide proxy.
#[derive(Clone, Copy, Debug, Default)]
pub struct VmuxEventLoop;

/// Entry point for posting [`VmuxUserEvent`] to the running winit loop.
pub fn event_loop() -> VmuxEventLoop {
    VmuxEventLoop
}

impl VmuxEventLoop {
    /// Deliver one user event to the shell (`ApplicationHandler::user_event`).
    pub fn send(self, event: VmuxUserEvent) {
        if let Some(p) = proxy() {
            let _ = p.send_event(event);
        }
    }
}

static EVENT_LOOP_PROXY: OnceLock<EventLoopProxy<VmuxUserEvent>> = OnceLock::new();

pub fn init(proxy: EventLoopProxy<VmuxUserEvent>) {
    let _ = EVENT_LOOP_PROXY.set(proxy);
}

fn proxy() -> Option<&'static EventLoopProxy<VmuxUserEvent>> {
    EVENT_LOOP_PROXY.get()
}
