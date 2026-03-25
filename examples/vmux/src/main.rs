#![cfg_attr(
    all(not(debug_assertions), not(feature = "sandbox"), target_os = "windows"),
    windows_subsystem = "windows"
)]

pub mod shared;

#[cfg(target_os = "macos")]
mod mac;

#[cfg(not(all(feature = "sandbox", target_os = "windows")))]
fn main() -> Result<(), &'static str> {
    shared::launch_trace("main: start");
    let _library = shared::load_cef();

    let args = cef::args::Args::new();
    shared::run_main(args.as_main_args(), std::ptr::null_mut());
    shared::launch_trace("main: run_main returned");
    Ok(())
}

#[cfg(all(feature = "sandbox", target_os = "windows"))]
fn main() -> Result<(), &'static str> {
    Err("Running in sandbox mode on Windows requires bootstrap.exe or bootstrapc.exe.")
}
