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
pub mod wire;
/// Stone 11.0a wire format kept reachable at its old path so call sites
/// outside this crate (`kernel`, `sdk-rust` doc comments) don't have to
/// move at the same time as the Stone 11.3a refactor.
pub use wire::pkgs as pkgs_wire;
pub mod resources;
pub mod result;
pub mod storage;
pub mod wasm_activity;
pub mod window_manager;

use std::collections::HashMap;
use std::rc::Rc;

use activity::{ActivityContext, KeyEvent, PointerEvent, SurfaceMetrics};
use activity_manager::{ActivityId, ActivityManager, LaunchMode, LifecycleState, TaskId};
use compositor::{Compositor, FrameCommands};
use intent::{Intent, IntentFilter};
use package_manager::LoadError;
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
/// `storage` is the KV store the framework uses for the package index
/// and per-package manifests — borrowed only for the duration of boot;
/// post-boot calls that need persistence take it explicitly. `host` is
/// the wasm-app runtime seam: framework-side `WasmActivity` factories
/// call it to instantiate and drive user-app wasm modules. The
/// framework never touches app memory directly.
///
/// Stone 11.0c — boot no longer takes `boot_count`. The Shell, now a
/// regular wasm app, doesn't have a portable way to read kernel-side
/// counters, and the value was only ever shown in the Shell header
/// (where it doubled as a smoke test for the storage HAL). The kernel
/// still increments `sys/boot_count` and logs the value at info level,
/// keeping the storage round-trip observable end-to-end.
pub fn boot(
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
        pending_result_for: HashMap::new(),
        pending_result: HashMap::new(),
        task_history: Vec::new(),
    };

    // Stone 10.1 — declare the MVP permission catalog before any
    // package rehydration. `PackageManager::install` will refuse to
    // grant a manifest-requested permission that isn't declared here,
    // so the order matters: declare first, then rehydrate packages.
    permissions::declare_mvp_permissions(&mut fw.permissions);

    // Rehydrate installed packages from storage. A corrupt manifest is
    // reported but not fatal — the Shell and healthy apps stay usable.
    let mut package_errors: Vec<(String, String)> = Vec::new();
    let packages_loaded = fw.packages.load_installed(storage.as_mut(), |id, e: LoadError| {
        package_errors.push((id.to_string(), format!("{e:?}")));
    });

    // Stone 10.2 — rehydrate persisted permission grants. Must run
    // after `load_installed` (needs the package list) and before any
    // activity instantiation (Stone 10.3 will build import maps from
    // these grants). Per-package read failures are folded into the
    // same error channel as package-rehydration failures because from
    // the user's perspective a package without its grants is as broken
    // as a package without its manifest.
    fw.packages.load_granted_permissions(
        storage.as_ref(),
        &mut fw.permissions,
        |id, e: LoadError| {
            package_errors.push((id.to_string(), format!("grants: {e:?}")));
        },
    );

    // Stone 11.0c — self-heal for packages whose `sys/permissions/{id}`
    // blob is missing. The dev-apps install path in worker.js writes
    // `manifest.toml` + `app.wasm` directly to OPFS without going
    // through `PackageManager::install`, so these packages have no
    // grants file and `load_granted_permissions` finds nothing. Apply
    // the manifest's `required` list now — same semantics as the
    // fresh-install branch (Normal-protection auto-grant). Skipping
    // would leave the Shell (and any future shell-shaped app) with
    // its capability imports stubbed out at instantiation, producing
    // an empty launcher with no obvious cause.
    //
    // Heal-condition is "no grants currently AND manifest declares
    // some". A package that legitimately installed with empty perms
    // still has its (empty) blob written, so it doesn't trip this.
    // Per-heal failures get folded into `package_errors` rather than
    // aborting the whole boot.
    let heal: Vec<(String, Vec<String>)> = fw.packages.list()
        .filter(|r| {
            !r.manifest.permissions.required.is_empty()
                && fw.permissions.granted_to(&r.id).is_empty()
        })
        .map(|r| (r.id.clone(), r.manifest.permissions.required.clone()))
        .collect();
    for (id, required) in heal {
        if let Err(e) = package_manager::apply_grants(
            storage.as_mut(), &mut fw.permissions, &id, &required,
        ) {
            package_errors.push((id, format!("grant heal: {e:?}")));
        }
    }

    // Stone 10.3b — push each package's grants to the wasm host so
    // `_instantiateProcess` can filter the `akiva_host_*` import map
    // at link time (soft enforcement via stub replacement). Must run
    // AFTER rehydration (so the set is complete) and BEFORE any
    // activity launch below — `_instantiateProcess` consults the
    // grants when wiring imports, including the Shell's own
    // instantiation since Stone 11.0c (Shell is now a regular wasm
    // package).
    let package_ids: Vec<String> = fw.packages.list().map(|r| r.id.clone()).collect();
    for pkg in &package_ids {
        let grants = fw.permissions.granted_to(pkg);
        host.set_grants(pkg, &grants);
    }

    // Stone 11.0a — push the snapshot of installed packages so apps with
    // QUERY_PACKAGES can enumerate them via `akiva_host_query_packages`.
    // Order is whatever `PackageManager::list()` produces (HashMap
    // values; insertion order isn't guaranteed); the Shell sorts at the
    // app layer if it cares about a stable display order.
    //
    // Stone 11.0c — Shell is now itself a `sys.shell` package; we
    // include it in the snapshot. The Shell filters itself out of its
    // own launcher (so it doesn't offer "launch yourself") inside
    // `apps/shell/src/lib.rs`. Letting the framework decide that here
    // would re-introduce the special-casing 11.0c is removing.
    let pkg_refs: Vec<pkgs_wire::PackageRef<'_>> = fw.packages.list()
        .map(|r| pkgs_wire::PackageRef {
            id: r.id.as_str(),
            class: r.entry_class.as_str(),
            name: r.name.as_str(),
        })
        .collect();
    host.set_packages(&pkgs_wire::encode(&pkg_refs));

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

    // Stone 11.0c — Shell is no longer a built-in. It ships as the
    // `sys.shell` wasm package (apps/shell/), gets installed via the
    // standard dev-apps pipeline, and registers MAIN+HOME via its
    // manifest's intent filter exactly like a user app. The boot path
    // here is responsible only for *resolving* HOME and bringing the
    // matching activity to the foreground — we don't know or care that
    // it's "the shell".
    //
    // Resolution failure is no longer a panic: a fresh checkout that
    // hasn't run `akiva build` yet boots without a Shell installed, and
    // we'd rather log the gap and continue than crash the kernel. The
    // display will sit empty until something gets installed, which is a
    // more honest signal of a misconfigured dev environment than a
    // wasm trap.
    let home = Intent::new(intent::action::MAIN).with_category(intent::category::HOME);
    match fw.intents.resolve(&home) {
        Ok((pkg, class)) => {
            match fw.activities.launch(&pkg, &class) {
                Ok(launched) => {
                    let id = launched.id;
                    fw.stack.push(id);
                    fw.drive_to_resumed(id, 0.0);
                }
                Err(e) => {
                    package_errors.push((
                        pkg,
                        format!("HOME launch failed: {e:?}"),
                    ));
                }
            }
        }
        Err(e) => {
            // Logged by the kernel via the BootReport — no log binding
            // in framework code, so we surface it as a synthetic
            // package error. The kernel writes warns from these.
            package_errors.push((
                "<home>".into(),
                format!("no installed package matches MAIN+HOME: {e:?}"),
            ));
        }
    }

    // Stash the storage handle for post-boot writes (saved-state
    // persistence on Stop / rehydration on Create). We're done using it
    // locally above — the `Box` can now live on the framework.
    fw.storage = Some(storage);

    let report = BootReport { packages_loaded, package_errors, resource_errors };
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
    /// Stone 9.5 — for every activity launched via
    /// `start_activity_for_result`, record the (caller_id, request_code)
    /// so we can deliver `on_activity_result` to the caller when the
    /// callee finishes. Keyed by callee id so destroy-paths only need
    /// one map lookup.
    pending_result_for: HashMap<ActivityId, (ActivityId, u32)>,
    /// Stone 9.5 — bytes an activity stashed via `set_result`, keyed by
    /// its own id. Drained in `finish_top` / `destroy_with_persist`
    /// just before the activity is destroyed. Absent means the callee
    /// finished without producing a result, and the framework
    /// synthesises `ActivityResult::canceled()` on delivery.
    pending_result: HashMap<ActivityId, result::ActivityResult>,
    /// Stone 9.6 — back-stack of task ids the Framework has visited.
    /// Pushed every time `switch_to_task` brings a new task forward
    /// (the *previous* foreground is recorded). Popped when the
    /// current foreground task empties out (SingleInstance finishing
    /// alone) so we can restore the prior task instead of stranding
    /// the display on an empty stack. Never grows unbounded in
    /// practice — the Shell's task sits at position 0 and is never
    /// popped, so worst-case depth is the number of live tasks.
    task_history: Vec<TaskId>,
}

impl Framework {
    pub fn set_metrics(&mut self, m: SurfaceMetrics) {
        self.metrics = m;
        let Some(id) = self.stack.last().copied() else { return; };
        let (post, _) = self.with_ctx(id, 0.0, |a, ctx| a.on_resize(ctx, m));
        self.apply_post(post, id, 0.0);
    }

    /// Dispatch a pointer event to the active activity. The framework
    /// tracks the latest pointer state so callbacks that inspect `ctx`
    /// see a consistent view.
    pub fn dispatch_pointer(&mut self, ev: PointerEvent, ts: f64) {
        self.pointer = Some(ev);
        let Some(id) = self.stack.last().copied() else { return; };
        let (post, _) = self.with_ctx(id, ts, |a, ctx| a.on_pointer(ctx, ev));
        self.apply_post(post, id, ts);
    }

    pub fn dispatch_key(&mut self, ev: KeyEvent, ts: f64) {
        // Back key short-circuit: when something sits above the Shell,
        // Escape pops instead of reaching the app. This matches the
        // Android convention of a global Back button and spares each
        // app from having to implement "close me" themselves. Only
        // `Down` triggers so a held-down Escape doesn't rapid-fire.
        //
        // Stone 9.6 — a SingleInstance living alone in its dedicated
        // task has `stack.len() == 1`, so we also short-circuit when
        // `task_history` has a task to return to. Without this, Back
        // would fall through to the activity and users couldn't
        // escape a full-screen SingleInstance via Escape.
        if ev.code == KEY_CODE_BACK
            && matches!(ev.phase, activity::KeyPhase::Down)
            && (self.stack.len() > 1 || !self.task_history.is_empty())
        {
            self.finish_top(ts);
            return;
        }
        let Some(id) = self.stack.last().copied() else { return; };
        let (post, _) = self.with_ctx(id, ts, |a, ctx| a.on_key(ctx, ev));
        self.apply_post(post, id, ts);
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
            let mut launch_for_result: Option<(Intent, u32)> = None;
            let mut set_result: Option<result::ActivityResult> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
                start_for_result_requested: &mut launch_for_result,
                set_result_requested: &mut set_result,
            };
            // Explicit split so the compiler sees `compositor` and
            // `activities` as disjoint mut borrows of self.
            let activities = &mut self.activities;
            let target = self.compositor.paint_target();
            activities.with_instance(id, |a| a.render(&mut ctx, target));
            self.apply_post(PostCallback { finish, launch, launch_for_result, set_result }, id, ts);
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
        let mut launch_for_result: Option<(Intent, u32)> = None;
        let mut set_result: Option<result::ActivityResult> = None;
        let mut result: Option<R> = None;
        {
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
                start_for_result_requested: &mut launch_for_result,
                set_result_requested: &mut set_result,
            };
            self.activities.with_instance(id, |a| {
                result = Some(f(a, &mut ctx));
            });
        }
        (PostCallback { finish, launch, launch_for_result, set_result }, result)
    }

    /// Consume the activity's post-callback requests in a deterministic
    /// order:
    ///   1. `set_result` (idempotent side-effect; just stashes bytes) —
    ///      run first so a callback that both stashes a result AND
    ///      finishes delivers that result, not a CANCELED synthesis.
    ///   2. `start_for_result` (richer of the two launch shapes) — wins
    ///      over plain `launch` if both are set.
    ///   3. `launch` (plain start_activity).
    ///   4. `finish` — last so a callback that launches-and-finishes
    ///      doesn't tear itself down before the new top is paused-over it.
    ///
    /// Unresolvable intents (NoMatch / AmbiguousMatch) are silently
    /// dropped — no log binding in framework, and a buggy app shouldn't
    /// crash the OS. When we get observability wiring the failures will
    /// become visible; for now they surface as "the click did nothing".
    fn apply_post(&mut self, post: PostCallback, caller_id: ActivityId, ts: f64) {
        if let Some(result) = post.set_result {
            self.pending_result.insert(caller_id, result);
        }
        if let Some((intent, request_code)) = post.launch_for_result {
            if let Ok((pkg, class)) = self.intents.resolve(&intent) {
                self.launch_app_for_result(&intent, &pkg, &class, caller_id, request_code, ts);
            }
        } else if let Some(intent) = post.launch {
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
        // bring-up sites that have nothing to restore (HOME at boot,
        // before any prior saved state could exist).
        //
        // If the activity finishes from on_create we honour the
        // `Created → Destroyed` shortcut here too — a HOME app could
        // self-close at boot, for example. Returning early is enough:
        // no Start/Resume fire, and the boot caller doesn't re-examine
        // the activity's state.
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
            let mut launch_for_result: Option<(Intent, u32)> = None;
            let mut set_result: Option<result::ActivityResult> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
                start_for_result_requested: &mut launch_for_result,
                set_result_requested: &mut set_result,
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
        let mut launch_for_result: Option<(Intent, u32)> = None;
        let mut set_result: Option<result::ActivityResult> = None;
        self.activities.advance(id, LifecycleState::Created, |a, new_state| {
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
                start_for_result_requested: &mut launch_for_result,
                set_result_requested: &mut set_result,
            };
            if matches!(new_state, LifecycleState::Created) {
                a.on_create(&mut ctx);
            }
        });
        // `launch` raised from within on_create is currently a no-op:
        // the activity hasn't reached Resumed yet so there's nowhere
        // coherent to stack a new top on. Same with
        // `launch_for_result` / `set_result` (9.5) — all three are
        // discarded here since on_create fires before the activity has
        // reached Resumed. Apps that want these behaviours must set
        // them from `on_resume` (or later) instead.
        let _ = (launch, launch_for_result, set_result);
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
            let mut launch_for_result: Option<(Intent, u32)> = None;
            let mut set_result: Option<result::ActivityResult> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
                start_for_result_requested: &mut launch_for_result,
                set_result_requested: &mut set_result,
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
            let mut launch_for_result: Option<(Intent, u32)> = None;
            let mut set_result: Option<result::ActivityResult> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
                start_for_result_requested: &mut launch_for_result,
                set_result_requested: &mut set_result,
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
    ///
    /// Stone 9.6 — the target may land in a task that isn't currently
    /// foreground (SingleInstance always gets its own dedicated task;
    /// SingleInstance reuse can find the target in any task). The
    /// fresh-create and reuse branches both funnel through a
    /// "switch-if-needed" step so a non-foreground target becomes the
    /// active task with correct pause/resume semantics on the old top.
    fn launch_app(&mut self, intent: &Intent, package_id: &str, class: &str, ts: f64) {
        let outcome = match self.activities.launch(package_id, class) {
            Ok(o) => o,
            Err(_) => return,
        };

        // Stash the task the target lands in — we need it both for the
        // fresh-create branch (where AM already pushed outcome.id onto
        // this task's stack) and for the reuse branch (where we may
        // need to bring the task to foreground before resuming).
        let target_task = self.activities.record(outcome.id)
            .expect("just launched")
            .task;

        // SingleTask / SingleInstance reuse may have identified activities
        // stacked above the target. Destroy each (top-first order is
        // already baked into `outcome.popped`) so their on_pause /
        // on_stop / on_destroy fire in the correct sequence and any
        // save-state is persisted. Popped ids always live in the target
        // task (SingleTask restricts to foreground; SingleInstance's
        // dedicated-task invariant guarantees `popped` is empty), so the
        // `self.stack.retain` mirror update only matters when
        // target_task is already foreground.
        //
        // Stone 9.5: if any popped activity was launched for a result,
        // synthesise a CANCELED delivery to its caller.
        for popped_id in &outcome.popped {
            self.destroy_with_persist(*popped_id, ts);
            self.deliver_activity_result_for(*popped_id, ts);
            self.stack.retain(|x| *x != *popped_id);
        }

        if outcome.reused {
            // Three reuse shapes merge here:
            //   1. SingleTop / SingleTask top match — target was Resumed.
            //   2. SingleTask below-top after pop loop above — target was
            //      Paused (covered) and covering activities are gone.
            //   3. SingleInstance across tasks — target was Paused in its
            //      dedicated (background) task; we need to bring that
            //      task to foreground before driving resume.
            //
            // `drive_start_to_resumed` handles Paused→Resumed and the
            // Resumed no-op, so the only extra work is the optional
            // task switch.
            self.switch_to_task_if_needed(target_task, ts);
            self.drive_start_to_resumed(outcome.id, ts);
            self.deliver_new_intent(outcome.id, intent, ts);
            return;
        }

        // Fresh-instance path. Branch on "same foreground task" vs
        // "dedicated task" (SingleInstance) so the fw.stack mirror stays
        // in sync with whichever task is foreground.
        let prev_fg = self.activities.foreground_task();
        let switching = Some(target_task) != prev_fg;
        if switching {
            // SingleInstance only, in MVP. Pause the old foreground top
            // (it lives in `prev_fg`, which is still mirrored in
            // fw.stack until we swap below), then bring the new task
            // forward. AM already pushed outcome.id onto `target_task`'s
            // stack, so the mirror update below picks up `[outcome.id]`.
            if let Some(prev_top) = self.stack.last().copied() {
                self.drive_to_paused(prev_top, ts);
            }
            self.switch_foreground_stack(target_task);
        } else {
            // Same-task push (Standard / SingleTop-no-match / SingleTask-
            // not-found): AM pushed onto the foreground task; mirror the
            // push into fw.stack after pausing the current top.
            if let Some(prev_top) = self.stack.last().copied() {
                self.drive_to_paused(prev_top, ts);
            }
            self.stack.push(outcome.id);
        }

        // Drive Create first so the activity exists, then hand it any
        // saved-state blob we persisted on a previous run. Only after
        // that do we let it proceed to Start → Resumed. This matches the
        // "on_create then on_restore_state" ordering in
        // docs/activity-model.md §3.
        //
        // Android parity: if the activity called `finish()` from inside
        // `on_create`, skip restore + Start + Resume and jump straight
        // to on_destroy via the `Created → Destroyed` shortcut. Then
        // remove the stack entry and resume whatever we just paused —
        // possibly reverting the foreground switch if a dedicated task
        // just emptied out.
        if self.advance_through_create(outcome.id, ts) {
            self.destroy_shortcut(outcome.id, ts);
            self.stack.retain(|x| *x != outcome.id);
            if switching && self.stack.is_empty() {
                if let Some(prev) = prev_fg {
                    self.switch_foreground_stack(prev);
                }
            }
            if let Some(new_top) = self.stack.last().copied() {
                self.drive_to_resumed(new_top, ts);
            }
            return;
        }
        self.try_restore_state(outcome.id, package_id, class);
        self.drive_start_to_resumed(outcome.id, ts);
    }

    /// Stone 9.5 — the for-result variant of `launch_app`. Identical
    /// plumbing except we record the (caller, request_code) pair against
    /// the new callee id so `finish_top` can deliver
    /// `on_activity_result` when the callee eventually finishes.
    ///
    /// If the resolver lands on a reuse path (SingleTop / SingleTask /
    /// SingleInstance), the for-result contract is **silently** dropped:
    /// `launch_for_result` against an existing live activity is
    /// ill-defined (the callee never "finishes" in the sense that
    /// triggers delivery), and we don't want to synthesise a result
    /// mid-session that the reused activity didn't ask for. This
    /// matches Android's "RESULT_CANCELED immediately on SingleTask
    /// target" escape hatch — the caller gets nothing, which is at
    /// least debuggable via absence of on_activity_result.
    fn launch_app_for_result(
        &mut self,
        intent: &Intent,
        package_id: &str,
        class: &str,
        caller_id: ActivityId,
        request_code: u32,
        ts: f64,
    ) {
        let outcome = match self.activities.launch(package_id, class) {
            Ok(o) => o,
            Err(_) => return,
        };
        let target_task = self.activities.record(outcome.id)
            .expect("just launched")
            .task;
        if outcome.reused {
            // Reuse branch — drop the request_code on the floor. The
            // caller's `on_activity_result` simply never fires for this
            // request. See doc comment above.
            for popped_id in &outcome.popped {
                self.destroy_with_persist(*popped_id, ts);
                self.stack.retain(|x| *x != *popped_id);
            }
            self.switch_to_task_if_needed(target_task, ts);
            self.drive_start_to_resumed(outcome.id, ts);
            self.deliver_new_intent(outcome.id, intent, ts);
            return;
        }
        // Fresh-create — same shape as the fresh-create branch of
        // launch_app, but record the request-code mapping first so the
        // finish path sees it even if on_create finishes immediately
        // via the Created→Destroyed shortcut.
        self.pending_result_for.insert(outcome.id, (caller_id, request_code));
        let prev_fg = self.activities.foreground_task();
        let switching = Some(target_task) != prev_fg;
        if switching {
            if let Some(prev_top) = self.stack.last().copied() {
                self.drive_to_paused(prev_top, ts);
            }
            self.switch_foreground_stack(target_task);
        } else {
            if let Some(prev_top) = self.stack.last().copied() {
                self.drive_to_paused(prev_top, ts);
            }
            self.stack.push(outcome.id);
        }
        if self.advance_through_create(outcome.id, ts) {
            self.destroy_shortcut(outcome.id, ts);
            // destroy_shortcut bypasses destroy_with_persist — it fires
            // on_destroy only, no pause/stop, no save-state. But the
            // caller still needs a result, so deliver CANCELED explicitly.
            self.deliver_activity_result_for(outcome.id, ts);
            self.stack.retain(|x| *x != outcome.id);
            if switching && self.stack.is_empty() {
                if let Some(prev) = prev_fg {
                    self.switch_foreground_stack(prev);
                }
            }
            if let Some(new_top) = self.stack.last().copied() {
                self.drive_to_resumed(new_top, ts);
            }
            return;
        }
        self.try_restore_state(outcome.id, package_id, class);
        self.drive_start_to_resumed(outcome.id, ts);
    }

    // ── Task switching (Stone 9.6) ───────────────────────────────────────

    /// Public Shell-facing API: bring `task_id` to the foreground,
    /// pausing the old top and resuming the new task's top. No-op if the
    /// task is already foreground or doesn't exist. The previous
    /// foreground is pushed onto `task_history` so `finish_top` can
    /// return there when the current foreground empties out.
    pub fn switch_to_task(&mut self, task_id: TaskId, ts: f64) {
        self.switch_to_task_if_needed(task_id, ts);
    }

    /// Switch foreground to `target_task` if it isn't already. Drives
    /// the old top to `Paused` and the new top to `Resumed`; no-op when
    /// the target is already foreground (or unknown). Safe to call from
    /// any launch path after the target activity's state has been
    /// settled by ActivityManager.
    fn switch_to_task_if_needed(&mut self, target_task: TaskId, ts: f64) {
        if self.activities.foreground_task() == Some(target_task) { return; }
        if let Some(prev_top) = self.stack.last().copied() {
            self.drive_to_paused(prev_top, ts);
        }
        self.switch_foreground_stack(target_task);
        if let Some(new_top) = self.stack.last().copied() {
            self.drive_start_to_resumed(new_top, ts);
        }
    }

    /// Swap the foreground task in ActivityManager and re-sync `fw.stack`
    /// to the new task's stack. Records the previous foreground in
    /// `task_history` so a later `finish_top` that empties the new
    /// foreground can restore it. No lifecycle callbacks fire here —
    /// callers own pause/resume.
    fn switch_foreground_stack(&mut self, target_task: TaskId) {
        if let Some(prev) = self.activities.bring_task_to_front(target_task) {
            self.task_history.push(prev);
        }
        self.stack = self.activities
            .task_stack(target_task)
            .map(|s| s.to_vec())
            .unwrap_or_default();
    }

    /// If `callee_id` was launched via `start_activity_for_result`,
    /// deliver `on_activity_result` to the caller now. Drains
    /// `pending_result[callee_id]` (falling back to CANCELED if absent)
    /// and clears both pending maps. No-op for activities not launched
    /// for a result. Called on the destroy-path of the callee, before
    /// the caller is resumed.
    fn deliver_activity_result_for(&mut self, callee_id: ActivityId, ts: f64) {
        let Some((caller_id, request_code)) = self.pending_result_for.remove(&callee_id) else {
            // Not a for-result launch; but still clear any orphaned
            // pending_result bytes so they don't leak.
            self.pending_result.remove(&callee_id);
            return;
        };
        let result = self.pending_result.remove(&callee_id)
            .unwrap_or_else(result::ActivityResult::canceled);
        let (post, _) = self.with_ctx(caller_id, ts, |a, ctx| {
            a.on_activity_result(ctx, request_code, &result);
        });
        self.apply_post(post, caller_id, ts);
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
        self.apply_post(post, id, ts);
    }

    /// Finish the top of the stack and resume whatever it was covering.
    /// If the stack is already down to its last entry (the Shell), this
    /// is a no-op — we never destroy the home-screen.
    ///
    /// Delivery order for a for-result callee (Stone 9.5): destroy the
    /// callee first (so `on_destroy` fires + save-state persists),
    /// then `on_activity_result` on the caller while it's still
    /// `Paused`, then drive the caller up to `Resumed`. This matches
    /// Android's documented ordering (result before resume) and means
    /// a caller that acts on the result in `on_activity_result`
    /// observes its own state-flip from Paused→Resumed afterwards as
    /// a single coherent sequence.
    fn finish_top(&mut self, ts: f64) {
        let Some(id) = self.stack.last().copied() else { return; };

        // Stone 9.6 — if this finish would empty the foreground task
        // (e.g. a SingleInstance living alone in its dedicated task
        // calling `finish()`), revert to the most recently visited
        // task so the display isn't stranded on an empty stack. When
        // there's no prior task to return to (which in practice means
        // we're about to finish the Shell itself from an uncovered
        // foreground), refuse — same guard as before. The Shell's own
        // task always has the Shell at position 0, so the same-task
        // path below handles `len > 1` without ever hitting this
        // branch.
        if self.stack.len() == 1 {
            let Some(prev_task) = self.task_history.pop() else { return; };
            self.stack.pop();
            self.destroy_with_persist(id, ts);
            self.deliver_activity_result_for(id, ts);
            self.switch_foreground_stack(prev_task);
            // `switch_foreground_stack` pushed the just-emptied task
            // back onto history when flipping foreground; drop it so a
            // future finish in the restored task doesn't bounce back
            // to the dead one.
            self.task_history.pop();
            if let Some(new_top) = self.stack.last().copied() {
                self.drive_to_resumed(new_top, ts);
            }
            return;
        }

        self.stack.pop();
        self.destroy_with_persist(id, ts);
        self.deliver_activity_result_for(id, ts);
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
            let mut launch_for_result: Option<(Intent, u32)> = None;
            let mut set_result: Option<result::ActivityResult> = None;
            let mut ctx = ActivityContext {
                metrics, timestamp_ms: ts, pointer,
                finish_requested: &mut finish,
                launch_requested: &mut launch,
                start_for_result_requested: &mut launch_for_result,
                set_result_requested: &mut set_result,
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
    launch_for_result: Option<(Intent, u32)>,
    set_result: Option<result::ActivityResult>,
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
        on_pointer_set_result: Option<result::ActivityResult>,
        on_pointer_finish: bool,
    }

    impl ScriptedActivity {
        fn new(name: &'static str, log: Rc<RefCell<Vec<String>>>) -> Self {
            Self {
                name, log,
                on_pointer_launch: None,
                on_pointer_set_result: None,
                on_pointer_finish: false,
            }
        }

        fn with_pointer_launch(mut self, intent: Intent) -> Self {
            self.on_pointer_launch = Some(intent);
            self
        }

        /// Stone 9.5 — on pointer, stash a result and finish. Simulates
        /// a picker activity where the user taps a value.
        fn with_pointer_set_result_then_finish(
            mut self, result: result::ActivityResult,
        ) -> Self {
            self.on_pointer_set_result = Some(result);
            self.on_pointer_finish = true;
            self
        }

        /// Stone 9.5 — on pointer, just finish (no set_result). Simulates
        /// a picker the user dismisses without picking.
        fn with_pointer_finish(mut self) -> Self {
            self.on_pointer_finish = true;
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
        fn on_activity_result(
            &mut self,
            _: &mut ActivityContext<'_>,
            request_code: u32,
            result: &result::ActivityResult,
        ) {
            self.note(&format!(
                "activity_result:{}:code={}:data={}",
                request_code, result.code,
                String::from_utf8_lossy(&result.data),
            ));
        }
        fn on_pointer(&mut self, ctx: &mut ActivityContext<'_>, _ev: activity::PointerEvent) {
            self.note("pointer");
            if let Some(intent) = self.on_pointer_launch.clone() {
                *ctx.launch_requested = Some(intent);
            }
            if let Some(r) = self.on_pointer_set_result.clone() {
                *ctx.set_result_requested = Some(r);
            }
            if self.on_pointer_finish {
                *ctx.finish_requested = true;
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
            pending_result_for: HashMap::new(),
            pending_result: HashMap::new(),
            task_history: Vec::new(),
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

    // ── Stone 9.5: start_activity_for_result + on_activity_result ────────

    /// Common scaffold for the 9.5 tests: a Shell that launches a
    /// "picker" activity for a result on pointer, plus a picker factory
    /// built by the caller. Both shell and picker write to `log` so
    /// assertions can inspect the whole sequence.
    fn for_result_harness(
        log: Rc<RefCell<Vec<String>>>,
        build_picker: impl Fn() -> ScriptedActivity + 'static,
    ) -> (Framework, ActivityId) {
        let mut fw = empty_framework();

        // Shell that launches the picker for-result on pointer. We use a
        // small custom activity rather than ScriptedActivity so we can
        // attach a request_code to the launch — ScriptedActivity only
        // exposes plain start_activity via pointer.
        struct ShellActivity {
            log: Rc<RefCell<Vec<String>>>,
            intent: Intent,
            request_code: u32,
        }
        impl activity::Activity for ShellActivity {
            fn on_create(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push("shell:create".into());
            }
            fn on_start(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push("shell:start".into());
            }
            fn on_resume(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push("shell:resume".into());
            }
            fn on_pause(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push("shell:pause".into());
            }
            fn on_stop(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push("shell:stop".into());
            }
            fn on_destroy(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push("shell:destroy".into());
            }
            fn on_pointer(&mut self, ctx: &mut ActivityContext<'_>, _: activity::PointerEvent) {
                self.log.borrow_mut().push("shell:pointer".into());
                *ctx.start_for_result_requested = Some((self.intent.clone(), self.request_code));
            }
            fn on_activity_result(
                &mut self,
                _: &mut ActivityContext<'_>,
                request_code: u32,
                result: &result::ActivityResult,
            ) {
                self.log.borrow_mut().push(format!(
                    "shell:activity_result:{}:code={}:data={}",
                    request_code, result.code,
                    String::from_utf8_lossy(&result.data),
                ));
            }
            fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
        }

        let log_shell = log.clone();
        let pick_intent = Intent::explicit("test.pkg", "Picker");
        fw.activities.register("test.shell", "Main", LaunchMode::SingleTop, move || {
            Box::new(ShellActivity {
                log: log_shell.clone(),
                intent: pick_intent.clone(),
                request_code: 7,
            })
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.shell".into(),
            activity_class: "Main".into(),
            actions: vec![action::MAIN.into()],
            categories: vec![category::HOME.into()],
            data_schemes: vec![], mime_types: vec![],
        });

        // Picker — the caller's factory produces a fresh instance per
        // launch so tests that exercise destroy-then-relaunch don't
        // need to re-build the whole harness.
        fw.activities.register("test.pkg", "Picker", LaunchMode::Standard, move || {
            Box::new(build_picker())
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.pkg".into(),
            activity_class: "Picker".into(),
            actions: vec![action::MAIN.into()],
            categories: vec![], data_schemes: vec![], mime_types: vec![],
        });

        let shell_id = fw.activities.launch("test.shell", "Main").unwrap().id;
        fw.stack.push(shell_id);
        fw.drive_to_resumed(shell_id, 0.0);
        log.borrow_mut().clear();

        (fw, shell_id)
    }

    #[test]
    fn start_for_result_delivers_picked_value_to_caller() {
        // Two pointer events drive the flow:
        //   Click 1 → shell launches picker for-result
        //   Click 2 → picker stashes result + finishes
        // Expected sequence:
        //   shell:pointer, shell:pause
        //   picker:create, picker:start, picker:resume    ← first click aftermath
        //   picker:pointer                                ← second click
        //   picker:pause, picker:stop, picker:destroy     ← finish_top
        //   shell:activity_result:7:code=0:data=picked=42 ← delivery
        //   shell:resume                                  ← caller back up
        let picked = result::ActivityResult {
            code: result::RESULT_OK,
            data: b"picked=42".to_vec(),
        };
        let log = Rc::new(RefCell::new(Vec::<String>::new()));
        let log_for_factory = log.clone();
        let picked_for_factory = picked.clone();
        let (mut fw, _shell_id) = for_result_harness(log.clone(), move || {
            ScriptedActivity::new("picker", log_for_factory.clone())
                .with_pointer_set_result_then_finish(picked_for_factory.clone())
        });

        fw.dispatch_pointer(pointer_down(), 1.0);  // shell: launch picker
        fw.dispatch_pointer(pointer_down(), 2.0);  // picker: pick + finish

        assert_eq!(
            &*log.borrow(),
            &[
                "shell:pointer",
                "shell:pause",
                "picker:create",
                "picker:start",
                "picker:resume",
                "picker:pointer",
                "picker:pause",
                "picker:stop",
                "picker:destroy",
                "shell:activity_result:7:code=0:data=picked=42",
                "shell:resume",
            ],
            "for-result round-trip must deliver picked bytes back to shell",
        );
        assert_eq!(fw.stack.len(), 1, "stack must be back to shell-only");
        assert!(fw.pending_result_for.is_empty(),
            "pending_result_for must be cleared after delivery");
        assert!(fw.pending_result.is_empty(),
            "pending_result must be cleared after delivery");
    }

    #[test]
    fn callee_finishing_without_set_result_synthesises_canceled() {
        // Picker finishes without ever calling set_result. The shell
        // must still see on_activity_result, with RESULT_CANCELED and
        // empty data — otherwise the caller would be stuck waiting on
        // a result that never arrives.
        let log = Rc::new(RefCell::new(Vec::<String>::new()));
        let log_for_factory = log.clone();
        let (mut fw, _) = for_result_harness(log.clone(), move || {
            ScriptedActivity::new("picker", log_for_factory.clone()).with_pointer_finish()
        });

        fw.dispatch_pointer(pointer_down(), 1.0);  // shell → launch picker
        fw.dispatch_pointer(pointer_down(), 2.0);  // picker → finish (no result)

        let events = log.borrow().clone();
        // The tail of the sequence is what we care about: pause/stop/destroy
        // of picker, CANCELED delivery, shell resume. Check the last
        // three entries to avoid coupling the test to the full preamble.
        assert_eq!(
            &events[events.len() - 3..],
            &[
                "picker:destroy".to_string(),
                "shell:activity_result:7:code=1:data=".to_string(),
                "shell:resume".to_string(),
            ],
            "callee without set_result must synthesise CANCELED (code=1, empty data)",
        );
        assert!(fw.pending_result_for.is_empty());
        assert!(fw.pending_result.is_empty());
    }

    #[test]
    fn for_result_reuse_path_drops_request_code_silently() {
        // start_activity_for_result against a SingleTop reuse target
        // is ill-defined (the callee never "finishes" in the delivery
        // sense). The framework deliberately drops the request_code
        // rather than synthesising a bogus result. This test locks
        // that decision.
        let log = Rc::new(RefCell::new(Vec::<String>::new()));
        let mut fw = empty_framework();

        // Shell that on pointer launches *itself* via
        // start_for_result. The resolver picks SingleTop reuse →
        // on_new_intent should fire, but no on_activity_result ever.
        struct ShellActivity {
            log: Rc<RefCell<Vec<String>>>,
            intent: Intent,
        }
        impl activity::Activity for ShellActivity {
            fn on_pause(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push("shell:pause".into());
            }
            fn on_resume(&mut self, _: &mut ActivityContext<'_>) {
                self.log.borrow_mut().push("shell:resume".into());
            }
            fn on_pointer(&mut self, ctx: &mut ActivityContext<'_>, _: activity::PointerEvent) {
                self.log.borrow_mut().push("shell:pointer".into());
                *ctx.start_for_result_requested = Some((self.intent.clone(), 99));
            }
            fn on_new_intent(&mut self, _: &mut ActivityContext<'_>, _: &Intent) {
                self.log.borrow_mut().push("shell:new_intent".into());
            }
            fn on_activity_result(
                &mut self, _: &mut ActivityContext<'_>, _: u32, _: &result::ActivityResult,
            ) {
                self.log.borrow_mut().push("shell:activity_result".into());
            }
            fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
        }
        let log_shell = log.clone();
        let self_intent = Intent::explicit("test.shell", "Main");
        fw.activities.register("test.shell", "Main", LaunchMode::SingleTop, move || {
            Box::new(ShellActivity { log: log_shell.clone(), intent: self_intent.clone() })
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.shell".into(),
            activity_class: "Main".into(),
            actions: vec![action::MAIN.into()],
            categories: vec![category::HOME.into()],
            data_schemes: vec![], mime_types: vec![],
        });

        let shell_id = fw.activities.launch("test.shell", "Main").unwrap().id;
        fw.stack.push(shell_id);
        fw.drive_to_resumed(shell_id, 0.0);
        log.borrow_mut().clear();

        fw.dispatch_pointer(pointer_down(), 1.0);

        // The sequence: pointer → SingleTop reuse → on_new_intent on
        // the same shell. No on_activity_result in here.
        let events = log.borrow().clone();
        assert!(events.contains(&"shell:new_intent".to_string()),
            "SingleTop reuse must fire on_new_intent");
        assert!(!events.contains(&"shell:activity_result".to_string()),
            "for-result against a reuse target must NOT deliver a result");
        assert!(fw.pending_result_for.is_empty(),
            "no request_code should persist for the dropped reuse path");
    }

    // ── Stone 9.6: multiple tasks / foreground switching ────────────────

    /// Build a Framework with a Shell (SingleTop), a Standard "App" that
    /// will sit on top of the Shell, and a SingleInstance "SI" that
    /// lives in its own dedicated task. Returns the framework with Shell
    /// already resumed (fw.stack = [shell_id]). Tests use
    /// `dispatch_pointer` to trigger the scripted launch of App or SI.
    fn task_switch_harness(
        log: Rc<RefCell<Vec<String>>>,
    ) -> (Framework, ActivityId) {
        let mut fw = empty_framework();
        let log_shell = log.clone();
        fw.activities.register("test.shell", "Main", LaunchMode::SingleTop, move || {
            Box::new(ScriptedActivity::new("shell", log_shell.clone()))
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.shell".into(),
            activity_class: "Main".into(),
            actions: vec![action::MAIN.into()],
            categories: vec![category::HOME.into()],
            data_schemes: vec![], mime_types: vec![],
        });
        let log_app = log.clone();
        fw.activities.register("test.app", "Main", LaunchMode::Standard, move || {
            Box::new(ScriptedActivity::new("app", log_app.clone()))
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.app".into(),
            activity_class: "Main".into(),
            actions: vec!["test.OPEN_APP".into()],
            categories: vec![], data_schemes: vec![], mime_types: vec![],
        });
        let log_si = log.clone();
        fw.activities.register("test.si", "Main", LaunchMode::SingleInstance, move || {
            Box::new(ScriptedActivity::new("si", log_si.clone()))
        });
        fw.intents.register_filter(IntentFilter {
            package_id: "test.si".into(),
            activity_class: "Main".into(),
            actions: vec!["test.OPEN_SI".into()],
            categories: vec![], data_schemes: vec![], mime_types: vec![],
        });

        let shell_id = fw.activities.launch("test.shell", "Main").unwrap().id;
        fw.stack.push(shell_id);
        fw.drive_to_resumed(shell_id, 0.0);
        (fw, shell_id)
    }

    #[test]
    fn singleinstance_fresh_create_allocates_dedicated_task_and_foregrounds_it() {
        // Shell → launch SI. SI lands in its own task; Shell's task is
        // pushed onto the back-stack. Callback sequence must be
        // shell:pause, si:create, si:start, si:resume (the new task
        // becomes foreground as part of the launch).
        let log = Rc::new(RefCell::new(Vec::new()));
        let (mut fw, shell_id) = task_switch_harness(log.clone());
        log.borrow_mut().clear();

        let si_id = fw.launch_via_intent(Intent::new("test.OPEN_SI"), 1.0);

        let shell_task = fw.activities.record(shell_id).unwrap().task;
        let si_task = fw.activities.record(si_id).unwrap().task;
        assert_ne!(shell_task, si_task, "SingleInstance must live in its own task");
        assert_eq!(fw.activities.foreground_task(), Some(si_task),
            "foreground must follow the launched SingleInstance");
        assert_eq!(fw.stack, vec![si_id], "fw.stack mirrors the SI's task");
        assert_eq!(fw.task_history, vec![shell_task],
            "shell's task recorded for back-out");
        assert_eq!(
            &*log.borrow(),
            &["shell:pause", "si:create", "si:start", "si:resume"],
        );
    }

    #[test]
    fn singleinstance_reuse_across_tasks_brings_target_task_forward() {
        // Shell → SI (dedicated task, now foreground) → Shell re-foreground
        // via `switch_to_task` → re-launch SI via intent. SI must be
        // reused (not recreated), its task brought forward, and
        // on_new_intent delivered. Stack of each task is preserved.
        let log = Rc::new(RefCell::new(Vec::new()));
        let (mut fw, shell_id) = task_switch_harness(log.clone());
        let si_id = fw.launch_via_intent(Intent::new("test.OPEN_SI"), 1.0);
        let shell_task = fw.activities.record(shell_id).unwrap().task;
        let si_task = fw.activities.record(si_id).unwrap().task;

        // Shell explicitly asks to switch back to its own task (mimicking
        // a task-switcher UI). SI goes to Paused, Shell resumes.
        log.borrow_mut().clear();
        fw.switch_to_task(shell_task, 2.0);
        assert_eq!(fw.activities.foreground_task(), Some(shell_task));
        assert_eq!(fw.stack, vec![shell_id]);
        assert_eq!(&*log.borrow(), &["si:pause", "shell:resume"]);

        // Re-launching the SI from Shell must reuse the existing instance
        // (not call on_create again) and bring its task back to front.
        log.borrow_mut().clear();
        let reused = fw.launch_via_intent(Intent::new("test.OPEN_SI"), 3.0);
        assert_eq!(reused, si_id, "SingleInstance reuse must return the same id");
        assert_eq!(fw.activities.foreground_task(), Some(si_task));
        assert_eq!(fw.stack, vec![si_id]);
        // Stack of each task preserved across the swap.
        assert_eq!(fw.activities.task_stack(shell_task), Some(&[shell_id][..]));
        assert_eq!(fw.activities.task_stack(si_task), Some(&[si_id][..]));
        assert_eq!(
            &*log.borrow(),
            &["shell:pause", "si:resume", "si:new_intent:test.OPEN_SI"],
            "reuse must fire on_new_intent, not on_create",
        );
    }

    #[test]
    fn task_swap_preserves_each_tasks_stack() {
        // Shell → App (stacks in Shell's task) → SI (dedicated task,
        // foreground) → switch back to Shell's task. Each task's stack
        // must remain intact across the swap.
        let log = Rc::new(RefCell::new(Vec::new()));
        let (mut fw, shell_id) = task_switch_harness(log.clone());
        let app_id = fw.launch_via_intent(Intent::new("test.OPEN_APP"), 1.0);
        let shell_task = fw.activities.record(shell_id).unwrap().task;
        assert_eq!(fw.activities.task_stack(shell_task), Some(&[shell_id, app_id][..]),
            "App stacks into Shell's task");

        let si_id = fw.launch_via_intent(Intent::new("test.OPEN_SI"), 2.0);
        let si_task = fw.activities.record(si_id).unwrap().task;
        assert_eq!(fw.stack, vec![si_id], "fw.stack follows SI's dedicated task");

        // Bring Shell's task back; App's Paused state is unchanged and
        // it's still on top of the Shell task's stack.
        log.borrow_mut().clear();
        fw.switch_to_task(shell_task, 3.0);
        assert_eq!(fw.stack, vec![shell_id, app_id],
            "Shell task's stack [shell, app] preserved");
        assert_eq!(fw.activities.task_stack(si_task), Some(&[si_id][..]),
            "SI task's stack also preserved");
        assert_eq!(fw.activities.state_of(app_id),
            Some(activity_manager::LifecycleState::Resumed),
            "App resumes on top of Shell task");
        assert_eq!(&*log.borrow(), &["si:pause", "app:resume"]);
    }

    #[test]
    fn back_from_singleinstance_returns_to_prev_task() {
        // Shell → SI (dedicated task) → back key. SI's task empties out;
        // foreground must revert to Shell's task and Shell resumes.
        let log = Rc::new(RefCell::new(Vec::new()));
        let (mut fw, shell_id) = task_switch_harness(log.clone());
        let si_id = fw.launch_via_intent(Intent::new("test.OPEN_SI"), 1.0);
        let shell_task = fw.activities.record(shell_id).unwrap().task;

        log.borrow_mut().clear();
        let esc = activity::KeyEvent {
            code: KEY_CODE_BACK,
            modifiers: 0,
            phase: activity::KeyPhase::Down,
        };
        fw.dispatch_key(esc, 2.0);

        assert_eq!(fw.activities.foreground_task(), Some(shell_task));
        assert_eq!(fw.stack, vec![shell_id]);
        assert!(fw.task_history.is_empty(),
            "task_history must drain as we back out of the dedicated task");
        assert_eq!(fw.activities.state_of(si_id),
            Some(activity_manager::LifecycleState::Destroyed),
            "finish destroys the SingleInstance fully");
        assert_eq!(
            &*log.borrow(),
            &["si:pause", "si:stop", "si:destroy", "shell:resume"],
        );
    }

    /// Convenience helper for the 9.6 tests: inject an intent through
    /// the router + launch path and return the resulting activity id.
    /// Mirrors what `apply_post` does for a pointer-driven launch,
    /// without forcing every test to spin up a scripted-pointer
    /// activity.
    impl Framework {
        fn launch_via_intent(&mut self, intent: Intent, ts: f64) -> ActivityId {
            let (pkg, class) = self.intents.resolve(&intent).expect("intent resolves");
            self.launch_app(&intent, &pkg, &class, ts);
            *self.stack.last().expect("stack non-empty after launch")
        }
    }

    // ── Stone 11.0c — boot integration tests ─────────────────────────────
    //
    // These exercise `framework::boot` end-to-end against a MemoryStorage
    // pre-populated by a previous PackageManager::install. The point is
    // to lock in the contract that 11.0c established:
    //
    //   - Shell is no longer special. MAIN+HOME resolution comes from
    //     a regular installed package's manifest.
    //   - `WasmHost::set_grants` is called for *every* installed package,
    //     including the one that happens to claim HOME.
    //   - `WasmHost::set_packages` is called once, with an encoded
    //     snapshot that includes the HOME-claiming package.
    //   - HOME launch path drives the activity to Resumed.
    //   - Missing HOME (no installed package matches) doesn't panic;
    //     boot returns with an empty stack and a synthetic
    //     `<home>` package_error so the kernel can warn.

    /// Manifest for a `sys.shell`-shaped package — declares MAIN+HOME +
    /// LAUNCHER, asks for the two shell-only permissions. The wasm
    /// bytes don't have to be a real wasm module here: the test never
    /// instantiates one (MockHost::register hands out a fake handle).
    const HOME_MANIFEST: &str = r#"
[app]
id = "sys.shell"
name = "Akiva Shell"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
[runtime]
wasm = "shell.wasm"
[[activities]]
id = "main"
class = "ShellActivity"
entry = true
launch_mode = "singleTop"
exported = true
[[activities.intent_filters]]
actions = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.HOME", "akiva.intent.category.LAUNCHER"]
[permissions]
required = ["akiva.permission.QUERY_PACKAGES", "akiva.permission.LAUNCH_ANY_APP"]
"#;

    /// Pre-install a package into a fresh MemoryStorage and return the
    /// storage so `boot()` can rehydrate from it. Mirrors what the
    /// runtime's dev-apps install path does at startup, just without the
    /// wasm-bytes round-trip through OPFS.
    fn storage_with_installed(manifests: &[&str]) -> Box<storage::MemoryStorage> {
        let mut store = storage::MemoryStorage::new();
        let mut pm = package_manager::PackageManager::new();
        let mut pc = permissions::PermissionController::new();
        permissions::declare_mvp_permissions(&mut pc);
        for m in manifests {
            pm.install(&mut store, &mut pc, m, b"\0asm\x01\x00\x00\x00")
                .expect("install");
        }
        Box::new(store)
    }

    /// Bare-bones MockHost for the boot path. Records `set_grants` and
    /// `set_packages` calls so tests can assert the boot wiring; every
    /// other method delegates to a sane default. Distinct from the
    /// `wasm_activity::tests::MockHost` because that one is not exposed
    /// outside its module — duplicating the minimum surface here is
    /// cheaper than restructuring visibility.
    struct BootMockHost {
        next_handle: RefCell<u32>,
        grants: RefCell<Vec<(String, Vec<String>)>>,
        packages_blob: RefCell<Vec<u8>>,
    }
    impl BootMockHost {
        fn new() -> Rc<Self> {
            Rc::new(Self {
                next_handle: RefCell::new(1),
                grants: RefCell::new(Vec::new()),
                packages_blob: RefCell::new(Vec::new()),
            })
        }
    }
    impl WasmHost for BootMockHost {
        fn register(&self, _pkg: &str, _class: &str)
            -> Option<wasm_activity::AppHandle>
        {
            let mut n = self.next_handle.borrow_mut();
            let h = *n; *n += 1;
            wasm_activity::AppHandle::new(h)
        }
        fn unregister(&self, _h: wasm_activity::AppHandle) {}
        fn lifecycle(
            &self, _h: wasm_activity::AppHandle,
            _t: wasm_activity::LifecycleTransition,
        ) -> u64 { 0 }
        fn pointer(
            &self, _h: wasm_activity::AppHandle,
            _ev: activity::PointerEvent, _ts: f64,
        ) {}
        fn key(
            &self, _h: wasm_activity::AppHandle,
            _ev: activity::KeyEvent, _ts: f64,
        ) {}
        fn resize(
            &self, _h: wasm_activity::AppHandle,
            _m: SurfaceMetrics,
        ) {}
        fn render(
            &self, _h: wasm_activity::AppHandle,
            _ts: f64, _out: &mut compositor::FrameCommands,
        ) {}
        fn set_grants(&self, package_id: &str, grants: &[&str]) {
            self.grants.borrow_mut().push((
                package_id.to_string(),
                grants.iter().map(|s| (*s).to_string()).collect(),
            ));
        }
        fn set_packages(&self, bytes: &[u8]) {
            *self.packages_blob.borrow_mut() = bytes.to_vec();
        }
    }

    #[test]
    fn boot_with_installed_home_app_resolves_and_launches_it() {
        // Stone 11.0c — the home-screen path no longer goes through a
        // built-in Shell. A regular installed package that declares
        // MAIN+HOME is what `boot` resolves against.
        let storage = storage_with_installed(&[HOME_MANIFEST]);
        let host = BootMockHost::new();
        let (fw, report) = boot(storage, host.clone());

        // Boot reports no errors — packages_loaded counts the one we
        // installed; package_errors is empty (no <home> synthetic
        // entry, no per-package failures).
        assert_eq!(report.packages_loaded, 1);
        assert!(report.package_errors.is_empty(),
            "no errors expected, got: {:?}", report.package_errors);

        // Stack has exactly one entry — the HOME activity, driven all
        // the way to Resumed by `drive_to_resumed`.
        assert_eq!(fw.stack.len(), 1);
        let home_id = fw.stack[0];
        assert_eq!(
            fw.activities.state_of(home_id),
            Some(activity_manager::LifecycleState::Resumed),
            "HOME app must reach Resumed on boot",
        );

        // set_grants was called for sys.shell with both required
        // permissions. Order isn't guaranteed (Vec from a HashSet),
        // so compare as sets.
        let grants = host.grants.borrow();
        let shell_grants: Vec<&str> = grants.iter()
            .find(|(pkg, _)| pkg == "sys.shell")
            .map(|(_, g)| g.iter().map(String::as_str).collect())
            .expect("sys.shell grants entry");
        let want: std::collections::HashSet<&str> = [
            "akiva.permission.QUERY_PACKAGES",
            "akiva.permission.LAUNCH_ANY_APP",
        ].into_iter().collect();
        let got: std::collections::HashSet<&str> = shell_grants.iter().copied().collect();
        assert_eq!(got, want);

        // set_packages was called with a non-empty blob — the encoded
        // snapshot should decode back to one record (sys.shell).
        let blob = host.packages_blob.borrow();
        let decoded = pkgs_wire::decode(&blob).expect("blob decodes");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].id, "sys.shell");
        assert_eq!(decoded[0].class, "ShellActivity");
        assert_eq!(decoded[0].name, "Akiva Shell");
    }

    #[test]
    fn boot_self_heals_grants_when_permissions_blob_missing() {
        // Stone 11.0c — covers the dev-apps install path bug: worker.js
        // writes manifest.toml + app.wasm directly to OPFS without ever
        // calling PackageManager::install, so `sys/permissions/{id}` is
        // never written. Without the self-heal a package's gated host
        // imports would be stubbed at instantiation and the user would
        // see a Shell with an empty launcher and no obvious cause.
        //
        // Simulate: install via PackageManager (which writes the perms
        // blob), then delete the blob to mimic the dev-apps shape.
        let mut store = storage::MemoryStorage::new();
        let mut pm = package_manager::PackageManager::new();
        let mut pc = permissions::PermissionController::new();
        permissions::declare_mvp_permissions(&mut pc);
        pm.install(&mut store, &mut pc, HOME_MANIFEST,
            b"\0asm\x01\x00\x00\x00").expect("install");
        // Mimic the dev-apps stage: blow away the permissions blob.
        store.delete("sys/permissions/sys.shell").expect("delete");

        // Boot from this storage — the package list rehydrates, the
        // permissions blob is missing, the self-heal kicks in.
        let host = BootMockHost::new();
        let (_fw, report) = boot(Box::new(store), host.clone());

        assert!(report.package_errors.is_empty(),
            "self-heal should silently re-grant; got errors: {:?}",
            report.package_errors);

        // Host received the grant push for sys.shell with the
        // healed-from-manifest set, not an empty list.
        let grants = host.grants.borrow();
        let shell_grants: Vec<&str> = grants.iter()
            .find(|(pkg, _)| pkg == "sys.shell")
            .map(|(_, g)| g.iter().map(String::as_str).collect())
            .expect("sys.shell grants entry");
        assert_eq!(shell_grants.len(), 2,
            "self-heal must re-grant both shell-only perms; got {:?}",
            shell_grants);
    }

    #[test]
    fn boot_without_home_app_logs_error_and_continues() {
        // Pre-Stone-11.0c this path panicked (`expect("HOME resolves")`);
        // 11.0c relaxed it to a warning so a fresh checkout that hasn't
        // run `akiva build` yet boots cleanly. Display sits empty, kernel
        // logs the package_error from the report.
        let storage = Box::new(storage::MemoryStorage::new());
        let host = BootMockHost::new();
        let (fw, report) = boot(storage, host);

        assert_eq!(fw.stack.len(), 0,
            "no HOME means no foreground activity");
        assert!(report.package_errors.iter().any(|(id, msg)|
            id == "<home>" && msg.contains("MAIN+HOME")),
            "expected synthetic <home> entry in package_errors, got: {:?}",
            report.package_errors);
    }
}
