//! Winit [`WindowEvent`]s for OSR host windows: queued from the runner, drained on Bevy `Update`.
//!
//! The custom winit runner appends events here in [`crate::browser::event_loop::flush_winit_runner_pending_callbacks`];
//! [`ingest_winit_window_dispatches_system`] turns each queued pair into a [`OsrHostWindowDispatch`](crate::browser::renderer::osr_host::window_effect::OsrHostWindowDispatch) event;
//! [`apply_osr_host_window_dispatches_system`] runs after browser entity spawn/despawn (see
//! [`crate::browser::event_loop::WinitPlugin`]) and before [`crate::browser::shell_ops::apply_browser_ui_ops_system`], calling
//! [`crate::window::dispatch::handle_window_event`] in FIFO order for each dispatch.
//!
//! Each batch is dispatched with [`crate::browser::browser_entity::BrowserWindowId`] / [`crate::browser::browser_entity::BrowserId`]
//! resolved via ECS `Query` so handlers can align CEF `browser_id` with browser entities.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use bevy_ecs::entity::Entity;
use bevy_ecs::event::EventReader;
use bevy_ecs::event::EventWriter;
use bevy_ecs::prelude::{NonSendMut, Query, Res, ResMut, Resource};
use winit::event::WindowEvent;
use winit::window::WindowId;

use crate::browser::events::{
    ArmWindowlessCloseBrowserEvent, LinkHintsFeedKeyDeferredBrowserEvent, NavigateBrowserEvent,
    QuitCloseAllBrowsersBrowserEvent,
};
use crate::browser::browser_entity::{BrowserId, BrowserWindowId};
use crate::browser::event_loop::{LinkHintsNavPending, RuntimeState};
use crate::browser::renderer::osr_host::state::OsrHostState;
use crate::browser::renderer::osr_host::window_effect::OsrHostWindowDispatch;
use crate::browser::view_state::EditableFocusHint;
use crate::vimium::editable_gating::EditableFocusSnapshot;
use crate::vimium::window_input::BrowserEventBatch;
use crate::vimium::VimiumStateResource;
use crate::window::dispatch;

#[derive(Resource, Clone)]
pub struct PendingWindowEvents(pub Arc<Mutex<VecDeque<(WindowId, WindowEvent)>>>);

impl Default for PendingWindowEvents {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(VecDeque::new())))
    }
}

/// Drain the winit queue into Bevy [`OsrHostWindowDispatch`](crate::browser::renderer::osr_host::window_effect::OsrHostWindowDispatch) events (preserves order).
pub fn ingest_winit_window_dispatches_system(
    pending: Res<PendingWindowEvents>,
    mut writer: EventWriter<OsrHostWindowDispatch>,
) {
    let batch: Vec<(WindowId, WindowEvent)> = pending
        .0
        .lock()
        .ok()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default();
    for (window_id, event) in batch {
        writer.send(OsrHostWindowDispatch { window_id, event });
    }
}

/// Apply each dispatch in order (CEF / OSR host + [`crate::vimium`] routing inside [`handle_window_event`](dispatch::handle_window_event)).
pub fn apply_osr_host_window_dispatches_system(
    mut osr_host: NonSendMut<OsrHostState>,
    mut rt: ResMut<RuntimeState>,
    mut vim: ResMut<VimiumStateResource>,
    mut link_hints_nav_pending: ResMut<LinkHintsNavPending>,
    browser_q: Query<(Entity, &BrowserWindowId, &BrowserId)>,
    focus_q: Query<(&BrowserId, &EditableFocusHint)>,
    mut dispatches: EventReader<OsrHostWindowDispatch>,
    mut navigate_events: EventWriter<NavigateBrowserEvent>,
    mut link_hints_feed_events: EventWriter<LinkHintsFeedKeyDeferredBrowserEvent>,
    mut arm_windowless_close_events: EventWriter<ArmWindowlessCloseBrowserEvent>,
    mut quit_close_events: EventWriter<QuitCloseAllBrowsersBrowserEvent>,
) {
    let mut by_window: HashMap<WindowId, (Entity, i32)> = HashMap::new();
    for (entity, wid, bid) in browser_q.iter() {
        by_window.insert(wid.0, (entity, bid.0));
    }
    let mut editable_focus: EditableFocusSnapshot = HashMap::new();
    for (bid, hint) in focus_q.iter() {
        editable_focus.insert(bid.0, hint.0);
    }
    let mut out = BrowserEventBatch::default();
    for dispatch in dispatches.read() {
        let ecs = by_window.get(&dispatch.window_id).copied();
        dispatch::handle_window_event(
            &mut osr_host,
            &mut rt,
            &mut vim.0,
            dispatch.window_id,
            dispatch.event.clone(),
            ecs,
            &editable_focus,
            &mut link_hints_nav_pending,
            &mut out,
        );
    }
    for ev in out.navigate {
        navigate_events.send(ev);
    }
    for ev in out.link_hints_feed_key_deferred {
        link_hints_feed_events.send(ev);
    }
    for ev in out.arm_windowless_close {
        arm_windowless_close_events.send(ev);
    }
    for ev in out.quit_close_all_browsers {
        quit_close_events.send(ev);
    }
}
