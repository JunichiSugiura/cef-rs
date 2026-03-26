//! Explicit state machine for vim-style OSR input (browse, link hints, insert, find, visual).
//!
//! ```text
//!                    ┌─────────┐
//!         ┌─────────►│ Browse  │◄────────────────────────────┐
//!         │          └────┬────┘                             │
//!         │               │ f (page safe)                    │
//!         │               ▼                                │
//!         │          ┌────────────┐   esc / ttl / editable  │
//!         │          │ LinkHints  │─────────────────────────┤
//!         │          └─────┬──────┘   pick (JS)            │
//!         │                │                                │
//!         │   i,/,v        │                                │
//!         ▼                │                                │
//!    ┌────────┐            │                                │
//!    │ Insert │────────────┼────────────────────────────────┘
//!    └────────┘  esc       │
//!    ┌────────┐            │
//!    │  Find  │────────────┘
//!    └────────┘  esc / enter
//!    ┌────────┐
//!    │ Visual │────────────────────────────────────────────┘
//!    └────────┘  esc
//! ```
//!
//! `LinkHints` is mutually exclusive with `Insert` / `Find` / `Visual`. `find_committed` and
//! `scroll_g_pending` apply only while in `Browse` (and persist across transient modes).
//!
//! Call sites use methods like [`OsrVimMachine::is_find`] instead of matching on [`OsrVimMode`]:
//! Pattern matching on `mode` stays in this module so transitions and predicates stay in one
//! place.

use std::time::{Duration, Instant};

/// How long a link-hint session stays armed after `f` (overlay may appear a frame later).
pub const LINK_HINT_TTL: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OsrVimMode {
    /// j/k scroll, `f` hints, `n`/`N` find repeat, etc.
    Browse,
    LinkHints {
        browser_id: i32,
        until: Instant,
        /// Prefix typed so far (mirrors JS after each successful feed); survives flaky DOM probes.
        typed: String,
    },
    Insert {
        browser_id: i32,
    },
    Find {
        browser_id: i32,
        query: String,
    },
    Visual {
        browser_id: i32,
    },
}

impl Default for OsrVimMode {
    fn default() -> Self {
        OsrVimMode::Browse
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimChromeCleanup {
    Find,
    Visual,
}

/// Shell-side vim / hint state for OSR windows.
#[derive(Debug, Clone)]
pub struct OsrVimMachine {
    mode: OsrVimMode,
    /// After find `Enter`; used for `n` / `N` in `Browse`.
    pub find_committed: String,
    /// First `g` of `gg` (scroll top), only meaningful in `Browse`.
    pub scroll_g_pending: Option<Instant>,
}

impl Default for OsrVimMachine {
    fn default() -> Self {
        Self {
            mode: OsrVimMode::Browse,
            find_committed: String::new(),
            scroll_g_pending: None,
        }
    }
}

impl OsrVimMachine {
    // --- private: all discrimination on `mode` lives here ---

    #[inline]
    fn in_link_hints(&self) -> bool {
        match &self.mode {
            OsrVimMode::LinkHints { .. } => true,
            _ => false,
        }
    }

    #[inline]
    fn in_find_any(&self) -> bool {
        match &self.mode {
            OsrVimMode::Find { .. } => true,
            _ => false,
        }
    }

    #[inline]
    fn in_any_ux(&self) -> bool {
        match &self.mode {
            OsrVimMode::Insert { .. } | OsrVimMode::Find { .. } | OsrVimMode::Visual { .. } => true,
            _ => false,
        }
    }

    // --- public queries (delegate to private helpers / small match for data extraction) ---

    pub fn link_hints_active(&self) -> bool {
        self.in_link_hints()
    }

    pub fn link_hints_browser_id(&self) -> Option<i32> {
        match &self.mode {
            OsrVimMode::LinkHints { browser_id, .. } => Some(*browser_id),
            _ => None,
        }
    }

    pub fn link_hints_typed_prefix(&self) -> &str {
        match &self.mode {
            OsrVimMode::LinkHints { typed, .. } => typed.as_str(),
            _ => "",
        }
    }

    pub fn link_hints_expired(&self, now: Instant) -> bool {
        match &self.mode {
            OsrVimMode::LinkHints { until, .. } => now > *until,
            _ => false,
        }
    }

    pub fn arm_link_hints(&mut self, browser_id: i32, now: Instant) {
        self.mode = OsrVimMode::LinkHints {
            browser_id,
            until: now + LINK_HINT_TTL,
            typed: String::new(),
        };
        self.scroll_g_pending = None;
    }

    pub fn link_hints_push_typed_char(&mut self, ch: char) {
        if let OsrVimMode::LinkHints { typed, .. } = &mut self.mode {
            typed.push(ch);
        }
    }

    /// Leave link-hint mode without checking browser id (caller already hid overlay).
    pub fn clear_link_hints(&mut self) {
        if self.in_link_hints() {
            self.mode = OsrVimMode::Browse;
        }
    }

    /// If `LinkHints` is owned by `browser_id`, clear to `Browse` and return `true`.
    pub fn clear_link_hints_if_browser(&mut self, browser_id: i32) -> bool {
        if self.link_hints_browser_id() == Some(browser_id) {
            self.clear_link_hints();
            return true;
        }
        false
    }

    pub fn find_swallows_keyup(&self, browser_id: i32) -> bool {
        self.is_find(browser_id)
    }

    pub fn ux_browser_id(&self) -> Option<i32> {
        match &self.mode {
            OsrVimMode::Insert { browser_id }
            | OsrVimMode::Find { browser_id, .. }
            | OsrVimMode::Visual { browser_id } => Some(*browser_id),
            _ => None,
        }
    }

    pub fn chrome_cleanup(&self) -> Option<VimChromeCleanup> {
        match &self.mode {
            OsrVimMode::Find { .. } => Some(VimChromeCleanup::Find),
            OsrVimMode::Visual { .. } => Some(VimChromeCleanup::Visual),
            _ => None,
        }
    }

    pub fn is_insert(&self, browser_id: i32) -> bool {
        match &self.mode {
            OsrVimMode::Insert { browser_id: b } => *b == browser_id,
            _ => false,
        }
    }

    pub fn is_find(&self, browser_id: i32) -> bool {
        match &self.mode {
            OsrVimMode::Find { browser_id: b, .. } => *b == browser_id,
            _ => false,
        }
    }

    pub fn is_visual(&self, browser_id: i32) -> bool {
        match &self.mode {
            OsrVimMode::Visual { browser_id: b } => *b == browser_id,
            _ => false,
        }
    }

    pub fn enter_insert(&mut self, browser_id: i32) {
        self.mode = OsrVimMode::Insert { browser_id };
        self.scroll_g_pending = None;
    }

    pub fn enter_find(&mut self, browser_id: i32) {
        self.mode = OsrVimMode::Find {
            browser_id,
            query: String::new(),
        };
        self.scroll_g_pending = None;
    }

    pub fn enter_visual(&mut self, browser_id: i32) {
        self.mode = OsrVimMode::Visual { browser_id };
        self.scroll_g_pending = None;
    }

    pub fn exit_ux_to_browse(&mut self) {
        if self.in_any_ux() {
            self.mode = OsrVimMode::Browse;
        }
    }

    pub fn find_query_mut(&mut self) -> Option<&mut String> {
        match &mut self.mode {
            OsrVimMode::Find { query, .. } => Some(query),
            _ => None,
        }
    }

    /// If expired, transition to `Browse` and return the browser id to hide hints for.
    pub fn expire_link_hints_if_due(&mut self, now: Instant) -> Option<i32> {
        if !self.link_hints_expired(now) {
            return None;
        }
        let bid = self.link_hints_browser_id()?;
        self.clear_link_hints();
        Some(bid)
    }

    /// `Enter` in find bar: move query into `find_committed`, return to `Browse`.
    pub fn finish_find_accept(&mut self) {
        if let OsrVimMode::Find { query, .. } = std::mem::replace(&mut self.mode, OsrVimMode::Browse) {
            self.find_committed = query;
        }
    }

    /// `Esc` in find bar: return to `Browse` and clear committed find string.
    pub fn cancel_find(&mut self) {
        if self.in_find_any() {
            self.mode = OsrVimMode::Browse;
            self.find_committed.clear();
        }
    }

}
