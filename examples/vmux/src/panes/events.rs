use bevy_ecs::event::Event;

use crate::panes::core::{Pane, PaneId};

#[derive(Event, Debug, Clone, Copy)]
pub struct PaneSpawnRequest {
    pub pane_id: PaneId,
    pub pane: Pane,
}

#[derive(Event, Debug, Clone, Copy)]
pub struct PaneCloseRequest {
    pub pane_id: PaneId,
}

#[derive(Event, Debug, Clone, Copy)]
pub struct PaneFocusRequest {
    pub pane_id: PaneId,
}
