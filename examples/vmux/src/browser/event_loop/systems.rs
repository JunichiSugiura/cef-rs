//! Bevy [`Plugin`] wiring and ECS systems for the winit-backed runner.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::sync::atomic::Ordering;

use bevy_app::{App, AppExit, Plugin, Update};
use bevy_ecs::change_detection::Mut;
use bevy_ecs::event::{EventReader, EventWriter};
use bevy_ecs::prelude::{NonSendMut, Res, ResMut, Resource};
use bevy_ecs::schedule::IntoSystemConfigs;
use bevy_ecs::world::World;
use winit::event::KeyEvent;
use winit::window::WindowId;

use crate::browser::active_browser::{
    navigate_active_browser_system, ActiveBrowserId, NavigateActiveBrowserEvent,
};
use crate::browser::browser_entity::{
    apply_browser_despawn_events, apply_browser_spawn_events, CefBrowserHandles, CefBrowserHandlesInner,
};
use crate::browser::editable_focus::{
    dispatch_editable_focus_probe_requests_system,
    dispatch_editable_vimium_replay_requests_system,
    EditableFocusProbeRequest, EditableVimiumReplayRequest,
};
use crate::browser::events;
use crate::browser::handler_runtime::{
    apply_close_all_browsers_requests_system, BrowserCloseGuardsInner, BrowserCloseGuardsResource,
    BrowserLifecycleInner, BrowserLifecycleResource, RequestCloseAllBrowsersEvent,
};
use crate::browser::renderer::osr_host::state::OsrHostState;
use crate::browser::shell_ops::{apply_browser_ui_ops_system, BrowserUiOpQueue};
use crate::browser::renderer::osr_host::window_effect::OsrHostWindowDispatch;
use crate::vimium::{VimiumState, VimiumStateResource};
use crate::window::pending_window_events::{
    apply_osr_host_window_dispatches_system, ingest_winit_window_dispatches_system, PendingWindowEvents,
};

use super::foreign_callbacks::{register_browser_runtime_for_foreign_callbacks, request_quit};
use super::user_events::{
    ingest_link_hints_nav_invalidate_events_system, LinkHintsNavPending, RuntimeState,
};

/// Runs `f` with [`OsrHostState`] (`NonSend`), [`RuntimeState`], and [`VimiumState`] — nested
/// [`World::resource_scope`] avoids overlapping `&mut World` borrows.
pub(crate) fn with_osr_host_runtime_and_vimium<F, R>(world: &mut World, f: F) -> R
where
    F: FnOnce(&mut OsrHostState, &mut RuntimeState, &mut VimiumState) -> R,
{
    world.resource_scope(|world, mut rt| {
        world.resource_scope(|world, mut vim: Mut<'_, VimiumStateResource>| {
            let mut osr_host = world.non_send_resource_mut::<OsrHostState>();
            f(&mut osr_host, &mut *rt, &mut vim.0)
        })
    })
}

#[derive(Resource, Debug, Default)]
pub struct AppExitRequested(pub bool);

/// Queues editable-focus work from the winit shell; [`process_editable_focus_probe_queue_system`] /
/// [`process_editable_vimium_replay_queue_system`] turn these into [`EditableFocusProbeRequest`] /
/// [`EditableVimiumReplayRequest`] for the dispatch systems in [`crate::browser::editable_focus`].
#[derive(Resource, Clone)]
pub struct EditableFocusQueues {
    pub pending_probes: Arc<Mutex<VecDeque<i32>>>,
    pub pending_vimium_replays: Arc<Mutex<VecDeque<(i32, WindowId, KeyEvent)>>>,
}

impl Default for EditableFocusQueues {
    fn default() -> Self {
        Self {
            pending_probes: Arc::new(Mutex::new(VecDeque::new())),
            pending_vimium_replays: Arc::new(Mutex::new(VecDeque::new())),
        }
    }
}

pub struct WinitPlugin;

impl WinitPlugin {
    pub const fn new() -> Self {
        Self
    }
}

impl Plugin for WinitPlugin {
    fn build(&self, app: &mut App) {
        let cef_handles = Arc::new(Mutex::new(CefBrowserHandlesInner::default()));
        let lifecycle = Arc::new(Mutex::new(BrowserLifecycleInner::default()));
        let close_guards = Arc::new(Mutex::new(BrowserCloseGuardsInner::default()));
        register_browser_runtime_for_foreign_callbacks(
            cef_handles.clone(),
            lifecycle.clone(),
            close_guards.clone(),
        );
        app.insert_resource(CefBrowserHandles(cef_handles));
        app.insert_resource(BrowserLifecycleResource(lifecycle));
        app.insert_resource(BrowserCloseGuardsResource(close_guards));
        app.add_event::<super::user_events::VimiumKeyReplayEvent>()
            .add_event::<super::user_events::LinkHintFeedEvent>()
            .add_event::<crate::browser::browser_entity::BrowserSpawnEvent>()
            .add_event::<crate::browser::browser_entity::BrowserDespawnEvent>()
            .add_event::<events::LinkHintsNavInvalidateBrowser>()
            .add_event::<NavigateActiveBrowserEvent>()
            .add_event::<RequestCloseAllBrowsersEvent>()
            .add_event::<EditableFocusProbeRequest>()
            .add_event::<EditableVimiumReplayRequest>()
            .init_resource::<EditableFocusQueues>()
            .init_resource::<BrowserUiOpQueue>()
            .add_event::<OsrHostWindowDispatch>()
            .init_resource::<PendingWindowEvents>()
            .init_resource::<crate::browser::browser_entity::BrowserEntities>()
            .init_resource::<LinkHintsNavPending>()
            .init_resource::<RuntimeState>()
            .init_resource::<VimiumStateResource>()
            .add_systems(
                Update,
                (
                    drain_signal_quit_to_request_quit_system,
                    ingest_link_hints_nav_invalidate_events_system,
                    apply_browser_spawn_events,
                    apply_browser_despawn_events,
                    (
                        ingest_winit_window_dispatches_system,
                        apply_osr_host_window_dispatches_system,
                        crate::browser::renderer::vmux_render::runner::vmux_osr_flush_redraw_queue_system,
                    )
                        .chain(),
                    apply_browser_ui_ops_system,
                    (
                        process_editable_focus_probe_queue_system,
                        dispatch_editable_focus_probe_requests_system,
                        process_editable_vimium_replay_queue_system,
                        dispatch_editable_vimium_replay_requests_system,
                    )
                        .chain(),
                    apply_pending_set_active_browser_system,
                    navigate_active_browser_system,
                    apply_close_all_browsers_requests_system,
                    emit_app_exit_on_shutdown_signal,
                    handle_app_exit_for_graceful_shutdown,
                ),
            )
            .init_resource::<ActiveBrowserId>()
            .init_resource::<AppExitRequested>();
    }
}

/// Unix signals must not flip [`ShutdownFlag`] directly: that flag gates `run_winit` exit and must
/// only become true after browsers close. Drain into the same path as AppKit Quit.
pub fn drain_signal_quit_to_request_quit_system(
    signal: Option<Res<super::user_events::SignalQuitFlag>>,
) {
    let Some(signal) = signal else {
        return;
    };
    if signal.0.swap(false, Ordering::AcqRel) {
        request_quit();
    }
}

pub fn process_editable_focus_probe_queue_system(
    queues: Res<EditableFocusQueues>,
    mut writer: EventWriter<EditableFocusProbeRequest>,
) {
    let pending: Vec<i32> = queues
        .pending_probes
        .lock()
        .ok()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default();
    for browser_id in pending {
        writer.send(EditableFocusProbeRequest { browser_id });
    }
}

pub fn process_editable_vimium_replay_queue_system(
    queues: Res<EditableFocusQueues>,
    mut writer: EventWriter<EditableVimiumReplayRequest>,
) {
    let pending: Vec<(i32, WindowId, KeyEvent)> = queues
        .pending_vimium_replays
        .lock()
        .ok()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default();
    for (browser_id, window_id, event) in pending {
        writer.send(EditableVimiumReplayRequest {
            browser_id,
            window_id,
            event,
        });
    }
}

/// Writes [`ActiveBrowserId`] from OSR host input (focus, clicks, …).
pub fn apply_pending_set_active_browser_system(
    osr_host: NonSendMut<OsrHostState>,
    mut active: ResMut<ActiveBrowserId>,
) {
    let Some(browser_id) = osr_host.take_pending_set_active_browser() else {
        return;
    };
    active.0 = Some(browser_id);
}

pub fn emit_app_exit_on_shutdown_signal(
    shutdown: Res<super::user_events::ShutdownFlag>,
    mut requested: ResMut<AppExitRequested>,
    mut app_exit: EventWriter<AppExit>,
) {
    if shutdown.0.load(Ordering::Acquire) && !requested.0 {
        requested.0 = true;
        app_exit.send(AppExit::Success);
    }
}

pub fn handle_app_exit_for_graceful_shutdown(
    shutdown: Res<super::user_events::ShutdownFlag>,
    handles: Res<CefBrowserHandles>,
    mut app_exit: EventReader<AppExit>,
    mut requested: ResMut<AppExitRequested>,
    mut close_all: EventWriter<RequestCloseAllBrowsersEvent>,
) {
    if app_exit.read().next().is_none() || requested.0 {
        return;
    }
    requested.0 = true;
    if handles.0.lock().map(|g| !g.is_empty()).unwrap_or(false) {
        // Match shell Cmd+Q (`QuitCloseAllBrowsersBrowserEvent`) and menu Quit: force-close so
        // windowless OSR teardown cannot stall on `close_browser(false)`.
        close_all.send(RequestCloseAllBrowsersEvent { force_close: true });
    } else {
        shutdown.0.store(true, Ordering::Release);
    }
}
