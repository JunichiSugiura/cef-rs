//! Chromium external message pump: **all** [`cef::do_message_loop_work`] calls for vmux live here.
//!
//! The winit [`crate::shared::run_main`] loop drives CEF by calling [`main_tick`]
//! (baseline + optional post-create settling). UI-thread bursts use [`pump`]; cross-thread CEF work
//! is posted with `post_task` and the shell is woken via [`super::event_loop`] — never import
//! `do_message_loop_work` outside this module.

use cef::do_message_loop_work;

/// Run CEF’s message pump a fixed number of times (UI-thread bursts, e.g. after `execute_java_script`).
#[inline]
pub fn pump(times: u32) {
    for _ in 0..times {
        do_message_loop_work();
    }
}

/// One outer-loop iteration: always at least one pump, plus up to `post_create_extra` spread from
/// [`super::VmuxOsrApp::drain_cef_post_create_pumps`].
#[inline]
pub fn main_tick(post_create_extra: u32) {
    pump(1 + post_create_extra);
}
