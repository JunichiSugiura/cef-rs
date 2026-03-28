//! Chromium external message pump: all `do_message_loop_work` calls live here.

use ::cef::do_message_loop_work;

#[inline]
pub fn pump(times: u32) {
    for _ in 0..times {
        do_message_loop_work();
    }
}

#[inline]
pub fn main_tick(post_create_extra: u32) {
    pump(1 + post_create_extra);
}
