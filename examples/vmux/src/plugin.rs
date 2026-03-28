use bevy_app::{App, Plugin};

pub struct VmuxPlugin;

impl VmuxPlugin {
    pub const fn new() -> Self {
        Self
    }
}

impl Plugin for VmuxPlugin {
    fn build(&self, app: &mut App) {
        // `VimiumStateResource` comes from `WinitPlugin` (via `WindowsPlugin`); `VimiumPlugin` registers
        // systems that consume `VimiumKeyReplayEvent` / find-mode queue — required after editable-focus defer.
        app.add_plugins(crate::settings::SettingsPlugin)
            .add_plugins(crate::browser::renderer::vmux_render::VmuxRenderPlugin)
            .add_plugins(crate::windows::WindowsPlugin)
            .add_plugins(crate::browser::CefPlugin::new())
            .add_plugins(crate::browser::BrowserPlugin::new())
            .add_plugins(crate::vimium::VimiumPlugin);
    }
}

#[cfg(test)]
mod tests {
    use bevy_app::App;

    use super::VmuxPlugin;
    use crate::vimium::VimiumPlugin;

    #[test]
    fn vmux_plugin_registers_vimium_plugin() {
        let mut app = App::new();
        app.add_plugins(VmuxPlugin::new());
        assert!(
            app.is_plugin_added::<VimiumPlugin>(),
            "deferred vimium keys post-probe are replayed only by VimiumPlugin systems"
        );
    }
}
