use bevy_app::{App, Plugin};

pub struct VmuxPlugin;

impl VmuxPlugin {
    pub const fn new() -> Self {
        Self
    }
}

impl Plugin for VmuxPlugin {
    fn build(&self, app: &mut App) {
        // MVP: core OSR + Google only — omit `PanesPlugin` / `VimiumPlugin` until CEF path is stable again.
        // `VimiumStateResource` still comes from `WinitPlugin` for `with_osr_host_runtime_and_vimium` / dispatch.
        app.add_plugins(crate::settings::SettingsPlugin)
            .add_plugins(crate::browser::renderer::vmux_render::VmuxRenderPlugin)
            .add_plugins(crate::windows::WindowsPlugin)
            .add_plugins(crate::browser::CefPlugin::new())
            .add_plugins(crate::browser::BrowserPlugin::new());
    }
}
