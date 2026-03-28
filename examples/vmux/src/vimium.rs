//! Vimium-style keyboard UX for vmux (Bevy plugin + state machine), listed at crate root alongside
//! other feature domains (`settings`, `panes`, `windows`, `browser`).

pub mod modes;
pub mod scroll;
pub mod state;
pub mod editable_gating;
pub mod window_input;

use bevy_app::{App as BevyApp, Plugin, Update};
use bevy_ecs::event::{EventReader, EventWriter};
use bevy_ecs::prelude::{NonSend, NonSendMut, Query, Res, ResMut, Resource};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use crate::browser::browser_entity::BrowserId;
use crate::browser::event_loop::RuntimeState;
use crate::browser::events::{
    LinkHintsHideBrowserEvent, LinkHintsShowBrowserEvent, NavigateBrowserEvent, ReloadBrowserEvent,
};
use crate::vimium::editable_gating::EditableFocusSnapshot;
use crate::vimium::window_input::BrowserEventBatch;
use crate::browser::view_state::EditableFocusHint;

pub use state::{VimiumChromeCleanup, VimiumState, VimiumStateSnapshot};

/// Live vimium mode machine — separate from [`RuntimeState`] (modifiers, pending shells, …).
#[derive(Resource)]
pub struct VimiumStateResource(pub VimiumState);

impl Default for VimiumStateResource {
    fn default() -> Self {
        Self(VimiumState::default())
    }
}

pub struct VimiumSession {
    vimium: VimiumState,
}

impl VimiumSession {
    pub fn new(vimium: VimiumState) -> Self {
        Self { vimium }
    }
}

impl Deref for VimiumSession {
    type Target = VimiumState;

    fn deref(&self) -> &Self::Target {
        &self.vimium
    }
}

impl DerefMut for VimiumSession {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.vimium
    }
}

pub struct VimiumPlugin;

#[derive(Resource, Debug, Clone, Default)]
pub struct VimiumRuntimeResource {
    pub snapshot: VimiumStateSnapshot,
}

impl Plugin for VimiumPlugin {
    fn build(&self, app: &mut BevyApp) {
        app.init_resource::<VimiumRuntimeResource>().add_systems(
            Update,
            (
                apply_vimium_key_replay_events_system,
                apply_link_hint_feed_events_system,
                apply_find_mode_key_queue_system,
                sync_vimium_runtime_resource_system,
            ),
        );
    }
}

fn apply_vimium_key_replay_events_system(
    mut events: EventReader<crate::browser::event_loop::VimiumKeyReplayEvent>,
    mut osr_host: NonSendMut<crate::browser::renderer::osr_host::state::OsrHostState>,
    mut rt: ResMut<RuntimeState>,
    mut vim: ResMut<VimiumStateResource>,
    focus_q: Query<(&BrowserId, &EditableFocusHint)>,
    mut navigate_events: EventWriter<NavigateBrowserEvent>,
    mut reload_events: EventWriter<ReloadBrowserEvent>,
    mut link_hints_show_events: EventWriter<LinkHintsShowBrowserEvent>,
    mut link_hints_hide_events: EventWriter<LinkHintsHideBrowserEvent>,
) {
    let mut editable_focus: EditableFocusSnapshot = HashMap::new();
    for (bid, hint) in focus_q.iter() {
        editable_focus.insert(bid.0, hint.0);
    }
    for event in events.read() {
        let mut out = BrowserEventBatch::default();
        window_input::handle_vimium_key_replay_event(
            &mut osr_host,
            &mut rt,
            &mut vim.0,
            event.clone(),
            &editable_focus,
            &mut out,
        );
        for ev in out.navigate {
            navigate_events.send(ev);
        }
        for ev in out.reload {
            reload_events.send(ev);
        }
        for ev in out.link_hints_show {
            link_hints_show_events.send(ev);
        }
        for ev in out.link_hints_hide {
            link_hints_hide_events.send(ev);
        }
    }
}

fn apply_link_hint_feed_events_system(
    mut events: EventReader<crate::browser::event_loop::LinkHintFeedEvent>,
    mut osr_host: NonSendMut<crate::browser::renderer::osr_host::state::OsrHostState>,
    mut vim: ResMut<VimiumStateResource>,
    mut link_hints_hide_events: EventWriter<LinkHintsHideBrowserEvent>,
) {
    for event in events.read() {
        let mut out = BrowserEventBatch::default();
        window_input::handle_link_hint_feed_event(&mut osr_host, &mut vim.0, event.clone(), &mut out);
        for ev in out.link_hints_hide {
            link_hints_hide_events.send(ev);
        }
    }
}

fn apply_find_mode_key_queue_system(
    mut osr_host: NonSendMut<crate::browser::renderer::osr_host::state::OsrHostState>,
    mut rt: ResMut<RuntimeState>,
    mut vim: ResMut<VimiumStateResource>,
) {
    window_input::drain_find_mode_keys(&mut osr_host, &mut rt, &mut vim.0);
}

fn sync_vimium_runtime_resource_system(
    osr_host: NonSend<crate::browser::renderer::osr_host::state::OsrHostState>,
    vim: Res<VimiumStateResource>,
    mut vimium_runtime: ResMut<VimiumRuntimeResource>,
) {
    vimium_runtime.snapshot = window_input::vimium_state_snapshot(&osr_host, &vim.0);
}
