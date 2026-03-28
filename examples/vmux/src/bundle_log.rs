//! Persistent Bevy / tracing logs beside the `.app` (same directory as `debug.log` and
//! `vmux-rust-panic.log`). Override with **`VMUX_BEVY_LOG`** (absolute or relative path).
//!
//! **Where to look:** for `…/Foo.app`, the default file is **`…/vmux-bevy.log`** (sibling of the
//! bundle folder), not inside `Contents/`. A one-line pointer **`vmux-bevy-log-path.txt`** is written
//! beside it with the resolved path. Launching from Finder hides stderr; if creating that file
//! fails (permissions / sandbox), logging falls back to **`$TMPDIR/vmux-bevy-<pid>.log`**.
//!
//! Always-on quit/OSR breadcrumbs (no env): **`vmux-runtime-events.log`** and
//! **`vmux-runtime-events-last.txt`** from [`crate::lifecycle_trace::record_runtime_event`].

use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bevy_app::App;
use bevy_log::tracing_subscriber::{
    filter::{FromEnvError, ParseError},
    fmt,
    EnvFilter, Layer,
};
use bevy_log::{BoxedLayer, Level};

use crate::bundle_paths::log_dir_beside_exe;

/// Same filter string as [`crate::main`] passes to [`bevy_log::LogPlugin::filter`].
pub fn vmux_log_filter_string() -> String {
    format!(
        "vmux=info,bevy_app=info,bevy_ecs=info,bevy_render=info,{}",
        bevy_log::DEFAULT_FILTER
    )
}

fn file_env_filter() -> EnvFilter {
    let default_filter = format!("{},{}", Level::INFO, vmux_log_filter_string());
    EnvFilter::try_from_default_env()
        .or_else(|from_env_error| {
            _ = from_env_error
                .source()
                .and_then(|source| source.downcast_ref::<ParseError>())
                .map(|parse_err| {
                    eprintln!("vmux file LogPlugin: failed to parse RUST_LOG: {parse_err}");
                });
            Ok::<EnvFilter, FromEnvError>(
                EnvFilter::builder().parse_lossy(&default_filter),
            )
        })
        .unwrap()
}

/// Path for the Bevy/tracing log file (see module docs).
/// Writes **`vmux-cef-log-path.txt`** next to the resolved Chromium log file (same directory as
/// [`vmux_bevy_log_path`] when both use the default bundle-adjacent layout).
pub fn write_cef_log_path_pointer(cef_log_path: &Path) {
    let Some(parent) = cef_log_path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);
    let pointer = parent.join("vmux-cef-log-path.txt");
    let line = cef_log_path
        .canonicalize()
        .unwrap_or_else(|_| cef_log_path.to_path_buf())
        .display()
        .to_string();
    let _ = std::fs::write(&pointer, format!("{line}\n"));
}

pub fn vmux_bevy_log_path() -> PathBuf {
    if let Ok(p) = std::env::var("VMUX_BEVY_LOG") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Some(dir) = log_dir_beside_exe() {
        return dir.join("vmux-bevy.log");
    }
    PathBuf::from("vmux-bevy.log")
}

fn try_append_log_file(path: &Path) -> Option<std::fs::File> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// Pass to [`bevy_log::LogPlugin::custom_layer`]. Appends filtered tracing output to [`vmux_bevy_log_path`].
pub fn vmux_bevy_file_custom_layer(_app: &mut App) -> Option<BoxedLayer> {
    let primary = vmux_bevy_log_path();
    let pid = std::process::id();
    let tmp_fallback = std::env::temp_dir().join(format!("vmux-bevy-{pid}.log"));

    let (used_path, mut file) = if let Some(f) = try_append_log_file(&primary) {
        (primary, f)
    } else if let Some(f) = try_append_log_file(&tmp_fallback) {
        eprintln!(
            "vmux: could not open {} for logging — using {}",
            primary.display(),
            tmp_fallback.display()
        );
        (tmp_fallback, f)
    } else {
        eprintln!(
            "vmux: could not open log file (tried {} and {})",
            primary.display(),
            tmp_fallback.display()
        );
        return None;
    };

    let path_line = std::fs::canonicalize(&used_path)
        .unwrap_or_else(|_| used_path.clone())
        .display()
        .to_string();
    let _ = writeln!(
        file,
        "\n--- vmux-bevy log session pid={pid} path={path_line} ---"
    );
    let _ = file.sync_all();
    // Tiny sibling file so Finder users can locate the real log without digging inside the .app.
    if let Some(dir) = used_path.parent() {
        let pointer = dir.join("vmux-bevy-log-path.txt");
        let _ = std::fs::write(&pointer, format!("{path_line}\n"));
    }
    let writer = Mutex::new(file);
    let filter = file_env_filter();
    Some(Box::new(
        fmt::layer()
            .with_ansi(false)
            .with_writer(writer)
            .with_filter(filter),
    ))
}
