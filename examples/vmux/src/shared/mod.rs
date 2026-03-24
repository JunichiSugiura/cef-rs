//! Rust port of the [`vmux`](https://github.com/chromiumembedded/cef/tree/master/tests/vmux) example.

use cef::*;

pub mod resources;
pub mod vmux_app;
pub mod vmux_handler;

#[cfg(target_os = "macos")]
pub type Library = library_loader::LibraryLoader;

#[cfg(not(target_os = "macos"))]
pub struct Library;

#[allow(dead_code)]
pub fn load_cef() -> Library {
    #[cfg(target_os = "macos")]
    let library = {
        let loader = library_loader::LibraryLoader::new(&std::env::current_exe().unwrap(), false);
        assert!(loader.load());
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
    let ret = execute_process(Some(main_args), None, sandbox_info);

    if ret >= 0 {
        println!("launch non-browser process");
        // non-browser process does not initialize cef
        return;
    }
    println!("launch browser process");
    assert_eq!(ret, -1, "cannot execute browser process");

    let mut app = vmux_app::VmuxApp::new();

    let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let root_cache_path = format!("{home_dir}/.local/share/vmux-cef");
    let settings = Settings {
        no_sandbox: !cfg!(feature = "sandbox") as _,
        root_cache_path: CefString::from(root_cache_path.as_str()),
        ..Default::default()
    };
    assert_eq!(
        initialize(
            Some(main_args),
            Some(&settings),
            Some(&mut app),
            sandbox_info,
        ),
        1
    );

    #[cfg(target_os = "macos")]
    let _delegate = crate::mac::setup_vmux_app_delegate();

    run_message_loop();

    shutdown();
}
