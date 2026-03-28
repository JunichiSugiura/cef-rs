use bevy_ecs::prelude::Component;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BrowserPaneState {
    pub browser_id: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    Browser(BrowserPaneState),
    Terminal,
    AIChat,
}

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneComponent {
    pub id: PaneId,
    pub pane: Pane,
}
