//! When the app is opened via Finder / `open`, stderr is often lost. Write Rust panics + backtrace
//! to a file beside the `.app` (same directory as `debug.log`). Override with `VMUX_PANIC_LOG`.

use std::io::Write;
use std::path::PathBuf;

use crate::bundle_paths::log_dir_beside_exe;

/// Install first thing in `main` (before other init).
pub fn install() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = write_panic_file(info);
        default_hook(info);
    }));
}

fn write_panic_file(info: &std::panic::PanicHookInfo<'_>) -> std::io::Result<()> {
    let path = panic_log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let _ = writeln!(f);
    let _ = writeln!(
        f,
        "--- vmux Rust panic pid={} --- {}",
        std::process::id(),
        chrono_like_timestamp()
    );
    let _ = writeln!(f, "{info}");
    let bt = std::backtrace::Backtrace::capture();
    let _ = writeln!(f, "{bt}");
    let _ = writeln!(f, "--- end ---");
    Ok(())
}

fn chrono_like_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return String::new();
    };
    format!("(unix_s={})", d.as_secs())
}

fn panic_log_path() -> PathBuf {
    if let Ok(p) = std::env::var("VMUX_PANIC_LOG") {
        let p = p.trim();
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Some(dir) = log_dir_beside_exe() {
        return dir.join("vmux-rust-panic.log");
    }
    PathBuf::from("vmux-rust-panic.log")
}
