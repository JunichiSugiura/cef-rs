use bevy_ecs::prelude::Resource;

use crate::panes::core::PaneId;

#[derive(Resource, Debug, Default, Clone, Copy)]
pub struct FocusedPane(pub Option<PaneId>);
