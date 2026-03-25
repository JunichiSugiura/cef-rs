//! Rust port of the [`vmux`](https://github.com/chromiumembedded/cef/tree/master/tests/vmux) example.

use cef::*;

/// When launched with `open`/Finder, stderr is easy to miss. This also appends to `/tmp/vmux-launch.log`.
/// Includes PID so browser vs CEF helper processes (same binary) are distinguishable in one file.
pub fn launch_trace(msg: &str) {
    let line = format!("[vmux pid={}] {msg}", std::process::id());
    eprintln!("{line}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/vmux-launch.log")
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    }
}

pub mod resources;
pub mod vmux_app;
pub mod vmux_handler;
pub mod vmux_osr;

#[cfg(target_os = "macos")]
pub type Library = library_loader::LibraryLoader;

#[cfg(not(target_os = "macos"))]
pub struct Library;

#[allow(dead_code)]
pub fn load_cef() -> Library {
    #[cfg(target_os = "macos")]
    let library = {
        let exe = std::env::current_exe().unwrap();
        launch_trace(&format!("load_cef: exe={}", exe.display()));
        let loader = library_loader::LibraryLoader::new(&exe, false);
        assert!(
            loader.load(),
            "cef: failed to load Chromium Embedded Framework (bundle ../Frameworks or CEF_FRAMEWORK_PATH)"
        );
        launch_trace("load_cef: framework load OK");
        loader
    };
    #[cfg(not(target_os = "macos"))]
    let library = Library;

    // Initialize the CEF API version.
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);

    #[cfg(target_os = "macos")]
    crate::mac::setup_vmux_application();

    library
}

#[allow(dead_code)]
pub fn run_main(main_args: &MainArgs, sandbox_info: *mut u8) {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;
    use winit::event_loop::ControlFlow;
    use winit::event_loop::EventLoop;
    use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};

    let shutdown = Arc::new(AtomicBool::new(false));
    vmux_osr::shutdown::install_shutdown_flag(shutdown.clone());

    #[cfg(unix)]
    {
        use signal_hook::{consts::SIGINT, consts::SIGTERM, flag};
        flag::register(SIGTERM, Arc::clone(&shutdown)).expect("vmux: register SIGTERM");
        flag::register(SIGINT, Arc::clone(&shutdown)).expect("vmux: register SIGINT");
    }

    let client_holder = Rc::new(RefCell::new(None));
    let mut app = vmux_app::VmuxApp::new(client_holder.clone());

    launch_trace("run_main: before execute_process");
    let ret = execute_process(Some(main_args), Some(&mut app), sandbox_info);
    launch_trace(&format!("run_main: execute_process -> {ret}"));

    if ret >= 0 {
        println!("launch non-browser process");
        launch_trace("run_main: exiting (subprocess)");
        return;
    }
    println!("launch browser process");
    assert_eq!(ret, -1, "cannot execute browser process");

    let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let root_cache_path = format!("{home_dir}/.local/share/vmux-cef");
    let settings = Settings {
        no_sandbox: !cfg!(feature = "sandbox") as _,
        root_cache_path: CefString::from(root_cache_path.as_str()),
        windowless_rendering_enabled: true as _,
        external_message_pump: true as _,
        ..Default::default()
    };

    // macOS: `applicationDidFinishLaunching` is delivered once. If we call `cef::initialize`
    // before `EventLoop::new()`, `VmuxAppDelegate` (or default) is NSApp's delegate and may receive
    // launch; then Winit replaces the delegate and never sees `didFinishLaunching`, so
    // `pump_app_events` blocks forever in `-[NSApplication run]` and `ApplicationHandler::resumed`
    // never runs. Build the Winit event loop (and its `NSApplicationDelegate`) first.
    launch_trace("run_main: building winit EventLoop (before cef::initialize)");
    let mut event_loop = {
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            // Bundled apps otherwise skip `setActivationPolicy(Regular)`; window may never appear.
            // (Avoid calling `focus_window` during `resumed`—that has crashed AppKit in this setup.)
            EventLoop::builder()
                .with_activation_policy(ActivationPolicy::Regular)
                .with_default_menu(false)
                .build()
                .expect("vmux: EventLoop::build")
        }
        #[cfg(not(target_os = "macos"))]
        {
            EventLoop::new().expect("vmux: EventLoop::new")
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);

    launch_trace("run_main: before cef::initialize");
    assert_eq!(
        initialize(
            Some(main_args),
            Some(&settings),
            Some(&mut app),
            sandbox_info,
        ),
        1
    );
    launch_trace("run_main: cef::initialize OK");

    let osr_attach = vmux_osr::hub::VmuxOsrAttach::new();
    vmux_osr::bootstrap::init_client_with_osr(client_holder.as_ref(), osr_attach.clone());
    launch_trace("run_main: OSR bootstrap (wgpu + client) OK");

    // Do not call `mac::setup_vmux_app_delegate` here: it sets `VmuxAppDelegate` as NSApp delegate
    // and would replace Winit's delegate, breaking `pump_app_events` as above. `VmuxApplication`
    // remains the `NSApplication` subclass from `load_cef`. MainMenu.xib / delegate menu wiring is
    // skipped until we can compose delegates with Winit.

    launch_trace("run_main: entering pump loop");

    let mut osr_app = vmux_osr::VmuxOsrApp::new(client_holder, osr_attach);
    loop {
        do_message_loop_work();
        osr_app.pump_macos_shell_refocus();
        // One pending shell per outer tick: never call `browser_host_create_browser` again until
        // `on_after_created` has popped the matching shell from `shell_fifo` (see `finish_next`).
        // Extra `do_message_loop_work` after each create helps Chromium settle before the next pump.
        let issued_create = osr_app.finish_next_pending_browser_if_any();
        if issued_create {
            for _ in 0..24 {
                do_message_loop_work();
            }
        }
        if shutdown.load(std::sync::atomic::Ordering::Acquire) {
            launch_trace("run_main: shutdown flag set, leaving pump loop");
            break;
        }
        let status = event_loop.pump_app_events(Some(Duration::ZERO), &mut osr_app);
        if let PumpStatus::Exit(_code) = status {
            launch_trace("run_main: PumpStatus::Exit, leaving pump loop");
            break;
        }
        osr_app.pump_macos_shell_refocus();
        // Higher tick rate improves input/scroll smoothness (trackpads can be 120Hz).
        thread::sleep(Duration::from_millis(1000 / 120));
    }

    launch_trace("run_main: quit_message_loop then cef::shutdown");
    cef::quit_message_loop();
    cef::shutdown();
    launch_trace("run_main: cef::shutdown done");
}
