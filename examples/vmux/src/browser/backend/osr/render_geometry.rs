//! View sizing for CEF OSR [`RenderHandler`] (`view_rect` / `screen_info`).
//! Resolves the shared logical-size [`Arc`] via [`ForeignOsrIndex`]; geometry is implemented on
//! [`crate::browser::browser_entity::OsrViewLogicalSize`] for Bevy ECS parity.

use cef::{Browser, ImplBrowser as _, Rect, ScreenInfo};
use winit::dpi::LogicalSize;

use crate::browser::browser_entity::OsrViewLogicalSize;

use super::foreign_index::{self, ForeignOsrIndex};

fn fill_view_rect_from_logical(sz: LogicalSize<f32>, rect: &mut Rect) {
    rect.x = 0;
    rect.y = 0;
    rect.width = sz.width.max(1.0).ceil() as i32;
    rect.height = sz.height.max(1.0).ceil() as i32;
}

fn fill_screen_info_from_logical(
    sz: LogicalSize<f32>,
    device_scale_factor: f32,
    screen_info: &mut ScreenInfo,
) {
    let w_px = (sz.width.max(1.0) * device_scale_factor).ceil() as i32;
    let h_px = (sz.height.max(1.0) * device_scale_factor).ceil() as i32;
    screen_info.rect.x = 0;
    screen_info.rect.y = 0;
    screen_info.rect.width = w_px;
    screen_info.rect.height = h_px;
    screen_info.available_rect = screen_info.rect.clone();
}

pub fn apply_view_rect(index: &ForeignOsrIndex, browser: &Browser, rect: &mut Rect) {
    let id = browser.identifier();
    if foreign_index::with_tab(index, id, |slot| {
        OsrViewLogicalSize::fill_cef_view_rect_from_arc(&slot.size, rect);
    })
    .is_some()
    {
        return;
    }
    if let Ok(g) = index.pre_attach_view_logical.lock() {
        if let Some(sz) = g.as_ref() {
            fill_view_rect_from_logical(*sz, rect);
        }
    }
}

pub fn apply_screen_info(
    index: &ForeignOsrIndex,
    browser: Option<&Browser>,
    device_scale_factor: f32,
    screen_info: &mut ScreenInfo,
) {
    screen_info.device_scale_factor = device_scale_factor;
    let Some(browser) = browser else {
        return;
    };
    let id = browser.identifier();
    if foreign_index::with_tab(index, id, |slot| {
        OsrViewLogicalSize::fill_cef_screen_rect_from_arc(
            &slot.size,
            device_scale_factor,
            screen_info,
        );
    })
    .is_some()
    {
        return;
    }
    if let Ok(g) = index.pre_attach_view_logical.lock() {
        if let Some(sz) = g.as_ref() {
            fill_screen_info_from_logical(*sz, device_scale_factor, screen_info);
        }
    }
}
