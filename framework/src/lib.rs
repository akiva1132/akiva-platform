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

use std::rc::Rc;

use activity::{ActivityContext, KeyEvent, LaunchRequest, PointerEvent, SurfaceMetrics};
use activity_manager::{ActivityId, ActivityManager, LaunchMode, LifecycleState};
use compositor::{Compositor, FrameCommands};
use package_manager::LoadError;
use shell::{AppEntry, ShellActivity};
use storage::Storage;
use wasm_activity::{WasmActivity, WasmHost};

/// Browser `KeyboardEvent.keyCode` for Escape. Phase 2b.5 uses this as
/// the universal Back key — if the foreground has something beneath it
/// on the stack, Escape pops instead of reaching the active activity.
/// A real key-map lives in the HAL in a later phase.
const KEY_CODE_BACK: u32 = 27;

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
/// that need persistence take it explicitly. `host` is the wasm-app
/// runtime seam: framework-side `WasmActivity` factories call it to
/// instantiate and drive user-app wasm modules. The framework never
/// touches app memory directly.
pub fn boot(
    boot_count: u32,
    storage: &mut dyn Storage,
    host: Rc<dyn WasmHost>,
) -> (Framework, BootReport) {
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
        stack: Vec::new(),
    };

    // Rehydrate installed packages from storage. A corrupt manifest is
    // reported but not fatal — the Shell and healthy apps stay usable.
    let mut package_errors: Vec<(String, String)> = Vec::new();
    let packages_loaded = fw.packages.load_installed(storage, |id, e: LoadError| {
        package_errors.push((id.to_string(), format!("{e:?}")));
    });
    let report = BootReport { packages_loaded, package_errors };

    // Register every installed package's entry activity as a
    // `WasmActivity` factory. The host owns the wasm process (one per
    // package); each `launch()` hands the framework a fresh handle
    // identifying one activity slot inside that process. Collecting
    // first avoids borrowing `fw.packages` while we mutate
    // `fw.activities`.
    let entries: Vec<(String, String, LaunchMode)> = fw.packages.list()
        .map(|r| {
            let launch_mode = r.launch_mode_of(&r.entry_class).unwrap_or(LaunchMode::Standard);
            (r.id.clone(), r.entry_class.clone(), launch_mode)
        })
        .collect();
    for (pkg, class, launch_mode) in entries {
        let host_for_factory = host.clone();
        let pkg_for_factory = pkg.clone();
        let class_for_factory = class.clone();
        fw.activities.register(&pkg, &class, launch_mode, move || {
            let handle = host_for_factory
                .register(&pkg_for_factory, &class_for_factory)
                .expect("wasm app register failed");
            Box::new(WasmActivity::new(handle, host_for_factory.clone()))
        });
    }

    // Build the Shell's launcher entries from whatever user packages we
    // just rehydrated. Sorted by id for a stable boot-to-boot order.
    // Skipping the Shell itself so it doesn't offer "launch yourself".
    let mut launcher: Vec<AppEntry> = fw.packages.list()
        .filter(|r| r.id != shell::PACKAGE_ID)
        .map(|r| AppEntry {
            package_id: r.id.clone(),
            class: r.entry_class.clone(),
            name: r.name.clone(),
        })
        .collect();
    launcher.sort_by(|a, b| a.package_id.cmp(&b.package_id));

    // Register + launch the system shell. Must come last so a hypothetical
    // user package claiming the shell's id can't shadow it. Failure here
    // is a boot-time bug, not a runtime condition — panic is appropriate.
    fw.activities.register(shell::PACKAGE_ID, shell::ENTRY_CLASS, LaunchMode::SingleTop, move || {
        Box::new(ShellActivity::new(boot_count, launcher.clone()))
    });
    let shell_id = fw.activities
        .launch(shell::PACKAGE_ID, shell::ENTRY_CLASS)
        .expect("shell launch");
    fw.stack.push(shell_id);
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
    /// Foreground task stack, bottom-to-top. The Shell always sits at
    /// position 0; user apps push on top. Only the top receives input and
    /// render calls. Activities below the top are `Paused` (not stopped,
    /// not destroyed), so popping back to them is free.
    stack: Vec<ActivityId>,
}

impl Framework {
    pub fn set_metrics(&mut self, m: SurfaceMetrics) {
        self.metrics = m;
        let Some(id) = self.stack.last().copied() else { return; };
        let (post, _) = self.with_ctx(id, 0.0, |a, ctx| a.on_resize(ctx, m));
        self.apply_post(post, 0.0);
    }

    /// Dispatch a pointer event to the active activity. The framework
    /// tracks the latest pointer state so callbacks that inspect `ctx`
    /// see a consistent view.
    pub fn dispatch_pointer(&mut self, ev: PointerEvent, ts: f64) {
        self.pointer = Some(ev);
        let Some(id) = self.stack.last().copied() else { return; };
        let (post, _) = self.with_ctx(id, ts, |a, ctx| a.on_pointer(ctx, ev));
        self.apply_post(post, ts);
    }

    pub fn dispatch_key(&mut self, ev: KeyEvent, ts: f64) {
        // Back key short-circuit: when something sits above the Shell,
        // Escape pops instead of reaching the app. This matches the
        // Android convention of a global Back button and spares each
        // app from having to implement "close me" themselves. Only
        // `Down` triggers so a held-down Escape doesn't rapid-fire.
        if ev.code == KEY_CODE_BACK
            && matches!(ev.phase, activity::KeyPhase::Down)
            && self.stack.len() > 1
        {
            self.finish_top(ts);
            return;
        }
        let Some(id) = self.stack.last().copied() else { return; };
        let (post, _) = self.with_ctx(id, ts, |a, ctx| a.on_key(ctx, ev));
        self.apply_post(post, ts);
    }

    /// Ask the active activity to paint one frame. Returns the freshly
    /// populated FrameCommands, which the kernel flushes to the HAL.
    pub fn render_frame(&mut self, ts: f64) -> &FrameCommands {
        self.compositor.begin_frame();
        if let Some(id) = self.stack.last().copied() {
            let metrics = self.metrics;
            let pointer = self.pointer;
            let mut finish = false;
            let mut launch: Option<LaunchRequest> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
            };
            // Explicit split so the compiler sees `compositor` and
            // `activities` as disjoint mut borrows of self.
            let activities = &mut self.activities;
            let target = self.compositor.paint_target();
            activities.with_instance(id, |a| a.render(&mut ctx, target));
            self.apply_post(PostCallback { finish, launch }, ts);
        }
        self.compositor.finish_frame()
    }

    // ── Internals ─────────────────────────────────────────────────────

    /// Build a fresh `ActivityContext` around `id`, run `f` against the
    /// live instance, and return what the activity requested via the
    /// `finish_requested` / `launch_requested` out-params. Having this
    /// helper keeps every dispatch site on the same shape and makes it
    /// impossible to forget to drain either request.
    fn with_ctx<R>(
        &mut self,
        id: ActivityId,
        ts: f64,
        f: impl FnOnce(&mut dyn activity::Activity, &mut ActivityContext<'_>) -> R,
    ) -> (PostCallback, Option<R>) {
        let metrics = self.metrics;
        let pointer = self.pointer;
        let mut finish = false;
        let mut launch: Option<LaunchRequest> = None;
        let mut result: Option<R> = None;
        {
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
            };
            self.activities.with_instance(id, |a| {
                result = Some(f(a, &mut ctx));
            });
        }
        (PostCallback { finish, launch }, result)
    }

    /// Consume the activity's post-callback requests in a deterministic
    /// order: launches first (so the requester is paused before it sees
    /// any further input), then finish. If an activity sets both in one
    /// callback, launch wins — finishing a just-paused activity would
    /// require a Paused→Destroyed edge with no Stopped step, which we
    /// don't exercise yet.
    fn apply_post(&mut self, post: PostCallback, ts: f64) {
        if let Some(req) = post.launch {
            self.launch_app(&req.package_id, &req.class, ts);
        } else if post.finish {
            self.finish_top(ts);
        }
    }

    fn drive_to_resumed(&mut self, id: ActivityId, ts: f64) {
        let metrics = self.metrics;
        let pointer = self.pointer;
        self.activities.resume(id, |a, new_state| {
            let mut finish = false;
            let mut launch: Option<LaunchRequest> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
            };
            match new_state {
                LifecycleState::Created => a.on_create(&mut ctx),
                LifecycleState::Started => a.on_start(&mut ctx),
                LifecycleState::Resumed => a.on_resume(&mut ctx),
                _ => {}
            }
            // `finish`/`launch` during bring-up are ignored — an activity
            // that wants to redirect has to wait until it's fully alive.
        });
    }

    fn drive_to_paused(&mut self, id: ActivityId, ts: f64) {
        let metrics = self.metrics;
        let pointer = self.pointer;
        self.activities.pause(id, |a, new_state| {
            let mut finish = false;
            let mut launch: Option<LaunchRequest> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
            };
            if matches!(new_state, LifecycleState::Paused) {
                a.on_pause(&mut ctx);
            }
        });
    }

    /// Push a new activity on top of the stack. The previous top is paused
    /// (but kept in memory) so popping back to it is free. Registration
    /// failures here indicate a buggy Shell handing us a stale entry — we
    /// log nothing (no log binding in framework) and swallow so the Shell
    /// survives the miss.
    fn launch_app(&mut self, package_id: &str, class: &str, ts: f64) {
        let new_id = match self.activities.launch(package_id, class) {
            Ok(id) => id,
            Err(_) => return,
        };
        // Reusing a SingleTop top is a no-op: the launched id equals the
        // current top, so don't pause it and don't stack it twice.
        if Some(new_id) == self.stack.last().copied() { return; }
        if let Some(prev) = self.stack.last().copied() {
            self.drive_to_paused(prev, ts);
        }
        self.stack.push(new_id);
        self.drive_to_resumed(new_id, ts);
    }

    /// Finish the top of the stack and resume whatever it was covering.
    /// If the stack is already down to its last entry (the Shell), this
    /// is a no-op — we never destroy the home-screen.
    fn finish_top(&mut self, ts: f64) {
        if self.stack.len() <= 1 { return; }
        let id = self.stack.pop().expect("checked len > 1");
        let metrics = self.metrics;
        let pointer = self.pointer;
        self.activities.destroy(id, |a, new_state| {
            let mut finish = false;
            let mut launch: Option<LaunchRequest> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
            };
            match new_state {
                LifecycleState::Paused => a.on_pause(&mut ctx),
                LifecycleState::Stopped => a.on_stop(&mut ctx),
                LifecycleState::Destroyed => a.on_destroy(&mut ctx),
                _ => {}
            }
        });
        if let Some(new_top) = self.stack.last().copied() {
            self.drive_to_resumed(new_top, ts);
        }
    }
}

/// What an activity's callback asked the framework to do next. Collected
/// before we re-enter activity code so the borrow on `self` from the
/// dispatch closure is fully released before we mutate the stack.
struct PostCallback {
    finish: bool,
    launch: Option<LaunchRequest>,
}
