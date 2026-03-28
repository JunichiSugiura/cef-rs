//! One Bevy [`Event`] per winit [`WindowEvent`] drained from [`crate::window::pending_window_events::PendingWindowEvents`].
//! [`crate::window::pending_window_events::apply_osr_host_window_dispatches_system`] runs after
//! [`crate::window::pending_window_events::ingest_winit_window_dispatches_system`] (both scheduled from
//! [`crate::browser::event_loop::WinitPlugin`]) and calls
//! [`crate::window::dispatch::handle_window_event`] for each dispatch in FIFO order.

use bevy_ecs::prelude::Event;
use winit::event::WindowEvent;
use winit::window::WindowId;

#[derive(Event, Clone, Debug)]
pub struct OsrHostWindowDispatch {
    pub window_id: WindowId,
    pub event: WindowEvent,
}
