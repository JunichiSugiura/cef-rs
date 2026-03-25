use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};

static VMUX_SHUTDOWN: OnceLock<Arc<AtomicBool>> = OnceLock::new();

pub fn install_shutdown_flag(flag: Arc<AtomicBool>) {
    let _ = VMUX_SHUTDOWN.set(flag);
}

pub fn shutdown_flag() -> Option<Arc<AtomicBool>> {
    VMUX_SHUTDOWN.get().cloned()
}
