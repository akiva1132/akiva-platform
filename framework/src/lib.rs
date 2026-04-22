//! Akiva framework.
//!
//! Analogous to the AOSP system_server: sits on top of the kernel and
//! implements the user-visible services — activity lifecycle, package
//! installation, window stacking, resource resolution, intent routing,
//! permission enforcement, and the paint-op compositor that flushes each
//! frame to the HAL.
//!
//! **What the framework does *not* own:** widget trees, layout math, and
//! AKML parsing. Those live in the SDK ([`sdk-rust/src/ui`] and
//! [`sdk-rust/src/akml`]) and are linked into each app's wasm binary.
//! The framework only speaks `PaintOp` across the ABI boundary — apps
//! build their widget tree, measure and place it, emit paint ops, and
//! hand them to the framework. Keeping widgets app-side preserves a
//! narrow, stable ABI and lets apps evolve their UI independently of
//! the framework version.
//!
//! Every subsystem is its own module with a clearly-scoped API. The kernel
//! is the only legitimate caller into the framework at the top level;
//! subsystems talk to each other through typed module APIs, never via
//! globals.

pub mod activity;
pub mod activity_manager;
pub mod compositor;
pub mod intent;
pub mod manifest;
pub mod package_manager;
pub mod permissions;
pub mod resources;
pub mod shell;
pub mod storage;
pub mod wasm_activity;
pub mod window_manager;

use std::rc::Rc;

use activity::{ActivityContext, KeyEvent, PointerEvent, SurfaceMetrics};
use activity_manager::{ActivityId, ActivityManager, LaunchMode, LifecycleState};
use compositor::{Compositor, FrameCommands};
use intent::{Intent, IntentFilter};
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
    /// Per-package resource-file load failures. Distinct from
    /// `package_errors` because a broken `res/values/*.toml` doesn't
    /// prevent the package from launching — the app just shows
    /// placeholders where the refs would have resolved. Reported so
    /// `akiva run`'s log has the breadcrumb for a typo.
    pub resource_errors: Vec<(String, String)>,
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
    mut storage: Box<dyn Storage>,
    host: Rc<dyn WasmHost>,
) -> (Framework, BootReport) {
    let mut fw = Framework {
        activities: ActivityManager::new(),
        packages: package_manager::PackageManager::new(),
        windows: window_manager::WindowManager::new(),
        resources: resources::ResourceManager::new(),
        intents: intent::IntentRouter::new(),
        permissions: permissions::PermissionController::new(),
        compositor: Compositor::new(),
        metrics: SurfaceMetrics::default(),
        pointer: None,
        stack: Vec::new(),
        storage: None,  // populated at the end once we finish using `storage` locally
    };

    // Rehydrate installed packages from storage. A corrupt manifest is
    // reported but not fatal — the Shell and healthy apps stay usable.
    let mut package_errors: Vec<(String, String)> = Vec::new();
    let packages_loaded = fw.packages.load_installed(storage.as_mut(), |id, e: LoadError| {
        package_errors.push((id.to_string(), format!("{e:?}")));
    });

    // Read per-package resource tables from storage and register them
    // with the framework's ResourceManager. Resource errors are tracked
    // separately — a broken values file doesn't stop a package from
    // running, it just means its @ref lookups fall back to placeholders.
    let mut resource_errors: Vec<(String, String)> = Vec::new();
    let tables = fw.packages.read_resource_tables(storage.as_mut(), |id, e| {
        resource_errors.push((id.to_string(), e.to_string()));
    });
    for (id, table) in tables {
        fw.resources.register(&id, table);
    }

    let report = BootReport { packages_loaded, package_errors, resource_errors };

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

    // Register every installed package's manifest-declared intent filters
    // with the router. This is the table the Shell hits when it says
    // "launch MAIN+LAUNCHER of package X" — without these entries, the
    // Shell would click into NoMatch and nothing would happen.
    let intent_entries: Vec<(String, String, manifest::IntentFilterDecl)> = fw.packages.list()
        .flat_map(|r| r.manifest.activities.iter().flat_map(|a| {
            let pkg = r.id.clone();
            let class = a.class.clone();
            a.intent_filters.iter().cloned().map(move |f| (pkg.clone(), class.clone(), f))
        }))
        .collect();
    for (pkg, class, f) in intent_entries {
        fw.intents.register_filter(IntentFilter {
            package_id: pkg,
            activity_class: class,
            actions: f.actions,
            categories: f.categories,
            data_schemes: Vec::new(),
            mime_types: Vec::new(),
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
    // Shell has no manifest (it's built in), so synthesise its intent
    // filter alongside the others. MAIN+HOME is what we resolve against
    // below; MAIN+LAUNCHER is so a future diagnostics path could list
    // "all launcher activities" and find the home screen too.
    fw.intents.register_filter(IntentFilter {
        package_id: shell::PACKAGE_ID.into(),
        activity_class: shell::ENTRY_CLASS.into(),
        actions: vec![intent::action::MAIN.into()],
        categories: vec![intent::category::HOME.into(), intent::category::LAUNCHER.into()],
        data_schemes: Vec::new(),
        mime_types: Vec::new(),
    });
    // Resolve MAIN+HOME through the router instead of hardcoding the
    // entry class — exercises the full intent path at boot and means
    // swapping the home screen later becomes a router-level concern.
    let home = Intent::new(intent::action::MAIN).with_category(intent::category::HOME);
    let (shell_pkg, shell_class) = fw.intents.resolve(&home).expect("HOME resolves");
    let shell_id = fw.activities
        .launch(&shell_pkg, &shell_class)
        .expect("shell launch");
    fw.stack.push(shell_id);
    // Shell has no saved state to restore — it's built-in and stateless
    // from the framework's perspective (boot counter + package list are
    // always recomputed from scratch).
    fw.drive_to_resumed(shell_id, 0.0);

    // Stash the storage handle for post-boot writes (saved-state
    // persistence on Stop / rehydration on Create). We're done using it
    // locally above — the `Box` can now live on the framework.
    fw.storage = Some(storage);

    (fw, report)
}

/// Aggregate handle exposed to the kernel. All cross-subsystem calls go
/// through `&mut Framework` so ownership stays linear and no subsystem
/// reaches around the back.
pub struct Framework {
    pub activities: ActivityManager,
    pub packages: package_manager::PackageManager,
    pub windows: window_manager::WindowManager,
    pub resources: resources::ResourceManager,
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
    /// Persistent backing store the framework uses post-boot (saved-state
    /// persistence, future activity-manager checkpoints). Wrapped in an
    /// `Option` only because boot fills it in after the borrow it needs
    /// for package rehydration is released; it's always `Some` by the
    /// time any dispatch method runs.
    storage: Option<Box<dyn Storage>>,
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
            let mut launch: Option<Intent> = None;
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
        let mut launch: Option<Intent> = None;
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
    ///
    /// Unresolvable intents (NoMatch / AmbiguousMatch) are silently
    /// dropped — no log binding in framework, and a buggy app shouldn't
    /// crash the OS. When we get observability wiring the failures will
    /// become visible; for now they surface as "the click did nothing".
    fn apply_post(&mut self, post: PostCallback, ts: f64) {
        if let Some(intent) = post.launch {
            if let Ok((pkg, class)) = self.intents.resolve(&intent) {
                self.launch_app(&pkg, &class, ts);
            }
        } else if post.finish {
            self.finish_top(ts);
        }
    }

    fn drive_to_resumed(&mut self, id: ActivityId, ts: f64) {
        // Preserved end-to-end path: Create → Start → Resumed in one
        // shot. Callers that need to inject restore_state_bytes between
        // Create and Start use `advance_through_create` +
        // `drive_start_to_resumed` instead; this short-hand stays for
        // bring-up sites that have nothing to restore (the Shell).
        self.advance_through_create(id, ts);
        self.drive_start_to_resumed(id, ts);
    }

    /// Fire `on_create` only. Split from the full resume driver so a
    /// restore hook can land between Created and Started (see
    /// `launch_app`). Idempotent: calling on an already-past-Created
    /// activity is a no-op.
    fn advance_through_create(&mut self, id: ActivityId, ts: f64) {
        let metrics = self.metrics;
        let pointer = self.pointer;
        self.activities.advance(id, LifecycleState::Created, |a, new_state| {
            let mut finish = false;
            let mut launch: Option<Intent> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
            };
            if matches!(new_state, LifecycleState::Created) {
                a.on_create(&mut ctx);
            }
        });
    }

    /// Continue bring-up from Created (or Paused, for the resume-after-
    /// pause case) up to Resumed.
    fn drive_start_to_resumed(&mut self, id: ActivityId, ts: f64) {
        let metrics = self.metrics;
        let pointer = self.pointer;
        self.activities.resume(id, |a, new_state| {
            let mut finish = false;
            let mut launch: Option<Intent> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
            };
            match new_state {
                // Created was fired (or was already past) by
                // `advance_through_create`; silently skip so we don't
                // double-fire.
                LifecycleState::Created => {}
                LifecycleState::Started => a.on_start(&mut ctx),
                LifecycleState::Resumed => a.on_resume(&mut ctx),
                _ => {}
            }
        });
    }

    fn drive_to_paused(&mut self, id: ActivityId, ts: f64) {
        let metrics = self.metrics;
        let pointer = self.pointer;
        self.activities.pause(id, |a, new_state| {
            let mut finish = false;
            let mut launch: Option<Intent> = None;
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
        // Drive Create first so the activity exists, then hand it any
        // saved-state blob we persisted on a previous run. Only after
        // that do we let it proceed to Start → Resumed. This matches the
        // "on_create then on_restore_state" ordering in
        // docs/activity-model.md §3.
        self.advance_through_create(new_id, ts);
        self.try_restore_state(new_id, package_id, class);
        self.drive_start_to_resumed(new_id, ts);
    }

    /// Finish the top of the stack and resume whatever it was covering.
    /// If the stack is already down to its last entry (the Shell), this
    /// is a no-op — we never destroy the home-screen.
    fn finish_top(&mut self, ts: f64) {
        if self.stack.len() <= 1 { return; }
        let id = self.stack.pop().expect("checked len > 1");

        // Capture the save-state blob on the Pause→Stopped edge. The
        // activity has already seen `on_pause` by this point (driven
        // below), so its internal state is whatever it wants persisted.
        // We pull the bytes out *before* Stop fires because Android's
        // contract — mirrored in docs/activity-model.md §3 — is that
        // on_save_state precedes on_stop, and the app is allowed to
        // assume its state is frozen by the time it sees on_stop.
        //
        // Collect (package_id, class) first so the later storage.write
        // call doesn't need to reach through the (still-borrowed)
        // activity manager.
        let meta = self.activities.record(id)
            .map(|r| (r.package_id.clone(), r.class.clone()));

        let metrics = self.metrics;
        let pointer = self.pointer;
        let mut save_bytes: Option<Vec<u8>> = None;
        self.activities.destroy(id, |a, new_state| {
            let mut finish = false;
            let mut launch: Option<Intent> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
            };
            match new_state {
                LifecycleState::Paused => a.on_pause(&mut ctx),
                LifecycleState::Stopped => {
                    // Pull the blob first, then deliver the Stop edge.
                    save_bytes = Some(a.save_state_bytes());
                    a.on_stop(&mut ctx);
                }
                LifecycleState::Destroyed => a.on_destroy(&mut ctx),
                _ => {}
            }
        });
        if let (Some((pkg, class)), Some(bytes)) = (meta, save_bytes) {
            self.persist_saved_state(&pkg, &class, &bytes);
        }
        if let Some(new_top) = self.stack.last().copied() {
            self.drive_to_resumed(new_top, ts);
        }
    }

    /// Write a saved-state blob to persistent storage, or delete any
    /// existing blob if the activity produced zero bytes (common path
    /// for activities that don't override save_state). Failures log
    /// nothing (no framework log binding) but also don't propagate —
    /// missing saved state is survivable, a crashed storage write is
    /// not.
    fn persist_saved_state(&mut self, package_id: &str, class: &str, bytes: &[u8]) {
        let Some(storage) = self.storage.as_deref_mut() else { return; };
        let key = package_manager::key_saved_state(package_id, class);
        let _ = if bytes.is_empty() {
            storage.delete(&key)
        } else {
            storage.write(&key, bytes)
        };
    }

    /// Load a saved-state blob from storage and hand it to the activity
    /// if present. Run between Create and Start on each fresh launch, so
    /// the app sees restored state before it's asked to do anything else.
    fn try_restore_state(&mut self, id: ActivityId, package_id: &str, class: &str) {
        let Some(storage) = self.storage.as_deref_mut() else { return; };
        let key = package_manager::key_saved_state(package_id, class);
        let bytes = match storage.read(&key) {
            Ok(b) if !b.is_empty() => b,
            // NotFound / empty is the common case — no prior state. Any
            // other error is treated the same way: the app boots fresh.
            _ => return,
        };
        self.activities.with_instance(id, |a| a.restore_state_bytes(&bytes));
    }
}

/// What an activity's callback asked the framework to do next. Collected
/// before we re-enter activity code so the borrow on `self` from the
/// dispatch closure is fully released before we mutate the stack.
struct PostCallback {
    finish: bool,
    launch: Option<Intent>,
}
