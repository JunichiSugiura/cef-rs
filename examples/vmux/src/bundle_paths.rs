//! Path helpers for files stored next to a macOS `.app` bundle (same directory as `debug.log`).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// `…/Foo.app/Contents/MacOS/exe` → parent of `Foo.app` (directory containing the bundle).
pub fn beside_macos_app_bundle_parent(exe: &Path) -> Option<&Path> {
    let macos = exe.parent()?;
    if macos.file_name() != Some(OsStr::new("MacOS")) {
        return None;
    }
    let contents = macos.parent()?;
    let app = contents.parent()?;
    if app.extension() != Some(OsStr::new("app")) {
        return None;
    }
    app.parent()
}

/// Resolved directory for logs beside the bundle, or next to the executable, or `None` for cwd fallback.
pub fn log_dir_beside_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    if let Some(parent) = beside_macos_app_bundle_parent(&exe) {
        return Some(parent.to_path_buf());
    }
    exe.parent().map(Path::to_path_buf)
}
