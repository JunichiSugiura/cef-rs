use std::collections::HashMap;

use bevy_app::{App, Plugin, Update};
use bevy_ecs::prelude::{EventReader, ResMut, Resource};

use crate::panes::core::{Pane, PaneId};
use crate::panes::events::{PaneCloseRequest, PaneSpawnRequest};

#[derive(Resource, Default)]
pub struct PaneRegistry {
    pub panes: HashMap<PaneId, Pane>,
}

pub struct PaneRegistryPlugin;

impl Plugin for PaneRegistryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PaneRegistry>().add_systems(
            Update,
            (track_spawn_requests_system, track_close_requests_system),
        );
    }
}

fn track_spawn_requests_system(
    mut spawn: EventReader<PaneSpawnRequest>,
    mut registry: ResMut<PaneRegistry>,
) {
    for event in spawn.read() {
        registry.panes.insert(event.pane_id, event.pane);
    }
}

fn track_close_requests_system(
    mut close: EventReader<PaneCloseRequest>,
    mut registry: ResMut<PaneRegistry>,
) {
    for event in close.read() {
        registry.panes.remove(&event.pane_id);
    }
}
