use std::collections::HashMap;

use bevy_ecs::prelude::Resource;
use winit::window::WindowId;

use crate::browser::browser_entity::{BrowserId, BrowserWindowId};

/// ECS mirror of `WindowId` → CEF `browser_id` for render-side reads (same info as the OSR host map, kept in sync each frame before redraw flush).
#[derive(Resource, Default, Clone)]
pub struct ExtractedOsrBrowserIds(pub HashMap<WindowId, i32>);

pub fn extract_osr_browser_ids(world: &mut bevy_ecs::world::World) {
    let mut map = HashMap::new();
    let mut q = world.query::<(&BrowserWindowId, &BrowserId)>();
    for (wid, bid) in q.iter(world) {
        map.insert(wid.0, bid.0);
    }
    world.insert_resource(ExtractedOsrBrowserIds(map));
}
