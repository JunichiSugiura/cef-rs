//! Append-only trace when **`VMUX_LIFECYCLE_TRACE=1`** (see `vmux` crate docs).
//!
//! On macOS, **`open Foo.app` often does not pass environment variables** to the GUI process.
//! Prefer: `VMUX_LIFECYCLE_TRACE=1 path/to/Foo.app/Contents/MacOS/vmux`
//!
//! ## Always-on milestones (no env)
//!
//! [`record_startup_milestone`] appends **`vmux-startup-milestone.log`** and overwrites
//! **`vmux-startup-milestone.txt`** (last line only) next to the `.app` / executable and under
//! **`$TMPDIR`**. Use this when Chromium exits with **133** / **`SIGTRAP`** and there is no Rust
//! panic — it shows the last Rust checkpoint before the framework trapped.
//!
//! [`record_runtime_event`] appends **`vmux-runtime-events.log`** (and **`vmux-runtime-events-last.txt`**)
//! for shutdown / OSR diagnostics when **`vmux-bevy.log`** is hard to correlate (same directory as
//! **`debug.log`** beside the bundle).

use std::io::Write;
use std::path::Path;

/// Synced lines to **`$TMPDIR/vmux-lifecycle-trace.txt`** and **`vmux-lifecycle-trace.txt`** beside the `.app`.
pub fn trace(msg: &str) {
    if std::env::var("VMUX_LIFECYCLE_TRACE").ok().as_deref() != Some("1") {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = format!(
        "ts_ms={ts} pid={} {msg}",
        std::process::id()
    );
    let mut paths = vec![std::env::temp_dir().join("vmux-lifecycle-trace.txt")];
    if let Some(dir) = crate::bundle_paths::log_dir_beside_exe() {
        paths.push(dir.join("vmux-lifecycle-trace.txt"));
    }
    for path in paths {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{line}");
            let _ = f.sync_all();
        }
    }
}

fn ts_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Writes startup progress to disk **without** env vars — survives Finder launches and CEF traps.
///
/// - **`vmux-startup-milestone.log`**: append one line per call.
/// - **`vmux-startup-milestone.txt`**: last line only (quick open in an editor).
///
/// Written under [`crate::bundle_paths::log_dir_beside_exe`] when known, and always under
/// **`std::env::temp_dir()`** as a fallback.
pub fn record_startup_milestone(msg: &str) {
    let line = format!("ts_ms={} pid={} {msg}\n", ts_ms(), std::process::id());
    let mut dirs = vec![std::env::temp_dir()];
    if let Some(d) = crate::bundle_paths::log_dir_beside_exe() {
        dirs.push(d);
    }
    dirs.sort();
    dirs.dedup();
    for dir in dirs {
        let _ = write_milestone_files(&dir, &line);
    }
}

fn write_milestone_files(dir: &Path, line: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let log_path = dir.join("vmux-startup-milestone.log");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        f.write_all(line.as_bytes())?;
        f.sync_all()?;
    }
    let txt_path = dir.join("vmux-startup-milestone.txt");
    std::fs::write(txt_path, line.trim_end())?;
    Ok(())
}

/// Quit / composite / pump breadcrumbs (always on). Same directories as [`record_startup_milestone`].
pub fn record_runtime_event(msg: &str) {
    let line = format!("ts_ms={} pid={} {msg}\n", ts_ms(), std::process::id());
    let mut dirs = vec![std::env::temp_dir()];
    if let Some(d) = crate::bundle_paths::log_dir_beside_exe() {
        dirs.push(d);
    }
    dirs.sort();
    dirs.dedup();
    for dir in dirs {
        let _ = write_runtime_event_files(&dir, &line);
    }
}

fn write_runtime_event_files(dir: &Path, line: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let log_path = dir.join("vmux-runtime-events.log");
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)?;
        f.write_all(line.as_bytes())?;
        f.sync_all()?;
    }
    let txt_path = dir.join("vmux-runtime-events-last.txt");
    std::fs::write(txt_path, line.trim_end())?;
    Ok(())
}
