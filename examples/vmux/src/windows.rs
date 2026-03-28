//! Window/session domain plugin entrypoint.

pub mod focus;
pub mod layout;
pub mod session;

pub use crate::browser::event_loop::{
    build_event_loop, register_winit_proxy_for_foreign_callbacks, run_winit, CefPumpDeadline,
    ShutdownFlag, SignalQuitFlag,
};
pub use crate::window::registry::{
    show_all_windows, track_window, WindowComponent, WindowRegistryPlugin,
};

use bevy_app::{App, Plugin};

pub struct WindowsPlugin;

impl Plugin for WindowsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(crate::browser::WinitPlugin::new())
            .add_plugins(WindowRegistryPlugin)
            .init_resource::<session::SessionState>()
            .init_resource::<focus::FocusedPane>()
            .init_resource::<layout::DefaultSplitAxis>();
    }
}
