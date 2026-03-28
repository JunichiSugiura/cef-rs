//! Pane domain contracts and plugin entrypoint.

pub mod core;
pub mod events;
pub mod registry;

pub use core::{BrowserPaneState, Pane, PaneId};
pub use registry::PaneRegistryPlugin;

use bevy_app::{App, Plugin};

pub struct PanesPlugin;

impl Plugin for PanesPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(PaneRegistryPlugin)
            .add_event::<events::PaneSpawnRequest>()
            .add_event::<events::PaneCloseRequest>()
            .add_event::<events::PaneFocusRequest>();
    }
}
