use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bevy_app::{App, Plugin, Startup};
use bevy_ecs::schedule::IntoSystemConfigs;
use bevy_ecs::prelude::Resource;
use ::cef::*;
#[cfg(target_os = "macos")]
use winit::window::Window;

use crate::browser::BrowserClient;
use crate::browser::backend::osr::foreign_index::ForeignOsrIndex;
use crate::browser::backend::osr::hub::CefAttach;
use crate::browser::backend::osr::render::{CefRenderHandler, CefRenderInner};
use crate::browser::renderer::gpu::device::SharedGpu;
use crate::browser::browser_entity::CefBrowserHandles;
use crate::browser::handler_runtime::{BrowserCloseGuardsResource, BrowserLifecycleResource};
use crate::browser::shell_ops::BrowserUiOpQueue;
use crate::browser::view_state::DomFocusAncestorWalkMax;
use crate::browser::renderer::osr_host::state::OsrHostState;
use crate::browser::event_loop::{EditableFocusQueues, AppUserEvent};

#[cfg(target_os = "macos")]
pub type Library = cef::library_loader::LibraryLoader;

#[cfg(not(target_os = "macos"))]
pub struct Library;

pub fn load_cef() -> Library {
    #[cfg(target_os = "macos")]
    let library = {
        let exe = std::env::current_exe().unwrap();
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "load_cef: exe={}",
            exe.display()
        );
        let loader = cef::library_loader::LibraryLoader::new(&exe, false);
        assert!(
            loader.load(),
            "cef: failed to load Chromium Embedded Framework (bundle ../Frameworks or CEF_FRAMEWORK_PATH)"
        );
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "load_cef: framework load OK"
        );
        loader
    };
    #[cfg(not(target_os = "macos"))]
    let library = Library;

    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    library
}

pub struct CefStartupState {
    pub cef_app: cef::App,
    pub client_holder: std::rc::Rc<std::cell::RefCell<Option<Client>>>,
    pub cef_attach: CefAttach,
    pub event_proxy: winit::event_loop::EventLoopProxy<AppUserEvent>,
}

pub struct CefPlugin {}

impl Default for CefPlugin {
    fn default() -> Self {
        Self {}
    }
}

impl CefPlugin {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Plugin for CefPlugin {
    fn build(&self, app: &mut App) {
        // After [`crate::settings::VmuxSettingsStartup`] so [`SettingsResource`] exists (see [`crate::settings::SettingsPlugin`]).
        app.add_systems(
            Startup,
            startup_browser_runtime_system.after(crate::settings::VmuxSettingsStartup),
        );
    }
}

/// WGPU device/queue shared with OSR; same `Arc` as [`crate::browser::event_loop::foreign_gpu`].
/// Bevy systems use `Res<GpuResource>` after GPU registration; `foreign_gpu()` is for CEF callbacks
/// and helpers that run without `World` access.
///
/// **macOS:** inserted after the first `winit` `resumed` so Metal matches **`examples/osr`**
/// (adapter chosen with a real window surface via [`init_browser_client_macos_after_first_window`],
/// not headless-before-`NSWindow`).
///
/// See [`crate::browser::renderer::gpu`] for how this relates to `bevy_render` (Branch B: vmux/CEF stay on
/// workspace `wgpu` 28; a minimal wgpu 23 context exists only for `bevy_render::render_graph::Node` plumbing).
#[derive(Resource, Clone)]
pub struct GpuResource(pub Arc<SharedGpu>);

/// OSR foreign tab index; same `Arc` as [`crate::browser::event_loop::foreign_osr_index`].
#[derive(Resource, Clone)]
pub struct ForeignOsrIndexResource(pub Arc<ForeignOsrIndex>);

impl ForeignOsrIndexResource {
    pub fn inner(&self) -> &Arc<ForeignOsrIndex> {
        &self.0
    }
}

#[derive(Resource, Clone)]
pub struct DeviceScaleFactorResource(pub Arc<Mutex<f32>>);

pub fn set_device_scale_factor(dsf: f32) {
    if let Ok(mut g) = crate::browser::event_loop::foreign_device_scale_factor().lock() {
        *g = dsf.max(0.5);
    }
}

/// Chromium log path for [`cef::Settings::log_file`].
///
/// - `VMUX_CEF_LOG` — absolute or relative path (always wins).
/// - macOS app bundle: `…/target/bundle/vmux.app/Contents/MacOS/vmux` → `…/target/bundle/debug.log`
/// - Otherwise: `debug.log` next to the executable (e.g. `target/debug/debug.log`).
fn vmux_cef_log_file_path() -> PathBuf {
    if let Ok(p) = std::env::var("VMUX_CEF_LOG") {
        let path = PathBuf::from(p.trim());
        if !path.as_os_str().is_empty() {
            return path;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(beside_app) = cef_log_beside_macos_app_bundle(&exe) {
            return beside_app;
        }
        if let Some(dir) = exe.parent() {
            return dir.join("debug.log");
        }
    }
    PathBuf::from("debug.log")
}

/// `…/Foo.app/Contents/MacOS/exe` → `…/Foo.app/../debug.log` (same dir as the `.app`).
fn cef_log_beside_macos_app_bundle(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    if macos.file_name() != Some(OsStr::new("MacOS")) {
        return None;
    }
    let contents = macos.parent()?;
    let app = contents.parent()?;
    if app.extension() != Some(OsStr::new("app")) {
        return None;
    }
    app.parent().map(|d| d.join("debug.log"))
}

/// `…/Foo.app/Contents/MacOS/foo` → absolute paths for [`cef::Settings`] (`framework_dir_path`,
/// `main_bundle_path`, optional `browser_subprocess_path`). CEF defaults usually work, but explicit
/// canonical paths avoid subtle helper / Mach rendezvous failures when the working directory or
/// bundle layout is ambiguous.
#[cfg(target_os = "macos")]
fn macos_bundled_cef_settings_paths(exe: &Path) -> Option<(String, String, Option<String>)> {
    let macos = exe.parent()?;
    if macos.file_name() != Some(OsStr::new("MacOS")) {
        return None;
    }
    let contents = macos.parent()?;
    let app = contents.parent()?;
    if app.extension() != Some(OsStr::new("app")) {
        return None;
    }
    let app = app.canonicalize().ok()?;
    let framework_dir = app.join("Contents/Frameworks/Chromium Embedded Framework.framework");
    let framework_dir = framework_dir.canonicalize().ok()?;
    let exe_stem = exe.file_name()?.to_str()?;
    let helper_exe = app.join(format!(
        "Contents/Frameworks/{exe_stem} Helper.app/Contents/MacOS/{exe_stem} Helper"
    ));
    let subprocess = helper_exe
        .canonicalize()
        .ok()
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned());
    Some((
        framework_dir.to_string_lossy().into_owned(),
        app.to_string_lossy().into_owned(),
        subprocess,
    ))
}

#[cfg(target_os = "macos")]
fn warn_macos_icudtl_if_bundle_incomplete() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(macos) = exe.parent() else {
        return;
    };
    if macos.file_name() != Some(OsStr::new("MacOS")) {
        return;
    }
    let Some(contents) = macos.parent() else {
        return;
    };
    let Some(app) = contents.parent() else {
        return;
    };
    if app.extension() != Some(OsStr::new("app")) {
        return;
    }
    let icu = app.join("Contents/Resources/icudtl.dat");
    if icu.is_file() {
        return;
    }
    let fw_icu = app.join(
        "Contents/Frameworks/Chromium Embedded Framework.framework/Resources/icudtl.dat",
    );
    bevy_log::warn!(
        target: "vmux",
        pid = std::process::id(),
        "cef: {} missing — copy from {} then re-launch, or re-run bundle-cef-app (it adds this file).",
        icu.display(),
        fw_icu.display()
    );
}

/// macOS: run from Bevy `Startup` **before** any `NSWindow` exists — registers OSR index + scale and
/// CEF foreign handles, but **no** GPU / `Client` yet (see [`init_browser_client_macos_after_first_window`]).
#[cfg(target_os = "macos")]
fn init_browser_client_macos_prepare(
    client_cell: &std::cell::RefCell<Option<Client>>,
    cef_attach: CefAttach,
    cef_handles: std::sync::Arc<std::sync::Mutex<
        crate::browser::browser_entity::CefBrowserHandlesInner,
    >>,
    lifecycle: std::sync::Arc<std::sync::Mutex<
        crate::browser::handler_runtime::BrowserLifecycleInner,
    >>,
    close_guards: std::sync::Arc<std::sync::Mutex<
        crate::browser::handler_runtime::BrowserCloseGuardsInner,
    >>,
) {
    *client_cell.borrow_mut() = None;
    let osr_index = Arc::new(ForeignOsrIndex::default());
    let dsf = Arc::new(Mutex::new(1.0f32));
    crate::browser::event_loop::register_osr_index_and_scale_for_foreign_callbacks(
        osr_index,
        dsf,
    );
    crate::browser::init_browser_runtime_globals(
        Some(cef_attach),
        cef_handles,
        lifecycle,
        close_guards,
    );
}

/// macOS: first winit window is ready — align wgpu + CEF with **`examples/osr`** (`State::new` timing).
#[cfg(target_os = "macos")]
pub(crate) fn init_browser_client_macos_after_first_window(
    client_cell: &std::cell::RefCell<Option<Client>>,
    cef_attach: CefAttach,
    window: Arc<Window>,
) {
    if client_cell.borrow().is_some() {
        return;
    }
    let gpu = Arc::new(pollster::block_on(SharedGpu::new_with_window(window)));
    crate::browser::event_loop::register_gpu_only_for_foreign_callbacks(gpu.clone());
    let osr_index = crate::browser::event_loop::foreign_osr_index();
    let dsf = crate::browser::event_loop::foreign_device_scale_factor();
    let render_inner = CefRenderInner {
        osr_index: osr_index.clone(),
        windows_attach: cef_attach.clone(),
        device: gpu.device.clone(),
        queue: gpu.queue.clone(),
        layout: gpu.texture_bind_group_layout.clone(),
        device_scale_factor: dsf.clone(),
        paint_redraw_throttle: osr_index.paint_redraw_throttle.clone(),
    };
    let rh = CefRenderHandler::build(render_inner);
    let client = BrowserClient::new(rh);
    *client_cell.borrow_mut() = Some(client);
}

#[cfg(not(target_os = "macos"))]
fn init_browser_client(
    client_cell: &std::cell::RefCell<Option<Client>>,
    cef_attach: CefAttach,
    _browser_ui_ops: BrowserUiOpQueue,
    cef_handles: std::sync::Arc<std::sync::Mutex<
        crate::browser::browser_entity::CefBrowserHandlesInner,
    >>,
    lifecycle: std::sync::Arc<std::sync::Mutex<
        crate::browser::handler_runtime::BrowserLifecycleInner,
    >>,
    close_guards: std::sync::Arc<std::sync::Mutex<
        crate::browser::handler_runtime::BrowserCloseGuardsInner,
    >>,
) {
    let gpu = pollster::block_on(SharedGpu::new_headless());
    let gpu = Arc::new(gpu);
    let osr_index = Arc::new(ForeignOsrIndex::default());
    let dsf = Arc::new(Mutex::new(1.0f32));
    // Before `BrowserClient::new`: CEF may invoke handlers during construction; `foreign_gpu()` /
    // `foreign_osr_index()` must be valid for any synchronous callback.
    crate::browser::event_loop::register_gpu_runtime_for_foreign_callbacks(
        gpu.clone(),
        osr_index.clone(),
        dsf.clone(),
    );
    let render_inner = CefRenderInner {
        osr_index: osr_index.clone(),
        windows_attach: cef_attach.clone(),
        device: gpu.device.clone(),
        queue: gpu.queue.clone(),
        layout: gpu.texture_bind_group_layout.clone(),
        device_scale_factor: dsf.clone(),
        paint_redraw_throttle: osr_index.paint_redraw_throttle.clone(),
    };
    let rh = CefRenderHandler::build(render_inner);
    crate::browser::init_browser_runtime_globals(
        Some(cef_attach),
        cef_handles,
        lifecycle,
        close_guards,
    );
    let client = BrowserClient::new(rh);
    *client_cell.borrow_mut() = Some(client);
}

fn vmux_cef_fresh_profile_from_env() -> bool {
    std::env::var("VMUX_CEF_FRESH_PROFILE")
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

/// Call after [`cef::execute_process`] returns `-1` (browser process), **before** constructing the
/// winit event loop. Matches [`examples/osr`]: CEF init runs before `EventLoop::new` / first pump.
pub fn initialize_cef_after_execute(args: &cef::args::Args, cef_app: &mut cef::App) {
    crate::lifecycle_trace::record_startup_milestone("cef_initialize_enter");
    let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let root_cache_path = vmux_root_cache_path(&home_dir);
    let _ = std::fs::create_dir_all(&root_cache_path);
    let cef_log_path = vmux_cef_log_file_path();
    if let Some(parent) = cef_log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cef_log_str = cef_log_path.to_string_lossy().into_owned();
    bevy_log::info!(
        target: "vmux",
        pid = std::process::id(),
        "cef: log_file={}",
        cef_log_str
    );
    #[cfg(target_os = "macos")]
    warn_macos_icudtl_if_bundle_incomplete();
    let mut settings = cef::Settings {
        no_sandbox: !cfg!(feature = "sandbox") as _,
        root_cache_path: cef::CefString::from(root_cache_path.as_str()),
        windowless_rendering_enabled: true as _,
        external_message_pump: true as _,
        log_file: cef::CefString::from(cef_log_str.as_str()),
        log_severity: cef::LogSeverity::INFO,
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    if let Ok(exe) = std::env::current_exe() {
        if let Some((framework_dir, main_bundle, subprocess)) = macos_bundled_cef_settings_paths(&exe)
        {
            bevy_log::info!(
                target: "vmux",
                pid = std::process::id(),
                "cef: macOS explicit bundle paths — framework_dir_path={} main_bundle_path={} browser_subprocess_path={}",
                framework_dir,
                main_bundle,
                subprocess.as_deref().unwrap_or("(default)"),
            );
            settings.framework_dir_path = cef::CefString::from(framework_dir.as_str());
            settings.main_bundle_path = cef::CefString::from(main_bundle.as_str());
            if let Some(ref p) = subprocess {
                settings.browser_subprocess_path = cef::CefString::from(p.as_str());
            }
            // Chromium resolves `.pak` / ICU relative to these; empty defaults can mis-resolve when
            // the process cwd differs from the bundle (e.g. `open Foo.app`).
            let fw_resources = PathBuf::from(&framework_dir).join("Resources");
            if fw_resources.is_dir() {
                let fw_resources = fw_resources.canonicalize().unwrap_or(fw_resources);
                let res = fw_resources.to_string_lossy().into_owned();
                settings.resources_dir_path = cef::CefString::from(res.as_str());
                settings.locales_dir_path = cef::CefString::from(res.as_str());
                bevy_log::info!(
                    target: "vmux",
                    pid = std::process::id(),
                    "cef: resources_dir_path={} locales_dir_path={}",
                    res,
                    res,
                );
                let resources_pak = fw_resources.join("resources.pak");
                if !resources_pak.is_file() {
                    bevy_log::warn!(
                        target: "vmux",
                        pid = std::process::id(),
                        "cef: expected {} missing — CEF may fail loading packed resources",
                        resources_pak.display(),
                    );
                }
            } else {
                bevy_log::warn!(
                    target: "vmux",
                    pid = std::process::id(),
                    "cef: framework Resources directory missing at {}",
                    fw_resources.display(),
                );
            }
        }
    }
    crate::bundle_log::write_cef_log_path_pointer(&cef_log_path);
    let main_args = args.as_main_args();
    assert_eq!(
        cef::initialize(
            Some(main_args),
            Some(&settings),
            Some(cef_app),
            std::ptr::null_mut(),
        ),
        1
    );
    crate::lifecycle_trace::record_startup_milestone("cef_initialize_ok");
}

fn vmux_root_cache_path(home_dir: &str) -> String {
    if let Ok(p) = std::env::var("VMUX_CEF_ROOT_CACHE") {
        let t = p.trim();
        if !t.is_empty() {
            bevy_log::info!(
                target: "vmux",
                pid = std::process::id(),
                "cef: using VMUX_CEF_ROOT_CACHE={}",
                t
            );
            return t.to_string();
        }
    }
    let base = format!("{home_dir}/.local/share/vmux-cef");
    if vmux_cef_fresh_profile_from_env() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let path = format!("{base}-run-{ms}");
        bevy_log::info!(
            target: "vmux",
            pid = std::process::id(),
            "cef: VMUX_CEF_FRESH_PROFILE — root_cache_path={path}"
        );
        return path;
    }
    base
}

fn startup_browser_runtime_system(world: &mut bevy_ecs::world::World) {
    crate::lifecycle_trace::record_startup_milestone("bevy_startup_enter");
    let state = world.non_send_resource_mut::<CefStartupState>();
    let (client_holder, cef_attach) = (state.client_holder.clone(), state.cef_attach.clone());
    drop(state);

    let key_settings = world
        .get_resource::<crate::settings::SettingsResource>()
        .map(|s| s.0.clone())
        .unwrap_or_else(crate::settings::default_key_settings);
    world.insert_resource(DomFocusAncestorWalkMax(
        key_settings.dom_focus_ancestor_walk_max,
    ));
    let editable_queues = world.resource::<EditableFocusQueues>().clone();
    let browser_ui_ops = world.resource::<BrowserUiOpQueue>().clone();
    let cef_handles = world.resource::<CefBrowserHandles>().0.clone();
    let lifecycle = world.resource::<BrowserLifecycleResource>().0.clone();
    let close_guards = world.resource::<BrowserCloseGuardsResource>().0.clone();
    let windows_store = cef_attach.windows_store.clone();
    #[cfg(target_os = "macos")]
    init_browser_client_macos_prepare(
        client_holder.as_ref(),
        cef_attach.clone(),
        cef_handles,
        lifecycle,
        close_guards,
    );
    #[cfg(not(target_os = "macos"))]
    init_browser_client(
        client_holder.as_ref(),
        cef_attach.clone(),
        browser_ui_ops.clone(),
        cef_handles,
        lifecycle,
        close_guards,
    );
    crate::lifecycle_trace::record_startup_milestone("bevy_startup_client_ready");
    let osr_host = OsrHostState::new(
        client_holder,
        cef_attach,
        key_settings,
        editable_queues,
        browser_ui_ops,
    );
    world.insert_resource(crate::browser::renderer::vmux_render::VmuxWindowsStoreResource(
        windows_store,
    ));
    #[cfg(not(target_os = "macos"))]
    world.insert_resource(GpuResource(crate::browser::event_loop::foreign_gpu()));
    world.insert_resource(ForeignOsrIndexResource(
        crate::browser::event_loop::foreign_osr_index(),
    ));
    world.insert_resource(DeviceScaleFactorResource(
        crate::browser::event_loop::foreign_device_scale_factor(),
    ));
    world.insert_non_send_resource(osr_host);
}
