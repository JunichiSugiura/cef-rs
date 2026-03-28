//! Deferred per-browser view state mutations queued from winit / CEF and applied on Bevy `Update`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use bevy_ecs::prelude::{Query, Res, Resource};

use crate::browser::browser_entity::BrowserId;
use crate::browser::view_state::{EditableFocusHint, LastAddressUrl};

#[derive(Debug, Clone)]
pub enum BrowserUiOp {
    InvalidateEditableFocusHint { browser_id: i32 },
    SetEditableFocusHint { browser_id: i32, editable: bool },
    RemoveBrowserEntries { browser_id: i32 },
}

#[derive(Resource, Clone)]
pub struct BrowserUiOpQueue(pub Arc<Mutex<VecDeque<BrowserUiOp>>>);

impl Default for BrowserUiOpQueue {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(VecDeque::new())))
    }
}

pub fn enqueue_browser_ui_op(queue: &BrowserUiOpQueue, op: BrowserUiOp) {
    if let Ok(mut q) = queue.0.lock() {
        q.push_back(op);
    }
}

pub fn apply_browser_ui_ops_system(
    queue: Res<BrowserUiOpQueue>,
    mut hints: Query<(&BrowserId, &mut EditableFocusHint, &mut LastAddressUrl)>,
) {
    let ops: Vec<BrowserUiOp> = queue
        .0
        .lock()
        .ok()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default();
    if ops.is_empty() {
        return;
    }
    for op in ops {
        match op {
            BrowserUiOp::InvalidateEditableFocusHint { browser_id } => {
                for (bid, mut hint, _) in &mut hints {
                    if bid.0 == browser_id {
                        hint.0 = None;
                        break;
                    }
                }
            }
            BrowserUiOp::SetEditableFocusHint {
                browser_id,
                editable,
            } => {
                for (bid, mut hint, _) in &mut hints {
                    if bid.0 == browser_id {
                        hint.0 = Some(editable);
                        break;
                    }
                }
            }
            BrowserUiOp::RemoveBrowserEntries { browser_id } => {
                for (bid, mut hint, mut last_url) in &mut hints {
                    if bid.0 == browser_id {
                        hint.0 = None;
                        last_url.0 = None;
                        break;
                    }
                }
            }
        }
    }
}
