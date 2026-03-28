//! Winit [`ApplicationHandler`] + [`run_winit`] pump loop.

use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use bevy_app::{App, AppExit};
use bevy_ecs::event::Events;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::WindowId;
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};

use crate::browser::browser_entity::{BrowserDespawnEvent, BrowserSpawnEvent, CefBrowserHandles};
use crate::browser::events;
use crate::browser::handler_runtime::RequestCloseAllBrowsersEvent;
use crate::browser::shell_ops::{enqueue_browser_ui_op, BrowserUiOp, BrowserUiOpQueue};
use crate::window::pending_window_events::PendingWindowEvents;

use super::systems::{with_osr_host_runtime_and_vimium, AppExitRequested};
use super::user_events::{
    AppEvent, CefEvent, CefPumpDeadline, LinkHintFeedEvent, ShutdownFlag, UserEvent, VimiumEvent,
    VimiumKeyReplayEvent,
};

fn push_shutdown_event(app: &mut App) {
    if let Some(mut ev) = app.world_mut().get_resource_mut::<Events<AppExit>>() {
        ev.send(AppExit::Success);
    }
}

/// AppKit `terminate:` / `request_quit` run outside normal ECS scheduling; mirror
/// [`super::systems::handle_app_exit_for_graceful_shutdown`] here so browser teardown starts in the
/// same turn (do not rely only on `AppExit` + `Update` ordering).
///
/// Do not bail out when [`AppExitRequested`] is already true: a prior quit may have failed to close
/// CEF browsers; retries must still emit [`RequestCloseAllBrowsersEvent`].
fn apply_immediate_graceful_teardown(app: &mut App) {
    let has_browsers = app
        .world()
        .get_resource::<CefBrowserHandles>()
        .map(|h| h.0.lock().map(|g| !g.is_empty()).unwrap_or(false))
        .unwrap_or(false);

    let mut branch = "noop";
    if has_browsers {
        if let Some(mut ev) = app
            .world_mut()
            .get_resource_mut::<Events<RequestCloseAllBrowsersEvent>>()
        {
            ev.send(RequestCloseAllBrowsersEvent { force_close: true });
            branch = "sent_request_close_all_force";
        } else {
            branch = "has_browsers_missing_request_close_events";
        }
    } else if let Some(flag) = app.world_mut().get_resource_mut::<ShutdownFlag>() {
        flag.0.store(true, Ordering::Release);
        branch = "set_shutdown_flag";
    }

    let ar = if let Some(mut requested) = app.world_mut().get_resource_mut::<AppExitRequested>() {
        if !requested.0 {
            requested.0 = true;
            "app_exit_requested_set"
        } else {
            "app_exit_requested_already"
        }
    } else {
        "no_app_exit_requested_resource"
    };

    crate::lifecycle_trace::record_runtime_event(&format!(
        "apply_immediate_graceful_teardown has_browsers={has_browsers} branch={branch} {ar}"
    ));

    if let Some(mut rt) = app.world_mut().get_resource_mut::<super::user_events::RuntimeState>() {
        rt.quit_requested = true;
        rt.quit_started_at.get_or_insert(std::time::Instant::now());
    }
}

/// Winit [`ApplicationHandler`] state: staging for [`PendingWindowEvents`] and about-to-wait.
/// Logic lives in [`flush_winit_runner_pending_callbacks`] and [`winit_runner_user_event`] helpers, not on inherent methods.
pub struct WinitAppRunnerState<'a> {
    pub app: &'a mut App,
    pending_window_events: Vec<(WindowId, WindowEvent)>,
    pending_about_to_wait: bool,
}

/// After each `pump_app_events`, forward staged window events into the ECS queue and run shell idle work.
pub(crate) fn flush_winit_runner_pending_callbacks(runner: &mut WinitAppRunnerState<'_>) {
    if !runner.pending_window_events.is_empty() {
        let q = runner
            .app
            .world_mut()
            .resource_mut::<PendingWindowEvents>();
        if let Ok(mut deque) = q.0.lock() {
            deque.extend(runner.pending_window_events.drain(..));
        }
    }
    if runner.pending_about_to_wait {
        runner.pending_about_to_wait = false;
        let shutdown = runner.app.world().resource::<ShutdownFlag>().clone();
        with_osr_host_runtime_and_vimium(runner.app.world_mut(), |osr_host, rt, _vim| {
            osr_host.handle_about_to_wait(rt, &shutdown);
        });
    }
}

fn winit_runner_user_event(
    runner: &mut WinitAppRunnerState<'_>,
    event_loop: &ActiveEventLoop,
    event: UserEvent,
) {
    let _ = event_loop;
    match event {
        UserEvent::App(app) => match app {
            AppEvent::LinkHintFeed(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<LinkHintFeedEvent>>()
                {
                    events.send(event);
                }
            }
            AppEvent::BrowserSpawn(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<BrowserSpawnEvent>>()
                {
                    events.send(event);
                }
            }
            AppEvent::BrowserDespawn(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<BrowserDespawnEvent>>()
                {
                    events.send(event);
                }
            }
            AppEvent::ScheduleCefPump { deadline } => {
                if let Some(mut d) = runner.app.world_mut().get_resource_mut::<CefPumpDeadline>() {
                    d.merge(deadline);
                }
            }
            AppEvent::RequestQuit => {
                crate::lifecycle_trace::record_runtime_event("winit_user_event RequestQuit");
                push_shutdown_event(runner.app);
                apply_immediate_graceful_teardown(runner.app);
            }
            AppEvent::ShutdownRequested => {
                if let Some(flag) = runner.app.world_mut().get_resource_mut::<ShutdownFlag>() {
                    flag.0.store(true, Ordering::Release);
                }
            }
            AppEvent::ShowMainWindowBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::ShowMainWindowBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            AppEvent::CloseAllBrowsersBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::CloseAllBrowsersBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            AppEvent::ArmWindowlessCloseBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::ArmWindowlessCloseBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            AppEvent::QuitCloseAllBrowsersBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::QuitCloseAllBrowsersBrowserEvent>>()
                {
                    events.send(event);
                }
            }
        },
        UserEvent::Cef(cef) => match cef {
            CefEvent::SetEditableFocusHint {
                browser_id,
                editable,
            } => {
                let queue = runner.app.world().resource::<BrowserUiOpQueue>().clone();
                enqueue_browser_ui_op(
                    &queue,
                    BrowserUiOp::SetEditableFocusHint {
                        browser_id,
                        editable,
                    },
                );
            }
            CefEvent::RemoveBrowserEntries { browser_id } => {
                let queue = runner.app.world().resource::<BrowserUiOpQueue>().clone();
                enqueue_browser_ui_op(&queue, BrowserUiOp::RemoveBrowserEntries { browser_id });
            }
            CefEvent::NavigateBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::NavigateBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            CefEvent::ReloadBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::ReloadBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            CefEvent::DelayedNavigationRepaintBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::DelayedNavigationRepaintBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            CefEvent::AddressChangedBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::AddressChangedBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            CefEvent::TitleChangedBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::TitleChangedBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            CefEvent::LoadingStateChangedBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::LoadingStateChangedBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            CefEvent::AfterCreatedBrowserCallback(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::AfterCreatedBrowserCallbackEvent>>()
                {
                    events.send(event);
                }
            }
            CefEvent::BeforeCloseBrowserCallback(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::BeforeCloseBrowserCallbackEvent>>()
                {
                    events.send(event);
                }
            }
            CefEvent::DoCloseBrowserCallback(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::DoCloseBrowserCallbackEvent>>()
                {
                    events.send(event);
                }
            }
            CefEvent::LoadErrorBrowserCallback(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::LoadErrorBrowserCallbackEvent>>()
                {
                    events.send(event);
                }
            }
        },
        UserEvent::Vimium(v) => match v {
            VimiumEvent::VimiumKeyReplay(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<VimiumKeyReplayEvent>>()
                {
                    events.send(event);
                }
            }
            VimiumEvent::LinkHintsShowBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::LinkHintsShowBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            VimiumEvent::LinkHintsHideBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::LinkHintsHideBrowserEvent>>()
                {
                    events.send(event);
                }
            }
            VimiumEvent::LinkHintsFeedKeyDeferredBrowser(event) => {
                if let Some(mut events) = runner
                    .app
                    .world_mut()
                    .get_resource_mut::<Events<events::LinkHintsFeedKeyDeferredBrowserEvent>>()
                {
                    events.send(event);
                }
            }
        },
    }
}

fn winit_runner_resumed(runner: &mut WinitAppRunnerState<'_>, event_loop: &ActiveEventLoop) {
    with_osr_host_runtime_and_vimium(runner.app.world_mut(), |osr_host, rt, _vim| {
        osr_host.handle_resumed(rt, event_loop);
    });
    #[cfg(target_os = "macos")]
    {
        use crate::browser::backend::cef::bootstrap::GpuResource;
        if runner.app.world().get_resource::<GpuResource>().is_none() {
            if let Some(gpu) = super::try_foreign_gpu() {
                runner.app.world_mut().insert_resource(GpuResource(gpu));
            }
        }
    }
}

fn winit_runner_window_event(
    runner: &mut WinitAppRunnerState<'_>,
    event_loop: &ActiveEventLoop,
    window_id: WindowId,
    event: WindowEvent,
) {
    let _ = event_loop;
    runner.pending_window_events.push((window_id, event));
}

fn winit_runner_about_to_wait(runner: &mut WinitAppRunnerState<'_>, event_loop: &ActiveEventLoop) {
    let _ = event_loop;
    runner.pending_about_to_wait = true;
}

impl ApplicationHandler<UserEvent> for WinitAppRunnerState<'_> {
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        winit_runner_user_event(self, event_loop, event);
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        winit_runner_resumed(self, event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        winit_runner_window_event(self, event_loop, window_id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        winit_runner_about_to_wait(self, event_loop);
    }
}

pub fn build_event_loop() -> EventLoop<UserEvent> {
    let event_loop = {
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            EventLoop::<UserEvent>::with_user_event()
                .with_activation_policy(ActivationPolicy::Regular)
                .with_default_menu(false)
                .build()
                .expect("vmux: EventLoop::build")
        }
        #[cfg(not(target_os = "macos"))]
        {
            EventLoop::<UserEvent>::with_user_event()
                .build()
                .expect("vmux: EventLoop::build")
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop
}

pub fn run_winit(
    event_loop: &mut winit::event_loop::EventLoop<UserEvent>,
    mut app: App,
) -> AppExit {
    const IDLE_WAIT: Duration = Duration::from_millis(8);
    // CEF + winit + Bevy: foreign callbacks and pump ordering stay in this module (not ECS systems)
    // until OSR is stable; see `OsrHostState::finish_next_pending_browser_if_any` for browser create.
    // Ensure Startup schedule runs before systems/resources that expect OsrHostState.
    app.update();
    crate::lifecycle_trace::record_startup_milestone("run_winit_after_first_bevy_update");

    loop {
        if super::foreign_callbacks::take_pending_request_quit() {
            crate::lifecycle_trace::record_runtime_event("run_winit drained pending_request_quit");
            push_shutdown_event(&mut app);
            apply_immediate_graceful_teardown(&mut app);
        }
        // Match `examples/osr`: Chromium `do_message_loop_work` before winit `pump_app_events`.
        crate::browser::backend::cef::pump::pump(1);

        {
            with_osr_host_runtime_and_vimium(app.world_mut(), |osr_host, rt, _vim| {
                osr_host.pump_macos_shell_refocus(rt);
                let issued_create = osr_host.finish_next_pending_browser_if_any(rt);
                if issued_create {
                    rt.bump_cef_post_create_pumps(24);
                }
                #[cfg(target_os = "macos")]
                osr_host.macos_poll_cef_browser_attach(rt);
            });
        }

        let wait = app
            .world()
            .resource::<CefPumpDeadline>()
            .next_wait_timeout(IDLE_WAIT);
        let (status, mut runner_state) = {
            let mut runner_state = WinitAppRunnerState {
                app: &mut app,
                pending_window_events: Vec::new(),
                pending_about_to_wait: false,
            };
            let status = event_loop.pump_app_events(Some(wait), &mut runner_state);
            (status, runner_state)
        };
        flush_winit_runner_pending_callbacks(&mut runner_state);

        if let PumpStatus::Exit(_code) = status {
            crate::lifecycle_trace::record_runtime_event("run_winit PumpStatus::Exit");
            push_shutdown_event(&mut app);
            apply_immediate_graceful_teardown(&mut app);
        }

        with_osr_host_runtime_and_vimium(app.world_mut(), |osr_host, rt, _vim| {
            osr_host.pump_macos_shell_refocus(rt);
        });

        app.update();
        let shutdown_complete = app
            .world()
            .get_resource::<ShutdownFlag>()
            .map(|r| r.0.load(Ordering::Acquire))
            .unwrap_or(false);
        if shutdown_complete {
            break;
        }
        let post_create_chunk =
            with_osr_host_runtime_and_vimium(app.world_mut(), |_osr_host, rt, _vim| {
                rt.drain_cef_post_create_pumps(8)
            });
        let _ = app
            .world_mut()
            .resource_mut::<CefPumpDeadline>()
            .take_if_due();
        crate::browser::backend::cef::pump::main_tick(post_create_chunk);
        #[cfg(target_os = "macos")]
        {
            with_osr_host_runtime_and_vimium(app.world_mut(), |osr_host, rt, _vim| {
                osr_host.macos_poll_cef_browser_attach(rt);
            });
            thread::sleep(Duration::from_millis(1000 / 17));
        }
    }

    // Keep CEF teardown coupled to the custom runner lifecycle.
    cef::quit_message_loop();
    cef::shutdown();

    AppExit::Success
}
