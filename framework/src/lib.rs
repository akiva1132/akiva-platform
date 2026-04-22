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
        .expect("shell launch")
        .id;
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
                self.launch_app(&intent, &pkg, &class, ts);
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
        //
        // If the activity finishes from on_create we honour the
        // `Created → Destroyed` shortcut here too — the Shell, for
        // example, could self-close at boot. Returning early is enough:
        // no Start/Resume fire, and our single caller at boot
        // (`framework::boot`) doesn't re-examine the Shell's state.
        if self.advance_through_create(id, ts) {
            self.destroy_shortcut(id, ts);
            return;
        }
        self.drive_start_to_resumed(id, ts);
    }

    /// Drive `id` directly from `Created` to `Destroyed` — the Android
    /// parity shortcut when `finish()` is raised from inside `on_create`.
    /// Fires only `on_destroy` (the `(Created, Destroyed)` edge is the
    /// one `legal_transition` allows); Start / Resume / Pause / Stop are
    /// skipped entirely, and save-state is not collected since the
    /// activity never went `Resumed → Paused` in the first place.
    fn destroy_shortcut(&mut self, id: ActivityId, ts: f64) {
        let metrics = self.metrics;
        let pointer = self.pointer;
        self.activities.destroy(id, |a, new_state| {
            let mut finish = false;
            let mut launch: Option<Intent> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
            };
            if matches!(new_state, LifecycleState::Destroyed) {
                a.on_destroy(&mut ctx);
            }
        });
    }

    /// Fire `on_create` only. Split from the full resume driver so a
    /// restore hook can land between Created and Started (see
    /// `launch_app`). Idempotent: calling on an already-past-Created
    /// activity is a no-op.
    ///
    /// Returns whether the activity asked to finish from inside
    /// `on_create`. The caller uses that to skip the restore + Start +
    /// Resume sequence and go straight to `Destroy`, matching the
    /// Android `Created → Destroyed` shortcut documented in
    /// [`docs/activity-model.md §2`](../../docs/activity-model.md).
    fn advance_through_create(&mut self, id: ActivityId, ts: f64) -> bool {
        let metrics = self.metrics;
        let pointer = self.pointer;
        let mut finish = false;
        let mut launch: Option<Intent> = None;
        self.activities.advance(id, LifecycleState::Created, |a, new_state| {
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
            };
            if matches!(new_state, LifecycleState::Created) {
                a.on_create(&mut ctx);
            }
        });
        // `launch` raised from within on_create is currently a no-op:
        // the activity hasn't reached Resumed yet so there's nowhere
        // coherent to stack a new top on. Drop it silently rather than
        // let it fire out of order. Apps that want this behaviour can
        // set launch from on_resume instead.
        let _ = launch;
        finish
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
    ///
    /// `intent` is carried through from `apply_post` so the reuse branch
    /// (SingleTop top match, or Stone 9.2's SingleTask / SingleInstance
    /// finding a matching instance anywhere in the foreground task) can
    /// deliver `on_new_intent` to the reused activity.
    fn launch_app(&mut self, intent: &Intent, package_id: &str, class: &str, ts: f64) {
        let outcome = match self.activities.launch(package_id, class) {
            Ok(o) => o,
            Err(_) => return,
        };

        // SingleTask / SingleInstance reuse may have identified activities
        // stacked above the target. Destroy each (top-first order is
        // already baked into `outcome.popped`) so their on_pause /
        // on_stop / on_destroy fire in the correct sequence and any
        // save-state is persisted. Remove from our mirror of the stack
        // as we go — `destroy_with_persist` drops them from the
        // ActivityManager's task.stack via its normal cleanup.
        for popped_id in &outcome.popped {
            self.destroy_with_persist(*popped_id, ts);
            self.stack.retain(|x| *x != *popped_id);
        }

        if outcome.reused {
            // Three reuse shapes merge here:
            //   1. SingleTop top match — target was Resumed, stays Resumed.
            //   2. SingleTask / SingleInstance finding the target at top —
            //      same as (1).
            //   3. SingleTask / SingleInstance finding the target below
            //      top — target was Paused (covered), the pop loop above
            //      destroyed whatever covered it, now we must drive it
            //      back to Resumed.
            //
            // `drive_start_to_resumed` handles both Paused→Resumed and
            // the Resumed no-op, so we don't need to branch on prior state.
            self.drive_start_to_resumed(outcome.id, ts);
            self.deliver_new_intent(outcome.id, intent, ts);
            return;
        }

        // Fresh-instance path.
        if let Some(prev) = self.stack.last().copied() {
            self.drive_to_paused(prev, ts);
        }
        self.stack.push(outcome.id);
        // Drive Create first so the activity exists, then hand it any
        // saved-state blob we persisted on a previous run. Only after
        // that do we let it proceed to Start → Resumed. This matches the
        // "on_create then on_restore_state" ordering in
        // docs/activity-model.md §3.
        //
        // Android parity: if the activity called `finish()` from inside
        // `on_create`, skip restore + Start + Resume and jump straight
        // to on_destroy via the `Created → Destroyed` shortcut. Then pop
        // the stack entry and resume whatever we just paused.
        if self.advance_through_create(outcome.id, ts) {
            self.destroy_shortcut(outcome.id, ts);
            self.stack.pop();
            if let Some(new_top) = self.stack.last().copied() {
                self.drive_to_resumed(new_top, ts);
            }
            return;
        }
        self.try_restore_state(outcome.id, package_id, class);
        self.drive_start_to_resumed(outcome.id, ts);
    }

    /// Fire `on_new_intent` on an already-Resumed activity and drain any
    /// follow-up requests. Split out from `launch_app`'s reuse branch so
    /// a future launch-mode path (e.g. SingleTask finding an instance
    /// deeper in the stack than the top) can call it directly after
    /// bringing that instance forward. `apply_post` recurses into
    /// whatever the handler requested — a chained `ctx.start_activity`
    /// is honoured, a `ctx.finish` unwinds immediately.
    fn deliver_new_intent(&mut self, id: ActivityId, intent: &Intent, ts: f64) {
        let (post, _) = self.with_ctx(id, ts, |a, ctx| a.on_new_intent(ctx, intent));
        self.apply_post(post, ts);
    }

    /// Finish the top of the stack and resume whatever it was covering.
    /// If the stack is already down to its last entry (the Shell), this
    /// is a no-op — we never destroy the home-screen.
    fn finish_top(&mut self, ts: f64) {
        if self.stack.len() <= 1 { return; }
        let id = self.stack.pop().expect("checked len > 1");
        self.destroy_with_persist(id, ts);
        if let Some(new_top) = self.stack.last().copied() {
            self.drive_to_resumed(new_top, ts);
        }
    }

    /// Drive an activity through its full destroy chain (pause → stop →
    /// destroy, whatever edges are legal from its current state) and
    /// persist any save-state bytes produced on the Stop edge.
    ///
    /// Save-state is captured *before* `on_stop` fires because Android's
    /// contract — mirrored in docs/activity-model.md §3 — is that
    /// on_save_state precedes on_stop, and the app is allowed to assume
    /// its state is frozen by the time it sees on_stop.
    ///
    /// Pulled out of `finish_top` so SingleTask / SingleInstance's
    /// pop-above-reuse path (`launch_app`) can destroy each covered
    /// activity with the same save-state discipline. Does NOT touch
    /// `self.stack` — the caller is responsible for mirror bookkeeping.
    fn destroy_with_persist(&mut self, id: ActivityId, ts: f64) {
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

#[cfg(test)]
mod tests {
    //! Framework-level integration tests covering cross-module behaviour
    //! (ActivityManager + IntentRouter + stack discipline + post-callback
    //! application). `activity_manager.rs` owns the state-machine tests;
    //! `wasm_activity.rs` owns the proxy tests; this module owns the
    //! end-to-end paths that can only be asserted after wiring them
    //! together.
    //
    // Boot path (`framework::boot`) is heavy — it rehydrates packages
    // from a Storage, registers every package's intent filters, brings
    // up the Shell. Tests here build a [`Framework`] manually instead,
    // registering only what they exercise. That's why the tests touch
    // `fw.stack.push(...)` directly — the field is crate-private but
    // visible inside this module.
    //
    // No Storage means `try_restore_state` / `persist_saved_state` are
    // no-ops (they early-return on `self.storage.is_none()`), which is
    // exactly what these tests want: we're asserting lifecycle routing,
    // not save-state persistence.

    use super::*;
    use crate::activity_manager::LaunchMode;
    use crate::intent::{action, category, IntentFilter};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Activity that (a) records every callback it sees and (b) may fire
    /// a pre-programmed `launch_requested` or `finish_requested` on a
    /// named callback. Test cases inject intents to exercise reuse vs
    /// bring-up paths without having to simulate wasm input.
    struct ScriptedActivity {
        name: &'static str,
        log: Rc<RefCell<Vec<String>>>,
        on_pointer_launch: Option<Intent>,
    }

    impl ScriptedActivity {
        fn new(name: &'static str, log: Rc<RefCell<Vec<String>>>) -> Self {
            Self { name, log, on_pointer_launch: None }
        }

        fn with_pointer_launch(mut self, intent: Intent) -> Self {
            self.on_pointer_launch = Some(intent);
            self
        }

        fn note(&self, tag: &str) {
            self.log.borrow_mut().push(format!("{}:{}", self.name, tag));
        }
    }

    impl activity::Activity for ScriptedActivity {
        fn on_create(&mut self, _: &mut ActivityContext<'_>) { self.note("create"); }
        fn on_start(&mut self, _: &mut ActivityContext<'_>) { self.note("start"); }
        fn on_resume(&mut self, _: &mut ActivityContext<'_>) { self.note("resume"); }
        fn on_pause(&mut self, _: &mut ActivityContext<'_>) { self.note("pause"); }
        fn on_stop(&mut self, _: &mut ActivityContext<'_>) { self.note("stop"); }
        fn on_destroy(&mut self, _: &mut ActivityContext<'_>) { self.note("destroy"); }
        fn on_new_intent(&mut self, _: &mut ActivityContext<'_>, intent: &Intent) {
            self.note(&format!("new_intent:{}", intent.action));
        }
        fn on_pointer(&mut self, ctx: &mut ActivityContext<'_>, _ev: activity::PointerEvent) {
            self.note("pointer");
            if let Some(intent) = self.on_pointer_launch.clone() {
                *ctx.launch_requested = Some(intent);
            }
        }
        fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
    }

    fn empty_framework() -> Framework {
        Framework {
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
            storage: None,
        }
    }

    fn pointer_down() -> activity::PointerEvent {
        activity::PointerEvent {
            x: 0.0, y: 0.0, button: 0,
            phase: activity::PointerPhase::Down,
        }
    }

    #[test]
    fn singletop_reuse_fires_on_new_intent_instead_of_bring_up() {
        // Stone 9.4 — the core reuse assertion. Shell is `SingleTop` in
        // the real boot; launching it again while it sits on top must
        // not double-create, double-resume, or churn the stack — the
        // live instance receives `on_new_intent` and stays Resumed.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut fw = empty_framework();

        let log_for_shell = log.clone();
        // Re-launching the Shell through an explicit intent gives us a
        // deterministic resolution target; we don't need the IntentRouter
        // to search.
        let shell_relaunch = Intent::explicit("test.shell", "Main");
        fw.activities.register("test.shell", "Main", LaunchMode::SingleTop, move || {
            Box::new(ScriptedActivity::new("shell", log_for_shell.clone())
                .with_pointer_launch(shell_relaunch.clone()))
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.shell".into(),
            activity_class: "Main".into(),
            actions: vec![action::MAIN.into()],
            categories: vec![category::HOME.into()],
            data_schemes: vec![],
            mime_types: vec![],
        });

        // Bring the shell up by the same path `boot()` uses: launch +
        // push onto the stack + drive to resumed.
        let shell_id = fw.activities.launch("test.shell", "Main").unwrap().id;
        fw.stack.push(shell_id);
        fw.drive_to_resumed(shell_id, 0.0);
        log.borrow_mut().clear();

        // Pointer fires, the scripted activity asks to launch itself.
        // `apply_post` should resolve the intent, see the reuse path
        // (new_id == top), and deliver `on_new_intent` — nothing else.
        fw.dispatch_pointer(pointer_down(), 1.0);

        let events = log.borrow().clone();
        assert_eq!(
            events,
            vec![
                "shell:pointer".to_string(),
                format!("shell:new_intent:{}", action::MAIN),
            ],
            "SingleTop reuse must fire pointer then exactly one on_new_intent",
        );
        // Stack unchanged — no double-push on reuse.
        assert_eq!(fw.stack.len(), 1);
        assert_eq!(fw.stack[0], shell_id);
        assert_eq!(
            fw.activities.state_of(shell_id),
            Some(activity_manager::LifecycleState::Resumed),
            "reused activity must stay Resumed — no pause/resume churn",
        );
    }

    #[test]
    fn standard_launch_stacks_and_finish_unwinds() {
        // Stone 9.4 — the "A opens B, back → A" spine. Two Standard
        // activities: Shell (bottom) and App on top. Opening App from
        // Shell's pointer pushes App; the Escape-back key in
        // `dispatch_key` finishes App and resumes Shell.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut fw = empty_framework();

        let log_for_shell = log.clone();
        let app_intent = Intent::explicit("test.app", "Main");
        fw.activities.register("test.shell", "Main", LaunchMode::SingleTop, move || {
            Box::new(ScriptedActivity::new("shell", log_for_shell.clone())
                .with_pointer_launch(app_intent.clone()))
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.shell".into(),
            activity_class: "Main".into(),
            actions: vec![action::MAIN.into()],
            categories: vec![category::HOME.into()],
            data_schemes: vec![],
            mime_types: vec![],
        });
        let log_for_app = log.clone();
        fw.activities.register("test.app", "Main", LaunchMode::Standard, move || {
            Box::new(ScriptedActivity::new("app", log_for_app.clone()))
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.app".into(),
            activity_class: "Main".into(),
            actions: vec![action::MAIN.into()],
            categories: vec![],
            data_schemes: vec![],
            mime_types: vec![],
        });

        let shell_id = fw.activities.launch("test.shell", "Main").unwrap().id;
        fw.stack.push(shell_id);
        fw.drive_to_resumed(shell_id, 0.0);
        log.borrow_mut().clear();

        // Shell pointer → launch App. Expected events:
        //   shell:pointer, shell:pause, app:create, app:start, app:resume
        fw.dispatch_pointer(pointer_down(), 1.0);
        assert_eq!(
            &*log.borrow(),
            &[
                "shell:pointer",
                "shell:pause",
                "app:create",
                "app:start",
                "app:resume",
            ],
        );
        assert_eq!(fw.stack.len(), 2);
        log.borrow_mut().clear();

        // Escape back → finish_top → App pauses+stops+destroys,
        // Shell resumes.
        let esc = activity::KeyEvent {
            code: KEY_CODE_BACK,
            modifiers: 0,
            phase: activity::KeyPhase::Down,
        };
        fw.dispatch_key(esc, 2.0);
        assert_eq!(
            &*log.borrow(),
            &[
                "app:pause",
                "app:stop",
                "app:destroy",
                "shell:resume",
            ],
        );
        assert_eq!(fw.stack, vec![shell_id]);
        assert_eq!(
            fw.activities.state_of(shell_id),
            Some(activity_manager::LifecycleState::Resumed),
        );
    }

    #[test]
    fn singletask_relaunch_pops_above_and_fires_new_intent() {
        // Stone 9.2 — end-to-end SingleTask reuse. Stack shape:
        //   Shell (SingleTop, bottom)
        //   TaskApp (SingleTask, the target of the reuse)
        //   Filler (Standard, covers the target)
        // Relaunching TaskApp must destroy Filler (pause/stop/destroy in
        // order), resume TaskApp from Paused, and deliver on_new_intent —
        // all in one apply_post pass, no second user input needed.
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut fw = empty_framework();

        // Shell: clicks launch TaskApp first time, then the Filler, then
        // request TaskApp again (the reuse we're testing). The sequence
        // is driven by a mutable pointer-click counter so the test can
        // step through each launch one pointer at a time.
        struct ShellActivity {
            name: &'static str,
            log: Rc<RefCell<Vec<String>>>,
            clicks: Rc<RefCell<u32>>,
            intents: Rc<[Intent; 3]>,
        }
        impl activity::Activity for ShellActivity {
            fn on_create(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push(format!("{}:create", self.name));
            }
            fn on_start(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push(format!("{}:start", self.name));
            }
            fn on_resume(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push(format!("{}:resume", self.name));
            }
            fn on_pause(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push(format!("{}:pause", self.name));
            }
            fn on_stop(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push(format!("{}:stop", self.name));
            }
            fn on_destroy(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push(format!("{}:destroy", self.name));
            }
            fn on_pointer(&mut self, ctx: &mut ActivityContext<'_>, _: activity::PointerEvent) {
                self.log.borrow_mut().push(format!("{}:pointer", self.name));
                let n = *self.clicks.borrow() as usize;
                *self.clicks.borrow_mut() += 1;
                if n < self.intents.len() {
                    *ctx.launch_requested = Some(self.intents[n].clone());
                }
            }
            fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
        }

        let clicks = Rc::new(RefCell::new(0u32));
        let task_intent = Intent::explicit("test.pkg", "TaskMain");
        let filler_intent = Intent::explicit("test.pkg", "Filler");
        let task_intent_for_reuse = Intent::explicit("test.pkg", "TaskMain");
        let intents: Rc<[Intent; 3]> = Rc::new([
            task_intent.clone(),
            filler_intent.clone(),
            task_intent_for_reuse.clone(),
        ]);

        let log_shell = log.clone();
        let clicks_shell = clicks.clone();
        let intents_shell = intents.clone();
        fw.activities.register("test.shell", "Main", LaunchMode::SingleTop, move || {
            Box::new(ShellActivity {
                name: "shell",
                log: log_shell.clone(),
                clicks: clicks_shell.clone(),
                intents: intents_shell.clone(),
            })
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.shell".into(),
            activity_class: "Main".into(),
            actions: vec![action::MAIN.into()],
            categories: vec![category::HOME.into()],
            data_schemes: vec![], mime_types: vec![],
        });

        // TaskApp — SingleTask. Plain ScriptedActivity, no scripted
        // pointer launch. on_new_intent delivery is the assertion.
        let log_task = log.clone();
        fw.activities.register("test.pkg", "TaskMain", LaunchMode::SingleTask, move || {
            Box::new(ScriptedActivity::new("task", log_task.clone()))
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.pkg".into(),
            activity_class: "TaskMain".into(),
            actions: vec!["akiva.action.TaskMain".into()],
            categories: vec![], data_schemes: vec![], mime_types: vec![],
        });

        // Filler — Standard. Just needs to cover the target.
        let log_filler = log.clone();
        fw.activities.register("test.pkg", "Filler", LaunchMode::Standard, move || {
            Box::new(ScriptedActivity::new("filler", log_filler.clone()))
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.pkg".into(),
            activity_class: "Filler".into(),
            actions: vec!["akiva.action.Filler".into()],
            categories: vec![], data_schemes: vec![], mime_types: vec![],
        });

        // Boot the shell the same way framework::boot does.
        let shell_id = fw.activities.launch("test.shell", "Main").unwrap().id;
        fw.stack.push(shell_id);
        fw.drive_to_resumed(shell_id, 0.0);
        log.borrow_mut().clear();

        // Click 1: Shell launches TaskApp (fresh create).
        fw.dispatch_pointer(pointer_down(), 1.0);
        assert_eq!(
            &*log.borrow(),
            &[
                "shell:pointer",
                "shell:pause",
                "task:create",
                "task:start",
                "task:resume",
            ],
        );
        assert_eq!(fw.stack.len(), 2);
        let task_id = fw.stack[1];
        log.borrow_mut().clear();

        // Click 2 is delivered to the top of the stack — which is now
        // TaskApp. The scripted `task` activity has no pointer launch, so
        // we need to go back to the Shell first. The easier approach: drive
        // click through Shell by calling dispatch_pointer while Shell is
        // top. Instead, bypass by calling launch_app directly to push the
        // filler; it's a framework-internal test anyway.
        fw.launch_app(&filler_intent, "test.pkg", "Filler", 2.0);
        assert_eq!(
            &*log.borrow(),
            &[
                "task:pause",
                "filler:create",
                "filler:start",
                "filler:resume",
            ],
        );
        assert_eq!(fw.stack.len(), 3);
        log.borrow_mut().clear();

        // The reuse under test: relaunch TaskApp. Stack was
        //   [shell, task, filler]
        // Expected sequence:
        //   filler:pause, filler:stop, filler:destroy  ← pop-above
        //   task:resume                                ← Paused → Resumed
        //   task:new_intent:<action>                   ← deliver_new_intent
        // Stack after: [shell, task]
        fw.launch_app(&task_intent_for_reuse, "test.pkg", "TaskMain", 3.0);
        assert_eq!(
            &*log.borrow(),
            &[
                "filler:pause",
                "filler:stop",
                "filler:destroy",
                "task:resume",
                "task:new_intent:akiva.intent.action.MAIN",
            ],
            "SingleTask reuse must destroy popped, resume target, then deliver on_new_intent",
        );
        assert_eq!(fw.stack, vec![shell_id, task_id]);
        assert_eq!(
            fw.activities.state_of(task_id),
            Some(activity_manager::LifecycleState::Resumed),
        );
    }
}
