use bevy_ecs::prelude::{Component, Resource};

/// Max DOM ancestor walk depth used by editable-focus probing.
#[derive(Resource, Debug, Clone, Copy)]
pub struct DomFocusAncestorWalkMax(pub usize);

/// Per-browser editable focus hint mirrored from CEF probe results.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct EditableFocusHint(pub Option<bool>);

/// Per-browser fixed link-hints label width cache.
#[derive(Component, Debug, Clone, Copy)]
pub struct LinkHintsLabelWidth(pub u8);

impl Default for LinkHintsLabelWidth {
    fn default() -> Self {
        Self(1)
    }
}

/// Last observed URL for address bar updates in the embedded view.
#[derive(Component, Debug, Clone, Default)]
pub struct LastAddressUrl(pub Option<String>);
