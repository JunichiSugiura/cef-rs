#![cfg_attr(
    all(not(debug_assertions), not(feature = "sandbox"), target_os = "windows"),
    windows_subsystem = "windows"
)]

pub mod bundle_paths;
pub mod bundle_log;
pub mod lifecycle_trace;
pub mod panic_log;
pub mod vmux;
pub mod browser;
pub mod panes;
pub mod resources;
pub mod settings;
pub mod vimium;
pub mod plugin;
pub mod window;
pub mod windows;

use std::cell::RefCell;
use std::rc::Rc;

#[cfg(not(all(feature = "sandbox", target_os = "windows")))]
fn main() -> Result<(), &'static str> {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    crate::panic_log::install();
    crate::lifecycle_trace::record_startup_milestone("main_after_panic_hook");
    crate::lifecycle_trace::trace("main_after_panic_hook");

    #[cfg(target_os = "macos")]
    if std::env::var_os("TZ").is_none() {
        // Default timezone before CEF/Chromium init; may stabilize ICU/temporal-style code paths.
        // SAFETY: `main` before other threads spawn; no concurrent `getenv` in this process yet.
        unsafe {
            std::env::set_var("TZ", "UTC");
        }
    }

    #[cfg(target_os = "macos")]
    crate::browser::renderer::osr_host::macos_lifecycle::setup_vmux_application();

    let mut vmux = bevy_app::App::new();
    // EnvFilter syntax: `RUST_LOG` overrides this entire filter when set (see bevy_log::LogPlugin).
    // Explicit `bevy_*` targets so schedule/ECS/render diagnostics show up alongside `target: "vmux"`.
    // Install logging before `load_cef` so early messages (and crashes there) appear in `vmux-bevy.log`.
    let log_filter = crate::bundle_log::vmux_log_filter_string();
    vmux.add_plugins(bevy_log::LogPlugin {
        filter: log_filter,
        level: bevy_log::Level::INFO,
        custom_layer: crate::bundle_log::vmux_bevy_file_custom_layer,
    })
    .add_event::<bevy_app::AppExit>();

    bevy_log::info!(
        target: "vmux",
        pid = std::process::id(),
        "main: start"
    );
    crate::vimium::input_trace::log_startup_notice();
    let _library = browser::backend::cef::bootstrap::load_cef();
    crate::lifecycle_trace::record_startup_milestone("main_after_load_cef");
    crate::lifecycle_trace::trace("main_after_load_cef");

    let args = cef::args::Args::new();
    let shutdown = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    let signal_quit = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        use signal_hook::{consts::SIGINT, consts::SIGTERM, flag};
        flag::register(SIGTERM, Arc::clone(&signal_quit)).expect("vmux: register SIGTERM");
        flag::register(SIGINT, Arc::clone(&signal_quit)).expect("vmux: register SIGINT");
    }

    let client_holder = Rc::new(RefCell::new(None));
    let mut cef_app = crate::vmux::VmuxApp::new(client_holder.clone());
    let ret = cef::execute_process(
        Some(args.as_main_args()),
        Some(&mut cef_app),
        std::ptr::null_mut(),
    );
    if ret >= 0 {
        return Ok(());
    }
    assert_eq!(ret, -1, "cannot execute browser process");
    crate::lifecycle_trace::record_startup_milestone("main_browser_process_before_cef_initialize");
    crate::lifecycle_trace::trace("main_browser_process_before_cef_initialize");
    browser::backend::cef::bootstrap::initialize_cef_after_execute(&args, &mut cef_app);
    crate::lifecycle_trace::record_startup_milestone("main_after_cef_initialize");
    crate::lifecycle_trace::trace("main_after_cef_initialize");

    let mut event_loop = windows::build_event_loop();
    crate::lifecycle_trace::record_startup_milestone("main_after_build_event_loop");
    #[cfg(all(debug_assertions, target_os = "macos"))]
    crate::browser::renderer::osr_host::macos_lifecycle::debug_assert_vmux_is_frontmost_app_subclass();
    let proxy = event_loop.create_proxy();
    vmux.insert_resource(windows::ShutdownFlag(shutdown.clone()));
    #[cfg(unix)]
    {
        vmux.insert_resource(windows::SignalQuitFlag(signal_quit.clone()));
    }
    vmux.insert_resource(windows::CefPumpDeadline::default());
    windows::register_winit_proxy_for_foreign_callbacks(proxy.clone());

    let cef_attach = browser::backend::osr::hub::CefAttach::new();
    vmux.insert_non_send_resource(browser::CefStartupState {
        cef_app,
        client_holder,
        cef_attach,
        event_proxy: proxy,
    })
    .add_plugins(plugin::VmuxPlugin::new())
    .set_runner(move |app| windows::run_winit(&mut event_loop, app))
    .run();

    bevy_log::info!(
        target: "vmux",
        pid = std::process::id(),
        "main: run_main returned"
    );
    Ok(())
}

#[cfg(all(feature = "sandbox", target_os = "windows"))]
fn main() -> Result<(), &'static str> {
    Err("Running in sandbox mode on Windows requires bootstrap.exe or bootstrapc.exe.")
}
