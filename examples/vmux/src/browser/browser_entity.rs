//! One Bevy [`Entity`] per embedded CEF browser, spawned on the main thread when CEF reports the browser ready.
//!
//! CEF callbacks run on the UI thread — they send [`crate::browser::event_loop::UserEvent`] to the
//! winit loop, which forwards into Bevy [`Events`]; [`apply_browser_spawn_events`] / [`apply_browser_despawn_events`]
//! run on `Update`.
//!
//! ## Three ways to resolve a browser by id
//!
//! - **`CefAttach.windows_store`** ([`crate::browser::backend::osr::hub::CefAttach`]): OSR **live** window
//!   and its paired [`cef::Browser`]; preferred for rendering and input that target the real shell.
//! - **`CefBrowserHandlesInner`** ([`CefBrowserHandles`]): **Foreign / UI-thread** map (mutex) updated in
//!   `BrowserHandler` CEF lifecycle callbacks (`on_after_created` / `on_before_close`) so code **without**
//!   a [`bevy_ecs::world::World`] can resolve [`cef::Browser`] by id. Also carries
//!   creation order (`order`) for fallbacks when no ECS “active” exists. Used with windowless paths.
//! - **ECS** ([`BrowserId`], [`CefBrowserHandle`], [`BrowserEntities`]): **identity** for shell/app logic on
//!   the main thread (`Query`, `Update` systems). [`CefBrowserHandle`] mirrors the handle once the entity
//!   exists (after [`BrowserSpawnEvent`]).
//!
//! [`crate::browser::events::cef_browser_by_id`] resolves `windows_store` first, then the mutex map.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use bevy_ecs::prelude::{Bundle, Commands, Component, Entity, Event, EventReader, ResMut, Resource};
use cef::{Browser, Rect, ScreenInfo};
use winit::dpi::LogicalSize;
use winit::window::WindowId;

use crate::browser::view_state::{EditableFocusHint, LastAddressUrl, LinkHintsLabelWidth};

/// CEF browser id for this instance (paired after `on_after_created`).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BrowserId(pub i32);

/// Winit window tied to this browser (after shell attach).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BrowserWindowId(pub WindowId);

/// CEF [`Browser`] handle for Bevy queries (mirrors [`CefBrowserHandlesInner`] on the UI thread).
#[derive(Component, Clone)]
pub struct CefBrowserHandle(pub Browser);

/// Shared `Arc` with [`crate::browser::backend::osr::foreign_index::ForeignOsrIndex`] tab slot and `WindowEntry::size`.
///
/// CEF [`RenderHandler::view_rect`](cef::RenderHandler::view_rect) / [`screen_info`](cef::RenderHandler::screen_info)
/// cannot use [`bevy_ecs::query::Query`]; they resolve the same `Arc` via [`crate::browser::backend::osr::foreign_index::with_tab`]
/// and call [`OsrViewLogicalSize::fill_cef_view_rect_from_arc`] / [`OsrViewLogicalSize::fill_cef_screen_rect_from_arc`].
#[derive(Component, Clone)]
pub struct OsrViewLogicalSize(pub Arc<Mutex<LogicalSize<f32>>>);

impl OsrViewLogicalSize {
    /// View size in **logical** units for CEF `view_rect` (matches ECS + foreign index).
    pub fn fill_cef_view_rect_from_arc(size: &Arc<Mutex<LogicalSize<f32>>>, rect: &mut Rect) {
        if let Ok(size) = size.lock() {
            rect.x = 0;
            rect.y = 0;
            let w = size.width.max(1.0).ceil() as i32;
            let h = size.height.max(1.0).ceil() as i32;
            rect.width = w;
            rect.height = h;
        }
    }

    /// Fills `screen_info.rect` / `available_rect` in **physical** pixels (`logical × device_scale_factor`).
    pub fn fill_cef_screen_rect_from_arc(
        size: &Arc<Mutex<LogicalSize<f32>>>,
        device_scale_factor: f32,
        screen_info: &mut ScreenInfo,
    ) {
        if let Ok(size) = size.lock() {
            let w_px = (size.width.max(1.0) * device_scale_factor).ceil() as i32;
            let h_px = (size.height.max(1.0) * device_scale_factor).ceil() as i32;
            screen_info.rect.x = 0;
            screen_info.rect.y = 0;
            screen_info.rect.width = w_px;
            screen_info.rect.height = h_px;
            screen_info.available_rect = screen_info.rect.clone();
        }
    }
}

/// Shared `Arc` with [`crate::browser::backend::osr::foreign_index::TabPaintSlot::bind_group`] (OSR texture bind group).
#[derive(Component, Clone)]
pub struct OsrPaintBindGroup(pub Arc<Mutex<Option<wgpu::BindGroup>>>);

/// Grouped per-browser ECS components (id + window + CEF handle).
#[derive(Bundle)]
pub struct BrowserBundle {
    pub browser_id: BrowserId,
    pub window_id: BrowserWindowId,
    pub cef_browser: CefBrowserHandle,
    pub osr_view_logical_size: OsrViewLogicalSize,
    pub osr_paint_bind_group: OsrPaintBindGroup,
    pub editable_focus_hint: EditableFocusHint,
    pub link_hints_label_width: LinkHintsLabelWidth,
    pub last_address_url: LastAddressUrl,
}

/// Shared registry: UI thread inserts/removes in [`crate::browser::BrowserHandler`] lifecycle; Bevy only
/// spawns/despawns entities from events.
#[derive(Default)]
pub struct CefBrowserHandlesInner {
    pub browsers: HashMap<i32, Browser>,
    pub order: Vec<i32>,
}

impl CefBrowserHandlesInner {
    pub fn len(&self) -> usize {
        self.browsers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.browsers.is_empty()
    }

    pub fn first_browser_id(&self) -> Option<i32> {
        self.order.first().copied()
    }

    pub fn get(&self, id: i32) -> Option<&Browser> {
        self.browsers.get(&id)
    }

    pub fn insert_spawn(&mut self, id: i32, browser: Browser) {
        if self.browsers.remove(&id).is_some() {
            self.order.retain(|&x| x != id);
        }
        self.browsers.insert(id, browser);
        self.order.push(id);
    }

    pub fn remove_despawn(&mut self, id: i32) -> Option<Browser> {
        let b = self.browsers.remove(&id);
        self.order.retain(|&x| x != id);
        b
    }

    pub fn values_cloned(&self) -> Vec<Browser> {
        self.browsers.values().cloned().collect()
    }
}

#[derive(Resource, Clone)]
pub struct CefBrowserHandles(pub Arc<Mutex<CefBrowserHandlesInner>>);

#[derive(Event, Clone)]
pub struct BrowserSpawnEvent {
    pub browser_id: i32,
    pub window_id: WindowId,
    pub browser: Browser,
    /// OSR only; same `Arc` as hub + `windows_store` entry. `None` for windowless spawn.
    pub osr_view_logical_size: Option<Arc<Mutex<LogicalSize<f32>>>>,
    /// OSR only; same `Arc` as [`ForeignOsrIndex`](crate::browser::backend::osr::foreign_index::ForeignOsrIndex) tab slot. `None` for windowless spawn.
    pub osr_paint_bind_group: Option<Arc<Mutex<Option<wgpu::BindGroup>>>>,
}

impl fmt::Debug for BrowserSpawnEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserSpawnEvent")
            .field("browser_id", &self.browser_id)
            .field("window_id", &self.window_id)
            .finish_non_exhaustive()
    }
}

#[derive(Event, Debug, Clone)]
pub struct BrowserDespawnEvent {
    pub browser_id: i32,
}

/// Maps CEF `browser_id` → Bevy entity for systems that need to jump from CEF ids to ECS.
#[derive(Resource, Default)]
pub struct BrowserEntities {
    pub by_browser_id: HashMap<i32, Entity>,
}

pub fn apply_browser_spawn_events(
    mut commands: Commands,
    mut events: EventReader<BrowserSpawnEvent>,
    mut index: ResMut<BrowserEntities>,
) {
    for ev in events.read() {
        if let Some(old) = index.by_browser_id.remove(&ev.browser_id) {
            commands.entity(old).despawn();
        }
        let osr_size = ev
            .osr_view_logical_size
            .clone()
            .map(OsrViewLogicalSize)
            .unwrap_or_else(|| OsrViewLogicalSize(Arc::new(Mutex::new(LogicalSize::new(1.0, 1.0)))));
        let osr_bind = ev
            .osr_paint_bind_group
            .clone()
            .map(OsrPaintBindGroup)
            .unwrap_or_else(|| OsrPaintBindGroup(Arc::new(Mutex::new(None))));
        let entity = commands
            .spawn(BrowserBundle {
                browser_id: BrowserId(ev.browser_id),
                window_id: BrowserWindowId(ev.window_id),
                cef_browser: CefBrowserHandle(ev.browser.clone()),
                osr_view_logical_size: osr_size,
                osr_paint_bind_group: osr_bind,
                // Optimistic "browse" until a probe / UI path updates. Avoids Vimium seeing `None`
                // when `SetEditableFocusHint` from CEF was applied before this entity existed.
                editable_focus_hint: EditableFocusHint(Some(false)),
                link_hints_label_width: LinkHintsLabelWidth::default(),
                last_address_url: LastAddressUrl::default(),
            })
            .id();
        index.by_browser_id.insert(ev.browser_id, entity);
    }
}

pub fn apply_browser_despawn_events(
    mut commands: Commands,
    mut events: EventReader<BrowserDespawnEvent>,
    mut index: ResMut<BrowserEntities>,
) {
    for ev in events.read() {
        if let Some(entity) = index.by_browser_id.remove(&ev.browser_id) {
            commands.entity(entity).despawn();
        }
    }
}
