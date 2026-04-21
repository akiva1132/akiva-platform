//! Akiva framework.
//!
//! Analogous to the AOSP system_server: sits on top of the kernel and
//! implements all the user-visible services — activity lifecycle, package
//! installation, window stacking, resource resolution, intent routing,
//! permission enforcement, AKML layout, widget tree, compositor.
//!
//! Every subsystem is its own module with a clearly-scoped API. The kernel
//! is the only legitimate caller into the framework at the top level;
//! subsystems talk to each other through typed module APIs, never via
//! globals.

pub mod activity;
pub mod activity_manager;
pub mod akml;
pub mod compositor;
pub mod intent;
pub mod layout;
pub mod manifest;
pub mod package_manager;
pub mod permissions;
pub mod resource_manager;
pub mod shell;
pub mod storage;
pub mod wasm_activity;
pub mod widgets;
pub mod window_manager;

use activity::{ActivityContext, KeyEvent, PointerEvent, SurfaceMetrics};
use activity_manager::{ActivityId, ActivityManager, LaunchMode, LifecycleState};
use compositor::{Compositor, FrameCommands};
use package_manager::LoadError;
use shell::ShellActivity;
use storage::Storage;

/// Summary the kernel prints after `boot` returns. Keeps logging out of
/// the framework — the framework has no host log binding and we don't
/// want to invent one just for a single line at startup.
#[derive(Debug, Default)]
pub struct BootReport {
    /// Number of packages successfully rehydrated from storage.
    pub packages_loaded: usize,
    /// Per-package load failures, `(package_id, error_message)`. Empty on
    /// a clean first boot. The kernel logs these at warn level.
    pub package_errors: Vec<(String, String)>,
}

/// Framework boot. Called once by the kernel during `akiva_boot` after
/// the display surface is live and persistent storage is loaded.
///
/// `boot_count` feeds the Shell a visible "boot #N" so the kernel's
/// storage smoke test is observable end-to-end. `storage` is the KV
/// store the framework uses for the package index and per-package
/// manifests — borrowed only for the duration of boot; post-boot calls
/// that need persistence take it explicitly.
pub fn boot(boot_count: u32, storage: &mut dyn Storage) -> (Framework, BootReport) {
    let mut fw = Framework {
        activities: ActivityManager::new(),
        packages: package_manager::PackageManager::new(),
        windows: window_manager::WindowManager::new(),
        resources: resource_manager::ResourceManager::new(),
        intents: intent::IntentRouter::new(),
        permissions: permissions::PermissionController::new(),
        compositor: Compositor::new(),
        metrics: SurfaceMetrics::default(),
        pointer: None,
        active: None,
    };

    // Rehydrate installed packages from storage. A corrupt manifest is
    // reported but not fatal — the Shell and healthy apps stay usable.
    let mut package_errors: Vec<(String, String)> = Vec::new();
    let packages_loaded = fw.packages.load_installed(storage, |id, e: LoadError| {
        package_errors.push((id.to_string(), format!("{e:?}")));
    });
    let report = BootReport { packages_loaded, package_errors };

    // Register + launch the system shell. Failure here is a boot-time
    // bug, not a runtime condition — panic is appropriate.
    fw.activities.register(shell::PACKAGE_ID, shell::ENTRY_CLASS, LaunchMode::SingleTop, move || {
        Box::new(ShellActivity::new(boot_count))
    });
    let shell_id = fw.activities
        .launch(shell::PACKAGE_ID, shell::ENTRY_CLASS)
        .expect("shell launch");
    fw.active = Some(shell_id);
    fw.drive_to_resumed(shell_id, 0.0);
    (fw, report)
}

/// Aggregate handle exposed to the kernel. All cross-subsystem calls go
/// through `&mut Framework` so ownership stays linear and no subsystem
/// reaches around the back.
pub struct Framework {
    pub activities: ActivityManager,
    pub packages: package_manager::PackageManager,
    pub windows: window_manager::WindowManager,
    pub resources: resource_manager::ResourceManager,
    pub intents: intent::IntentRouter,
    pub permissions: permissions::PermissionController,
    pub compositor: Compositor,
    metrics: SurfaceMetrics,
    pointer: Option<PointerEvent>,
    /// Currently foreground activity. Phase 2a tracks exactly one; Phase
    /// 2b replaces this with a proper task-stack-aware pick.
    active: Option<ActivityId>,
}

impl Framework {
    pub fn set_metrics(&mut self, m: SurfaceMetrics) {
        self.metrics = m;
        if let Some(id) = self.active {
            let metrics = self.metrics;
            let pointer = self.pointer;
            let mut finish = false;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: 0.0, pointer,
                finish_requested: &mut finish,
            };
            self.activities.with_instance(id, |a| a.on_resize(&mut ctx, m));
            if finish { self.finish_active(0.0); }
        }
    }

    /// Dispatch a pointer event to the active activity. The framework
    /// tracks the latest pointer state so callbacks that inspect `ctx`
    /// see a consistent view.
    pub fn dispatch_pointer(&mut self, ev: PointerEvent, ts: f64) {
        self.pointer = Some(ev);
        let Some(id) = self.active else { return; };
        let metrics = self.metrics;
        let mut finish = false;
        let mut ctx = ActivityContext {
            metrics, timestamp_ms: ts, pointer: Some(ev),
            finish_requested: &mut finish,
        };
        self.activities.with_instance(id, |a| a.on_pointer(&mut ctx, ev));
        if finish { self.finish_active(ts); }
    }

    pub fn dispatch_key(&mut self, ev: KeyEvent, ts: f64) {
        let Some(id) = self.active else { return; };
        let metrics = self.metrics;
        let pointer = self.pointer;
        let mut finish = false;
        let mut ctx = ActivityContext {
            metrics, timestamp_ms: ts, pointer,
            finish_requested: &mut finish,
        };
        self.activities.with_instance(id, |a| a.on_key(&mut ctx, ev));
        if finish { self.finish_active(ts); }
    }

    /// Ask the active activity to paint one frame. Returns the freshly
    /// populated FrameCommands, which the kernel flushes to the HAL.
    pub fn render_frame(&mut self, ts: f64) -> &FrameCommands {
        self.compositor.begin_frame();
        if let Some(id) = self.active {
            let metrics = self.metrics;
            let pointer = self.pointer;
            let mut finish = false;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
            };
            // Explicit split so the compiler sees `compositor` and
            // `activities` as disjoint mut borrows of self.
            let activities = &mut self.activities;
            let target = self.compositor.paint_target();
            activities.with_instance(id, |a| a.render(&mut ctx, target));
            if finish { self.finish_active(ts); }
        }
        self.compositor.finish_frame()
    }

    // ── Internals ─────────────────────────────────────────────────────

    fn drive_to_resumed(&mut self, id: ActivityId, ts: f64) {
        let metrics = self.metrics;
        let pointer = self.pointer;
        self.activities.resume(id, |a, new_state| {
            let mut finish = false;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
            };
            match new_state {
                LifecycleState::Created => a.on_create(&mut ctx),
                LifecycleState::Started => a.on_start(&mut ctx),
                LifecycleState::Resumed => a.on_resume(&mut ctx),
                _ => {}
            }
            // `finish` during bring-up is ignored — an activity that wants
            // out has to wait until it's fully alive to say so.
        });
    }

    fn finish_active(&mut self, ts: f64) {
        let Some(id) = self.active.take() else { return; };
        let metrics = self.metrics;
        let pointer = self.pointer;
        self.activities.destroy(id, |a, new_state| {
            let mut finish = false; // ignored during teardown
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
            };
            match new_state {
                LifecycleState::Paused => a.on_pause(&mut ctx),
                LifecycleState::Stopped => a.on_stop(&mut ctx),
                LifecycleState::Destroyed => a.on_destroy(&mut ctx),
                _ => {}
            }
        });
    }
}
