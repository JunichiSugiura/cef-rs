use std::sync::{Arc, Mutex, Weak};
use winit::window::Window;

static TRACKED: Mutex<Vec<Weak<Window>>> = Mutex::new(Vec::new());

pub fn track_window(window: &Arc<Window>) {
    let mut g = TRACKED.lock().expect("vmux-osr window registry");
    g.retain(|w| w.strong_count() > 0);
    g.push(Arc::downgrade(window));
}

pub fn show_all_windows() {
    let g = TRACKED.lock().expect("vmux-osr window registry");
    for w in g.iter().filter_map(Weak::upgrade) {
        w.set_visible(true);
    }
}
