use cef::*;
use std::cell::RefCell;
use std::rc::Rc;

use super::vmux_handler::*;

const NEW_TAB_BUTTON_ID: i32 = 1;
const TAB_BUTTON_ID_BASE: i32 = 1000;
const CLOSE_TAB_BUTTON_ID_BASE: i32 = 2000;

/// Vertical stack: tab bar on top, browser below. `cross_axis_alignment` must be
/// [`AxisAlignment::STRETCH`]: default is [`AxisAlignment::START`], which keeps each
/// child's preferred **width** — `BrowserView` then stays a thin strip on the left.
fn root_vertical_box_settings() -> BoxLayoutSettings {
    BoxLayoutSettings {
        horizontal: 0,
        between_child_spacing: 6,
        cross_axis_alignment: AxisAlignment::STRETCH,
        ..Default::default()
    }
}

/// CEF colors are `ARGB` (`0xAARRGGBB`). Distinct pages per tab id so switching tabs is obvious.
static DEMO_TAB_PAGES: &[(&str, &str)] = &[
    ("https://www.google.com/", "Google"),
    ("https://example.com/", "Example"),
    ("https://www.rust-lang.org/", "Rust"),
    ("https://en.wikipedia.org/wiki/Main_Page", "Wikipedia"),
];

fn demo_page_for_tab_id(tab_id: i32) -> (&'static str, &'static str) {
    let i = tab_id.saturating_sub(1) as usize % DEMO_TAB_PAGES.len();
    DEMO_TAB_PAGES[i]
}

fn style_tab_label_button(button: &LabelButton, active: bool) {
    let mut v = View::from(button);
    if active {
        v.set_background_color(0xFF3d6a9e);
        button.set_enabled_text_colors(0xFFFFFFFF);
    } else {
        v.set_background_color(0xFF2a2a2a);
        button.set_enabled_text_colors(0xFFb8b8b8);
    }
}

#[derive(Clone)]
struct TabEntry {
    id: i32,
    title: String,
    browser_view: BrowserView,
}

struct TabUiState {
    tabs: Vec<TabEntry>,
    next_tab_id: i32,
    active_tab_id: Option<i32>,
    window: Option<Window>,
    root_panel: Panel,
    tab_bar_panel: Panel,
    mounted_tab_id: Option<i32>,
    button_delegates: Vec<ButtonDelegate>,
    shared_state: Rc<RefCell<Option<Rc<RefCell<TabUiState>>>>>,
    client: Option<Client>,
    runtime_style: RuntimeStyle,
}

impl TabUiState {
    fn new(
        window: Option<Window>,
        root_panel: Panel,
        tab_bar_panel: Panel,
        shared_state: Rc<RefCell<Option<Rc<RefCell<TabUiState>>>>>,
        client: Option<Client>,
        runtime_style: RuntimeStyle,
    ) -> Self {
        Self {
            tabs: Vec::new(),
            next_tab_id: 1,
            active_tab_id: None,
            window,
            root_panel,
            tab_bar_panel,
            mounted_tab_id: None,
            button_delegates: Vec::new(),
            shared_state,
            client,
            runtime_style,
        }
    }

    fn append_tab_with_browser(&mut self, browser_view: BrowserView, title: String, active: bool) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        self.tabs.push(TabEntry {
            id,
            title,
            browser_view: browser_view.clone(),
        });

        if active || self.active_tab_id.is_none() {
            self.active_tab_id = Some(id);
        }
        self.apply_active_visibility();
        self.rebuild_tab_bar();
    }

    fn add_new_tab(&mut self) {
        let mut client = self.client.clone();
        let mut delegate = VmuxBrowserViewDelegate::new(self.runtime_style, self.client.clone());
        let (url_s, _label) = demo_page_for_tab_id(self.next_tab_id);
        let url = CefString::from(url_s);
        let browser_view = browser_view_create(
            client.as_mut(),
            Some(&url),
            Some(&BrowserSettings::default()),
            None,
            None,
            Some(&mut delegate),
        );
        if let Some(browser_view) = browser_view {
            if let Some(mut browser) = browser_view.browser() {
                if let Some(frame) = browser.main_frame() {
                    frame.load_url(Some(&url));
                }
            }
            let (_, label) = demo_page_for_tab_id(self.next_tab_id);
            let title = format!("{} · {}", label, self.next_tab_id);
            self.append_tab_with_browser(browser_view, title, true);
        }
    }

    fn apply_active_visibility(&mut self) {
        if let Some(mounted_id) = self.mounted_tab_id {
            if let Some(tab) = self.tabs.iter().find(|tab| tab.id == mounted_id) {
                let mut old_view = View::from(&tab.browser_view);
                self.root_panel.remove_child_view(Some(&mut old_view));
            }
        }

        if let Some(active_id) = self.active_tab_id {
            if let Some(tab) = self.tabs.iter().find(|tab| tab.id == active_id) {
                let mut view = View::from(&tab.browser_view);
                view.set_visible(1);
                self.root_panel.add_child_view(Some(&mut view));
                // BrowserView: flex 1 on main (vertical) axis; stretch on cross (horizontal) axis.
                if let Some(root_layout) =
                    self.root_panel.set_to_box_layout(Some(&root_vertical_box_settings()))
                {
                    let mut tab_bar_view = View::from(&self.tab_bar_panel);
                    root_layout.set_flex_for_view(Some(&mut tab_bar_view), 0);
                    root_layout.set_flex_for_view(Some(&mut view), 1);
                }
                self.mounted_tab_id = Some(active_id);
                if let Some(browser) = tab.browser_view.browser() {
                    if let Some(host) = browser.host() {
                        host.set_focus(1);
                        host.notify_move_or_resize_started();
                    }
                }
            }
        } else {
            self.mounted_tab_id = None;
        }
        self.root_panel.layout();
        if let Some(window) = self.window.as_ref() {
            window.layout();
        }
    }

    fn activate_tab(&mut self, tab_id: i32) {
        if self.tabs.iter().any(|tab| tab.id == tab_id) {
            self.active_tab_id = Some(tab_id);
            self.apply_active_visibility();
            self.rebuild_tab_bar();
        }
    }

    fn close_tab(&mut self, tab_id: i32) {
        let Some(index) = self.tabs.iter().position(|tab| tab.id == tab_id) else {
            return;
        };

        let tab = self.tabs.remove(index);
        if self.mounted_tab_id == Some(tab_id) {
            let mut view = View::from(&tab.browser_view);
            self.root_panel.remove_child_view(Some(&mut view));
            self.mounted_tab_id = None;
        }
        if let Some(browser) = tab.browser_view.browser() {
            if let Some(host) = browser.host() {
                host.close_browser(0);
            }
        }

        if self.tabs.is_empty() {
            self.active_tab_id = None;
        } else if self.active_tab_id == Some(tab_id) {
            let next_index = if index >= self.tabs.len() {
                self.tabs.len() - 1
            } else {
                index
            };
            self.active_tab_id = Some(self.tabs[next_index].id);
        }

        self.apply_active_visibility();
        self.rebuild_tab_bar();
    }

    fn handle_button(&mut self, id: i32) {
        if id == NEW_TAB_BUTTON_ID {
            self.add_new_tab();
        } else if id >= CLOSE_TAB_BUTTON_ID_BASE {
            self.close_tab(id - CLOSE_TAB_BUTTON_ID_BASE);
        } else if id >= TAB_BUTTON_ID_BASE {
            self.activate_tab(id - TAB_BUTTON_ID_BASE);
        }
    }

    fn rebuild_tab_bar(&mut self) {
        // Never drop delegates during a UI callback: the currently executing
        // button delegate may still be on the stack while we rebuild.
        // Keeping prior delegates alive avoids use-after-free crashes.
        self.tab_bar_panel.remove_all_child_views();
        let layout_settings = BoxLayoutSettings {
            horizontal: 1,
            between_child_spacing: 6,
            cross_axis_alignment: AxisAlignment::STRETCH,
            ..Default::default()
        };
        let _ = self.tab_bar_panel.set_to_box_layout(Some(&layout_settings));

        let delegate = TabBarButtonDelegate::new(self.shared_state.clone());
        let mut new_delegate: ButtonDelegate = delegate.clone();
        self.button_delegates.push(new_delegate.clone());
        if let Some(new_button) =
            label_button_create(Some(&mut new_delegate), Some(&CefString::from("+")))
        {
            new_button.set_id(NEW_TAB_BUTTON_ID);
            let mut new_view = View::from(&new_button);
            self.tab_bar_panel.add_child_view(Some(&mut new_view));
        }

        for tab in &self.tabs {
            let is_active = self.active_tab_id == Some(tab.id);
            let delegate = TabBarButtonDelegate::new(self.shared_state.clone());
            let mut tab_delegate: ButtonDelegate = delegate.clone();
            self.button_delegates.push(tab_delegate.clone());
            if let Some(tab_button) = label_button_create(
                Some(&mut tab_delegate),
                Some(&CefString::from(tab.title.as_str())),
            ) {
                tab_button.set_id(TAB_BUTTON_ID_BASE + tab.id);
                style_tab_label_button(&tab_button, is_active);
                let mut tab_view = View::from(&tab_button);
                self.tab_bar_panel.add_child_view(Some(&mut tab_view));
            }

            let delegate = TabBarButtonDelegate::new(self.shared_state.clone());
            let mut close_delegate: ButtonDelegate = delegate.clone();
            self.button_delegates.push(close_delegate.clone());
            if let Some(close_button) =
                label_button_create(Some(&mut close_delegate), Some(&CefString::from("x")))
            {
                close_button.set_id(CLOSE_TAB_BUTTON_ID_BASE + tab.id);
                let mut close_view = View::from(&close_button);
                self.tab_bar_panel.add_child_view(Some(&mut close_view));
            }
        }
        self.tab_bar_panel.layout();
    }
}

wrap_button_delegate! {
    struct TabBarButtonDelegate {
        state: Rc<RefCell<Option<Rc<RefCell<TabUiState>>>>>,
    }

    impl ViewDelegate {}

    impl ButtonDelegate {
        fn on_button_pressed(&self, button: Option<&mut Button>) {
            let Some(button) = button else {
                return;
            };
            // Capture button id first. Rebuild/close operations may destroy the
            // clicked button view, so we must not hold a live `&mut Button`
            // borrow while mutating the tab UI tree.
            let button_id = button.id();
            let state = self.state.borrow();
            let Some(tab_state) = state.as_ref().cloned() else {
                return;
            };
            drop(state);
            let mut task = TabButtonPressedTask::new(tab_state, button_id);
            post_task(ThreadId::UI, Some(&mut task));
        }
    }
}

wrap_task! {
    struct TabButtonPressedTask {
        state: Rc<RefCell<TabUiState>>,
        button_id: i32,
    }

    impl Task {
        fn execute(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);
            self.state.borrow_mut().handle_button(self.button_id);
        }
    }
}

wrap_window_delegate! {
    struct VmuxWindowDelegate {
        browser_view: RefCell<Option<BrowserView>>,
        tab_state: RefCell<Option<Rc<RefCell<TabUiState>>>>,
        initial_show_state: ShowState,
        window_slot: Option<usize>,
        runtime_style: RuntimeStyle,
        client: Option<Client>,
    }

    impl ViewDelegate {
        fn preferred_size(&self, _view: Option<&mut View>) -> Size {
            Size {
                width: 800,
                height: 600,
            }
        }
    }

    impl PanelDelegate {}

    impl WindowDelegate {
        fn on_window_created(&self, window: Option<&mut Window>) {
            // Build root layout: tab bar + active BrowserView (direct children; nested fill panel breaks paint).
            let browser_view = self.browser_view.borrow();
            let (Some(window), Some(initial_browser_view)) = (window, browser_view.as_ref()) else {
                return;
            };

            let Some(root_panel) = panel_create(None) else {
                return;
            };
            let Some(tab_bar_panel) = panel_create(None) else {
                return;
            };
            let _ = root_panel.set_to_box_layout(Some(&root_vertical_box_settings()));
            let _ = tab_bar_panel.set_to_box_layout(Some(&BoxLayoutSettings {
                horizontal: 1,
                between_child_spacing: 6,
                ..Default::default()
            }));

            let mut tab_bar_view = View::from(&tab_bar_panel);
            root_panel.add_child_view(Some(&mut tab_bar_view));

            let mut root_view = View::from(&root_panel);
            let _ = window.set_to_fill_layout();
            window.add_child_view(Some(&mut root_view));

            let shared_state = Rc::new(RefCell::new(None::<Rc<RefCell<TabUiState>>>));
            let tab_state_ref = Rc::new(RefCell::new(TabUiState::new(
                Some(window.clone()),
                root_panel.clone(),
                tab_bar_panel.clone(),
                shared_state.clone(),
                self.client.clone(),
                self.runtime_style,
            )));
            *self.tab_state.borrow_mut() = Some(tab_state_ref.clone());
            *shared_state.borrow_mut() = Some(tab_state_ref.clone());

            let (_, label) = demo_page_for_tab_id(1);
            let title = format!("{} · 1", label);
            tab_state_ref.borrow_mut().append_tab_with_browser(
                initial_browser_view.clone(),
                title,
                true,
            );

            if self.initial_show_state != ShowState::HIDDEN {
                window.show();
            }
        }

        fn on_window_destroyed(&self, _window: Option<&mut Window>) {
            let mut browser_view = self.browser_view.borrow_mut();
            *browser_view = None;
        }

        fn can_close(&self, _window: Option<&mut Window>) -> i32 {
            // Allow the window to close if the browser says it's OK.
            let browser_view = self.browser_view.borrow();
            let browser_view = browser_view.as_ref().expect("BrowserView is None");
            if let Some(browser) = browser_view.browser() {
                let browser_host = browser.host().expect("BrowserHost is None");
                browser_host.try_close_browser()
            } else {
                1
            }
        }

        fn initial_show_state(&self, _window: Option<&mut Window>) -> ShowState {
            self.initial_show_state
        }

        fn initial_bounds(&self, _window: Option<&mut Window>) -> Rect {
            if let Some(slot) = self.window_slot {
                let width = 700;
                let height = 1000;
                let x = if slot % 2 == 0 { 0 } else { width };
                return Rect {
                    x,
                    y: 0,
                    width,
                    height,
                };
            }

            Rect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            }
        }

        fn window_runtime_style(&self) -> RuntimeStyle {
            RuntimeStyle::ALLOY
        }
    }
}

wrap_browser_view_delegate! {
    struct VmuxBrowserViewDelegate {
        runtime_style: RuntimeStyle,
        client: Option<Client>,
    }

    impl ViewDelegate {}

    impl BrowserViewDelegate {
        fn on_popup_browser_view_created(
            &self,
            _browser_view: Option<&mut BrowserView>,
            popup_browser_view: Option<&mut BrowserView>,
            _is_devtools: i32,
        ) -> i32 {
            // Create a new top-level Window for the popup. It will show itself after
            // creation.
            let mut window_delegate = VmuxWindowDelegate::new(
                RefCell::new(popup_browser_view.cloned()),
                RefCell::new(None),
                ShowState::NORMAL,
                None,
                self.runtime_style,
                self.client.clone(),
            );
            window_create_top_level(Some(&mut window_delegate));

            // We created the Window.
            1
        }

        fn browser_runtime_style(&self) -> RuntimeStyle {
            self.runtime_style
        }
    }
}

wrap_app! {
    pub struct VmuxApp;

    impl App {
        fn browser_process_handler(&self) -> Option<BrowserProcessHandler> {
            Some(VmuxBrowserProcessHandler::new(RefCell::new(None)))
        }
    }
}

wrap_browser_process_handler! {
    struct VmuxBrowserProcessHandler {
        client: RefCell<Option<Client>>,
    }

    impl BrowserProcessHandler {
        fn on_context_initialized(&self) {
            debug_assert_ne!(currently_on(ThreadId::UI), 0);

            // Check if Alloy style will be used.
            let command_line = command_line_get_global().expect("Failed to get command line");
            let runtime_style = RuntimeStyle::ALLOY;

            {
                // VmuxHandler implements browser-level callbacks.
                let mut client = self.client.borrow_mut();
                *client = Some(VmuxHandlerClient::new(VmuxHandler::new()));
            }

            // Specify CEF browser settings here.
            let settings = BrowserSettings::default();

            // First tab in each window uses the same demo URL (see `demo_page_for_tab_id(1)`).
            let url = CefString::from(demo_page_for_tab_id(1).0);

            // Views is enabled by default (add `--use-native` to disable).
            let use_views = command_line.has_switch(Some(&CefString::from("use-native"))) == 0;

            // If using Views create the browser using the Views framework, otherwise
            // create the browser using the native platform framework.
            if use_views {
                // Create the BrowserView.
                let mut client = self.default_client();
                let mut delegate = VmuxBrowserViewDelegate::new(runtime_style, self.default_client());
                let browser_view = browser_view_create(
                    client.as_mut(),
                    Some(&url),
                    Some(&settings),
                    None,
                    None,
                    Some(&mut delegate),
                );
                // Optionally configure the initial show state.
                let initial_show_state = CefString::from(
                    &command_line.switch_value(Some(&CefString::from("initial-show-state"))),
                )
                .to_string();
                let initial_show_state = match initial_show_state.as_str() {
                    "minimized" => ShowState::MINIMIZED,
                    "maximized" => ShowState::MAXIMIZED,
                    // Hidden show state is only supported on MacOS.
                    #[cfg(target_os = "macos")]
                    "hidden" => ShowState::HIDDEN,
                    _ => ShowState::NORMAL,
                };

                // Create the Window. It will show itself after creation.
                let mut delegate = VmuxWindowDelegate::new(
                    RefCell::new(browser_view),
                    RefCell::new(None),
                    initial_show_state,
                    Some(0),
                    runtime_style,
                    self.default_client(),
                );
                window_create_top_level(Some(&mut delegate));

                // Create a second startup window in the right tile.
                let mut second_client = self.default_client();
                let mut second_delegate =
                    VmuxBrowserViewDelegate::new(runtime_style, self.default_client());
                let second_browser_view = browser_view_create(
                    second_client.as_mut(),
                    Some(&url),
                    Some(&settings),
                    None,
                    None,
                    Some(&mut second_delegate),
                );
                let mut second_window_delegate = VmuxWindowDelegate::new(
                    RefCell::new(second_browser_view),
                    RefCell::new(None),
                    ShowState::NORMAL,
                    Some(1),
                    runtime_style,
                    self.default_client(),
                );
                window_create_top_level(Some(&mut second_window_delegate));
            } else {
                // Information used when creating the native window.
                let window_info = WindowInfo {
                    runtime_style,
                    ..Default::default()
                };

                #[cfg(target_os = "windows")]
                let window_info = window_info.set_as_popup(Default::default(), "vmux");

                let mut client = self.default_client();
                browser_host_create_browser(
                    Some(&window_info),
                    client.as_mut(),
                    Some(&url),
                    Some(&settings),
                    None,
                    None,
                );
            }
        }

        fn default_client(&self) -> Option<Client> {
            self.client.borrow().clone()
        }
    }
}
