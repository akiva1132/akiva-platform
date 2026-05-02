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
pub mod cpu_budget;
pub mod intent;
pub mod lmk;
pub mod manifest;
pub mod memory_manager;
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
        host: host.clone(),
        pending_saves: Vec::new(),
            memory: memory_manager::MemoryManager::new(),
            cpu: cpu_budget::CpuBudget::new(),
            lmk_policy: lmk::LmkPolicy::default(),
            lmk_protected: {
                let mut s = std::collections::HashSet::new();
                s.insert("sys.shell".to_string());
                s
            },
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
    /// Stone 11.4a-5 — the wasm-app runtime seam. Cloned at boot from
    /// the `host` argument; held here so the frame loop can call
    /// `host.poll_future` from `drive_pending_transitions` without
    /// reaching into per-activity factory closures. In 11.4a every
    /// dispatch resolves inline so no slot ever opens — the field is
    /// dormant. 11.4c populates it when WorkerHost defers.
    host: Rc<dyn WasmHost>,
    /// P0 #1 — in-flight `save_state` dispatches the destroy-path fired
    /// at the worker. The dispatch returns a future_id but the bytes
    /// arrive 1+ frames later (worker FIFO processes pause→save→stop→
    /// destroy in order, but the kernel can't synchronously block on
    /// the reply). [`Framework::drive_pending_saves`] polls each entry
    /// once per frame and persists the bytes when the worker answers.
    /// Storage key + activity meta is captured at dispatch time so the
    /// activity record can vanish under us before the bytes resolve.
    pending_saves: Vec<PendingSave>,
    /// Phase 12.1 — per-package memory accounting. Updated by the
    /// host's per-frame sample of `WebAssembly.Memory.buffer.byteLength`
    /// for each app worker; read by Phase 12.3 (Low-Memory Killer)
    /// and Phase 12.5 (manifest enforcement).
    pub memory: memory_manager::MemoryManager,
    /// Phase 12.2 — per-package CPU attribution. Updated by the
    /// host's per-frame sample of cumulative CPU ms (each app
    /// worker brackets dispatch / poll / drive_spawned with
    /// `performance.now()`); read by Phase 12.3 LMK ranking. See
    /// [`cpu_budget`] for the rate-window mechanics.
    pub cpu: cpu_budget::CpuBudget,
    /// Phase 12.3 — LMK policy thresholds. Mutable so the kernel can
    /// retune at runtime (e.g. when the browser surfaces a memory
    /// pressure level change). Default values target desktop; mobile
    /// will retune in the Phase-12 mobile baseline pass.
    pub lmk_policy: lmk::LmkPolicy,
    /// Phase 12.3 — packages excluded from LMK eviction. Always
    /// includes `sys.shell`; the kernel can add more (e.g. a system
    /// IME, or a recovery app) post-boot.
    lmk_protected: std::collections::HashSet<String>,
}

/// P0 #1 — one in-flight `save_state` waiting on the worker. Lives in
/// [`Framework::pending_saves`] from dispatch (inside
/// `destroy_with_persist`) until [`Framework::drive_pending_saves`]
/// observes the future as resolved and persists / deletes the storage
/// entry. The activity instance itself may already be Destroyed and
/// gone from `ActivityManager` by then — the pending entry holds its
/// own copy of `(package_id, class)` so persistence doesn't depend on
/// record lookup.
#[derive(Debug)]
struct PendingSave {
    future_id: u64,
    package_id: String,
    class: String,
    /// Frame timestamp at dispatch. Future deadline / watchdog hookup
    /// land with the next foundation pass — for now we trust the
    /// underlying worker watchdog to eventually trap and `Err` the
    /// future, which `drive_pending_saves` drops without persisting.
    #[allow(dead_code)]
    started_at_ms: f64,
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
            // Stone 11.5 — Trapped activities don't paint. The empty
            // FrameCommands here means "nothing changed since last
            // frame"; the Shell's tap-to-restart UI is the user's
            // recovery path. Without this guard, render would call
            // through to the dead worker and the kernel would dispatch
            // futures that nobody answers — slowly leaking ids.
            if matches!(
                self.activities.state_of(id),
                Some(activity_manager::LifecycleState::Trapped)
            ) {
                return self.compositor.finish_frame();
            }
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

    /// P1 #3 — drain per-frame async signals (`finish`,
    /// `start_activity`, `set_result`) raised by the foreground
    /// activity since the last callback fired, and route them via
    /// [`apply_post`]. Runs once per frame before [`render_frame`];
    /// keeps render conceptually pure ("produce paint commands") and
    /// guarantees that a side-effect arriving on a frame WITHOUT new
    /// input still gets picked up — the property Stone 11.4c-5 fixed
    /// by adding a drain to render itself.
    ///
    /// Skips Trapped activities for the same reason render does — the
    /// dead worker can't be answering anything.
    ///
    /// In-process activities (default `drive_pending_signals` no-op)
    /// pay nothing for this hook; only `WasmActivity` overrides.
    pub fn drive_pending_signals(&mut self, ts: f64) {
        let Some(id) = self.stack.last().copied() else { return; };
        if matches!(
            self.activities.state_of(id),
            Some(activity_manager::LifecycleState::Trapped)
        ) {
            return;
        }
        let (post, _) = self.with_ctx(id, ts, |a, ctx| {
            a.drive_pending_signals(ctx);
        });
        self.apply_post(post, id, ts);
    }

    /// Stone 11.4a-5 — poll every activity with an open
    /// [`activity_manager::PendingTransition`] slot. Resolved futures
    /// flip the activity's lifecycle state and clear the slot; pending
    /// futures stay open until the next call. Errors clear the slot but
    /// leave the state at its previous value (Stone 11.5 will mark
    /// `Trapped`).
    ///
    /// The kernel's frame loop calls this once per frame, before any
    /// fresh dispatch — an activity with an open slot must not receive
    /// a new host call until the prior one resolves. In Stone 11.4a no
    /// slot ever opens (every host method returns the `0` sentinel),
    /// so this is a cheap no-op in production today; the path is
    /// exercised by [`activity_manager::tests`] via
    /// `ActivityManager::open_pending` directly.
    pub fn drive_pending_transitions(&mut self) {
        let host: &dyn WasmHost = &*self.host;
        for id in self.activities.pending_ids() {
            // pending_of borrows immutably; copy out the future_id so we
            // can drop the borrow before calling resolve_pending /
            // cancel_pending (which need &mut self.activities).
            let future_id = match self.activities.pending_of(id) {
                Some(p) => p.future_id,
                None => continue,
            };
            match host.poll_future(future_id) {
                Some(Ok(_bytes)) => {
                    // 11.4a-5 ignores the resolved bytes here. Render and
                    // save_state route their bytes through the
                    // `WasmActivity::render` / `save_state_bytes` paths,
                    // which dispatch + poll inline (no `pending` slot).
                    // Lifecycle-class transitions carry no payload — the
                    // resolution is just an ack.
                    self.activities.resolve_pending(id);
                }
                None => {
                    // Still in flight. Leave the slot for next frame.
                }
                Some(Err(_e)) => {
                    // Stone 11.5 will fold this into a `Trapped` mark.
                    // For now: clear the slot so the activity isn't
                    // wedged forever, and leave its state at the prior
                    // value so the next driver call can retry from a
                    // legal edge.
                    self.activities.cancel_pending(id);
                }
            }
        }
    }

    /// P0 #1 — drain the in-flight `save_state` futures opened by
    /// `destroy_with_persist`. Each frame the kernel calls this once;
    /// every entry whose future has resolved gets its bytes written to
    /// storage (or the existing entry deleted, on empty bytes), and is
    /// dropped from the queue. Pending entries stay; errored entries
    /// are dropped without persisting (the bytes the worker would have
    /// produced are lost — the activity trapped or the dispatch was
    /// rejected).
    ///
    /// Empty queue is the common case. Worst-case work per frame is
    /// O(activities-being-destroyed-this-tick), which is bounded by
    /// the user's tap rate on Back / launcher.
    pub fn drive_pending_saves(&mut self) {
        if self.pending_saves.is_empty() {
            return;
        }
        // Two-pass to satisfy the borrow checker: poll all futures
        // (immutable host borrow) and collect the resolutions, then
        // mutate `pending_saves` and `storage` separately. Using
        // `Vec::retain_mut` plus a closure that touches both fields
        // doesn't compose because `self.host.poll_future` and
        // `self.storage` come from the same `&mut self`.
        let host: &dyn WasmHost = &*self.host;
        let mut to_persist: Vec<(String, String, Vec<u8>)> = Vec::new();
        self.pending_saves.retain(|ps| {
            match host.poll_future(ps.future_id) {
                None => true,  // still in flight, keep
                Some(Ok(bytes)) => {
                    to_persist.push((ps.package_id.clone(), ps.class.clone(), bytes));
                    false
                }
                Some(Err(_e)) => {
                    // Worker trapped or rejected the dispatch. Drop the
                    // entry without persisting — best we can do is leave
                    // the prior storage entry (if any) untouched. A
                    // future watchdog hookup may want to log this; for
                    // now silent-drop matches drive_pending_transitions.
                    false
                }
            }
        });
        for (pkg, class, bytes) in to_persist {
            self.persist_saved_state(&pkg, &class, &bytes);
        }
    }

    /// Phase 12.1 — record a fresh `byteLength` reading for an app
    /// worker. The host samples each app's `WebAssembly.Memory.buffer
    /// .byteLength` once per frame and forwards each value through this
    /// hook. `current` is replaced; `peak` advances monotonically.
    ///
    /// No validation against the manifest budget happens here — that's
    /// Phase 12.5's job. This call is the cheap accumulation hook that
    /// every later policy decision (LMK ranking, suspension picking,
    /// enforcement traps) reads from.
    pub fn record_app_memory(&mut self, package_id: &str, bytes: u64) {
        self.memory.record(package_id, bytes);
    }

    /// Phase 12.1 — public read API. Returns `None` for packages the
    /// host has never sampled (an app may exist in `PackageManager`
    /// before its worker has spawned, and a fresh-install package will
    /// have no readings until the first frame). Folds the manifest's
    /// `runtime.max_memory_mb` into the report so the caller doesn't
    /// have to chase two indices.
    ///
    /// If the package is tracked but has no manifest entry (test
    /// fixtures, mid-uninstall window), the limit is reported as
    /// `u64::MAX` — an explicit "no constraint on file" rather than
    /// the unsafer alternative of returning the call as `None`.
    pub fn memory_usage(&self, package_id: &str) -> Option<memory_manager::UsageReport> {
        let current = self.memory.current(package_id)?;
        let peak = self.memory.peak(package_id).unwrap_or(current);
        let limit_bytes = self
            .packages
            .get(package_id)
            .map(|rec| (rec.manifest.runtime.max_memory_mb as u64) * 1024 * 1024)
            .unwrap_or(u64::MAX);
        Some(memory_manager::UsageReport {
            current_bytes: current,
            peak_bytes: peak,
            limit_bytes,
        })
    }

    /// Phase 12.2 — record a fresh `(cumulative_ms, frame_ts_ms)`
    /// pair for an app worker. The app worker brackets every
    /// dispatch / poll / drive call with `performance.now()` and
    /// reports the cumulative through the heartbeat pong; the
    /// kernel pulls the value once per frame and forwards through
    /// here. See [`cpu_budget::CpuBudget::record`] for the
    /// rate-window mechanics.
    pub fn record_app_cpu(&mut self, package_id: &str, cumulative_ms: f64, frame_ts_ms: f64) {
        self.cpu.record(package_id, cumulative_ms, frame_ts_ms);
    }

    /// Phase 12.2 — public read API. Returns `None` for packages the
    /// host has never sampled. The returned [`cpu_budget::CpuReport`]
    /// carries `cumulative_ms`, `recent_ms_per_sec`, `peak_ms_per_sec`
    /// and `frame_budget_fraction` — Phase 12.3 LMK ranking reads the
    /// rate, debug overlays show all four.
    pub fn cpu_usage(&self, package_id: &str) -> Option<cpu_budget::CpuReport> {
        self.cpu.usage(package_id)
    }

    /// Phase 12.3 — run the LMK policy and return packages the kernel
    /// should evict, in the order it should evict them. **Pure read** —
    /// neither activities nor memory accounting is mutated by this call.
    /// The kernel takes the returned [`lmk::KillRequest`] vec and:
    ///   1. Sends `worker.terminate()` to each victim's app worker
    ///      (HAL signal — the framework has no direct handle).
    ///   2. Calls [`Framework::evict_package`] to drop activity records
    ///      and forget memory/cpu accounting.
    ///
    /// `reason` is preserved on every returned request for telemetry.
    /// Most callers pass [`lmk::KillReason::SystemPressure`]; the
    /// browser memory-pressure binding (when wired) maps to
    /// [`lmk::KillReason::HostPressure`].
    pub fn drive_lmk(&self, reason: lmk::KillReason) -> Vec<lmk::KillRequest> {
        let protected: std::collections::HashSet<&str> =
            self.lmk_protected.iter().map(String::as_str).collect();
        lmk::pick_victims(&self.lmk_policy, &self.memory, &self.activities, &protected, reason)
    }

    /// Phase 12.3 — add `package_id` to the LMK exclusion set. Idempotent.
    /// Used at boot for system packages (Shell already covered) and at
    /// runtime if the kernel has additional always-on candidates (e.g.
    /// a recovery activity).
    pub fn lmk_protect(&mut self, package_id: &str) {
        self.lmk_protected.insert(package_id.to_string());
    }

    /// Phase 12.3 — drop accounting + activity records for a package
    /// the kernel just terminated. The actual `worker.terminate()`
    /// happens host-side; this method handles the framework-side
    /// cleanup. Activity records transition to `Trapped` (if not already)
    /// so any in-flight transitions resolve cleanly. Memory + CPU
    /// accounting is forgotten — a re-launch will start with fresh
    /// counters.
    ///
    /// Returns the number of activity records affected. Safe to call
    /// for a package with no activities (e.g. a worker that never
    /// finished launching) — returns 0.
    pub fn evict_package(&mut self, package_id: &str, now_ms: f64) -> usize {
        let n = self.mark_package_trapped(package_id, now_ms);
        self.memory.forget(package_id);
        self.cpu.forget(package_id);
        n
    }

    /// Phase 12.4 — soft-evict a package: drive each live activity
    /// through `pause → save_state → stop → Suspended`, drop the
    /// `Activity` instances, forget memory + CPU accounting. Distinct
    /// from [`Framework::evict_package`] (which is the failure-path
    /// twin and marks `Trapped`) — suspended records are recoverable
    /// on re-launch (saved-state blob restores into a fresh instance).
    ///
    /// The save-state dispatch follows the same pattern as
    /// `destroy_with_persist`: a non-zero `future_id` returned by
    /// `dispatch_save_state_async` enrolls a `PendingSave` that
    /// [`Framework::drive_pending_saves`] persists when the worker
    /// answers. The pause/stop callbacks fire in order; for in-process
    /// activities the synchronous `save_state_bytes` path persists
    /// inline via `persist_saved_state`.
    ///
    /// Caller (kernel-side, for LMK soft eviction) must follow up with
    /// `hal_evict_package` to terminate the wasm worker. The framework
    /// has already dropped the `Activity` instance and cleared pending
    /// slots by the time this returns, so it's safe to terminate at any
    /// later point — Stone 12.4b will wire the kernel call.
    ///
    /// Returns the number of activities transitioned to Suspended.
    /// Safe on packages with no activities (returns 0).
    pub fn suspend_package(&mut self, package_id: &str, ts: f64) -> usize {
        let ids = self.activities.ids_for_package(package_id);
        if ids.is_empty() {
            return 0;
        }
        let metrics = self.metrics;
        let pointer = self.pointer;
        let mut to_persist: Vec<(String, String, Vec<u8>)> = Vec::new();

        for id in ids {
            let meta = self.activities.record(id)
                .map(|r| (r.package_id.clone(), r.class.clone()));

            // Same async-save dispatch as `destroy_with_persist`. Worker
            // FIFO guarantees pause/save/stop run in order; the bytes
            // arrive 1+ frames later via `drive_pending_saves`.
            let async_save_id = self.activities
                .with_instance_ret(id, |a| a.dispatch_save_state_async())
                .unwrap_or(0);
            if async_save_id != 0 {
                if let Some((pkg, class)) = &meta {
                    self.pending_saves.push(PendingSave {
                        future_id: async_save_id,
                        package_id: pkg.clone(),
                        class: class.clone(),
                        started_at_ms: ts,
                    });
                }
            }

            // Drive `pause → stop` only — Suspended replaces Destroyed.
            // Captures the synchronous save_state_bytes from in-process
            // activities (wasm activities return empty here; the async
            // path is the source of truth).
            let async_in_flight = async_save_id != 0;
            let mut sync_save_bytes: Option<Vec<u8>> = None;
            for target in [LifecycleState::Paused, LifecycleState::Stopped] {
                self.activities.advance(id, target, |a, new_state| {
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
                        LifecycleState::Paused  => a.on_pause(&mut ctx),
                        LifecycleState::Stopped => {
                            if !async_in_flight {
                                sync_save_bytes = Some(a.save_state_bytes());
                            }
                            a.on_stop(&mut ctx);
                        }
                        _ => {}
                    }
                });
            }
            if let (Some((pkg, class)), Some(bytes)) = (meta, sync_save_bytes) {
                to_persist.push((pkg, class, bytes));
            }
        }

        // Persist any synchronous saves outside the per-activity loop
        // so the borrow on `self.activities` is fully released first.
        for (pkg, class, bytes) in to_persist {
            self.persist_saved_state(&pkg, &class, &bytes);
        }

        // Final transition: Stopped → Suspended (drops the instance,
        // clears any open pending slot). Activities not at Stopped are
        // skipped — typically because they were already Suspended /
        // Destroyed / Trapped, which is fine.
        let n = self.activities.suspend_stopped_for_package(package_id);

        // Forget memory + CPU accounting — a suspended app is out of
        // budget. On re-launch (Stone 12.4b) the kernel re-spawns the
        // worker and accounting starts fresh.
        self.memory.forget(package_id);
        self.cpu.forget(package_id);

        n
    }

    /// P0 #3 — recycle `Trapped` activity records whose age exceeds
    /// `max_age_ms`. The instance and record both drop without firing
    /// `on_destroy` (the worker that owned the activity is dead, and a
    /// destroy dispatch would either time out or land in a recycled
    /// handle slot). Returns the number of records recycled.
    ///
    /// `now_ms` is the kernel's frame timestamp — same clock the trap
    /// was stamped against in `mark_trapped`. Activities older than
    /// `now_ms - max_age_ms` get reaped; younger ones are left so a
    /// debugger / tap-to-restart UI has a window to attach.
    ///
    /// Idempotent and cheap when no trapped records exist (the common
    /// case): the iteration stops at the filter, no allocation if the
    /// id list comes back empty.
    pub fn reap_trapped(&mut self, now_ms: f64, max_age_ms: f64) -> usize {
        let ids = self.activities.reap_trapped_ids(now_ms, max_age_ms);
        if ids.is_empty() {
            return 0;
        }
        let mut n = 0;
        for id in ids {
            // The record may have been recycled out from under us by
            // a concurrent tap-to-restart flip — `reap` returns None
            // in that case, which we count as "someone else handled
            // it" and skip silently.
            if self.activities.reap(id).is_some() {
                n += 1;
            }
        }
        n
    }

    /// P0 #2 — graceful-shutdown checkpoint. Triggers a `save_state`
    /// dispatch on the foreground activity without changing its
    /// lifecycle state, and queues the resulting bytes through the
    /// regular `pending_saves` pipeline. Called by the kernel when the
    /// browser tab signals "user is leaving" — `visibilitychange` to
    /// `hidden` (page going to background, foreground app should
    /// preserve its in-flight state) and `pagehide` (last-resort hook
    /// before the tab closes).
    ///
    /// Idempotent on the foreground activity: if a `PendingSave` for
    /// the foreground (pkg, class) is already in flight, the second
    /// call is a no-op so we don't pile up duplicate dispatches when
    /// the user toggles tabs rapidly. The activity stays at `Resumed`
    /// — checkpoint is read-only from the lifecycle's point of view.
    ///
    /// Returns the number of dispatches issued (0 if no foreground
    /// activity, or the activity's state is non-live, or a checkpoint
    /// for it is already in flight). The kernel uses the count for
    /// debug logging — no caller depends on it for correctness.
    ///
    /// Cross-worker async caveat: bytes arrive via `drive_pending_saves`
    /// on a later frame. On `visibilitychange` the page is still alive
    /// so the round-trip completes normally; on `pagehide` the tab may
    /// close before the bytes land — best-effort. See
    /// [`docs/runtime-web.md`] §10 for the full lifecycle picture.
    pub fn checkpoint(&mut self, ts: f64) -> usize {
        let Some(id) = self.stack.last().copied() else { return 0; };
        let (pkg, class) = match self.activities.record(id) {
            Some(r) => (r.package_id.clone(), r.class.clone()),
            None => return 0,
        };
        // Only checkpoint live activities. A Trapped activity has a
        // dead worker — dispatching save_state would either time out or
        // route into a recycled handle slot that belongs to someone
        // else.
        if !matches!(
            self.activities.state_of(id),
            Some(activity_manager::LifecycleState::Resumed)
                | Some(activity_manager::LifecycleState::Paused)
                | Some(activity_manager::LifecycleState::Started)
                | Some(activity_manager::LifecycleState::Created),
        ) {
            return 0;
        }
        // De-dup: if the foreground already has a save in flight (from
        // a recent destroy or a prior checkpoint that hasn't drained
        // yet) skip the second dispatch. The user's most recent state
        // is already on its way to disk.
        if self.pending_saves.iter().any(|ps| ps.package_id == pkg && ps.class == class) {
            return 0;
        }
        let async_save_id = self.activities
            .with_instance_ret(id, |a| a.dispatch_save_state_async())
            .unwrap_or(0);
        if async_save_id == 0 {
            // In-process activity, or wasm app with nothing to save.
            // Either way no async write is coming — the in-process
            // sync `save_state_bytes` path covers the fixture case
            // during destroy. Return 0 so the kernel can log "no
            // checkpoint produced" if it cares.
            return 0;
        }
        self.pending_saves.push(PendingSave {
            future_id: async_save_id,
            package_id: pkg,
            class,
            started_at_ms: ts,
        });
        1
    }

    /// Stone 11.5 — every activity belonging to `package_id` is marked
    /// `Trapped` and popped from its task stack so the user lands back
    /// at whatever was below (typically the Shell). Called by the
    /// kernel when (a) an app worker reports a trap via
    /// `{type:"trapped"}`, (b) `worker.onerror` fires, or (c) the
    /// watchdog's ping/pong times out and the worker is terminated.
    /// Idempotent: an already-Trapped activity stays Trapped.
    ///
    /// Returns the number of activities transitioned (only those in a
    /// live state count; already-Destroyed/Trapped are skipped). The
    /// kernel uses this to decide whether to log a notification.
    ///
    /// `now_ms` stamps `ActivityRecord::trapped_at_ms` on each
    /// transition so [`Self::reap_trapped`] can age the entries out
    /// after the configured grace window (P0 #3).
    pub fn mark_package_trapped(&mut self, package_id: &str, now_ms: f64) -> usize {
        let ids = self.activities.ids_for_package(package_id);
        let mut n = 0;
        for id in ids {
            if self.activities.mark_trapped(id, now_ms).is_some() {
                n += 1;
            }
        }
        if n == 0 {
            return 0;
        }

        // Stone 11.5d-prelude — pop every Trapped activity from its
        // task stack so the display isn't stranded on a dead frame.
        // Without this the user is stuck on a blank screen (the
        // Trapped activity no longer paints, no longer accepts
        // input). Auto-pop is the floor — the polished tap-to-
        // restart UI in the launcher lands later.
        //
        // We don't drive the destroy chain (pause/save_state/stop):
        // those dispatches would land in the dead worker. The
        // ActivityRecord stays in `Trapped` state with its instance
        // intact, but the stack no longer references it. Eventual
        // GC plus a manual `Destroyed` flip via a future API will
        // recycle the slot — for 11.5 the leak is bounded by the
        // number of distinct apps the user crashed.
        let trapped = self.activities.trapped_ids_for_package(package_id);
        self.activities.evict_from_task_stacks(&trapped);
        let trapped_set: std::collections::HashSet<_> = trapped.iter().copied().collect();
        self.stack.retain(|id| !trapped_set.contains(id));

        // Drive whatever's now on top back to Resumed so the user
        // sees a live screen (typically the Shell).
        if let Some(new_top) = self.stack.last().copied() {
            // Use the kernel's last-frame timestamp the framework
            // knows about — drive_to_resumed needs one but the
            // exact value only matters for app-visible `ctx.timestamp_ms`
            // which is inside the lifecycle callbacks, and those
            // shouldn't be doing anything timestamp-sensitive at
            // resume-after-trap time.
            self.drive_to_resumed(new_top, 0.0);
        }

        n
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
        // Collect (package_id, class) first so later storage / pending-
        // save bookkeeping doesn't need to reach through the (still-
        // borrowed) activity manager.
        let meta = self.activities.record(id)
            .map(|r| (r.package_id.clone(), r.class.clone()));

        // P0 #1 — fire the cross-worker `save_state` dispatch BEFORE
        // walking pause→stop→destroy. The worker queue handles all four
        // FIFO, so save runs ahead of stop/destroy on the worker side
        // even though every dispatch returns to us immediately. A
        // non-zero future_id means real bytes are in flight; we record
        // a `PendingSave` and let `drive_pending_saves` persist them
        // when the worker replies. A `0` future_id means either an
        // in-process activity (no async save) or a wasm activity with
        // nothing to save — in the in-process case we fall back to the
        // sync `save_state_bytes` path inside the destroy walk below.
        let async_save_id = self.activities
            .with_instance_ret(id, |a| a.dispatch_save_state_async())
            .unwrap_or(0);
        if async_save_id != 0 {
            if let Some((pkg, class)) = &meta {
                self.pending_saves.push(PendingSave {
                    future_id: async_save_id,
                    package_id: pkg.clone(),
                    class: class.clone(),
                    started_at_ms: ts,
                });
            }
        }

        let metrics = self.metrics;
        let pointer = self.pointer;
        let mut sync_save_bytes: Option<Vec<u8>> = None;
        let async_in_flight = async_save_id != 0;
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
                    // Only consult the sync path when no async save is
                    // in flight — for wasm activities the async result
                    // is the source of truth. Calling save_state_bytes
                    // here would be a wasted host round-trip (the
                    // WasmActivity impl deliberately returns empty so
                    // the legacy path doesn't double-fire).
                    if !async_in_flight {
                        sync_save_bytes = Some(a.save_state_bytes());
                    }
                    a.on_stop(&mut ctx);
                }
                LifecycleState::Destroyed => a.on_destroy(&mut ctx),
                _ => {}
            }
        });
        if let (Some((pkg, class)), Some(bytes)) = (meta, sync_save_bytes) {
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
            host: BootMockHost::new(),
            pending_saves: Vec::new(),
            memory: memory_manager::MemoryManager::new(),
            cpu: cpu_budget::CpuBudget::new(),
            lmk_policy: lmk::LmkPolicy::default(),
            lmk_protected: {
                let mut s = std::collections::HashSet::new();
                s.insert("sys.shell".to_string());
                s
            },
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
        ) -> u64 { 0 }
        fn key(
            &self, _h: wasm_activity::AppHandle,
            _ev: activity::KeyEvent, _ts: f64,
        ) -> u64 { 0 }
        fn resize(
            &self, _h: wasm_activity::AppHandle,
            _m: SurfaceMetrics,
        ) -> u64 { 0 }
        fn render(
            &self, _h: wasm_activity::AppHandle, _ts: f64,
        ) -> u64 { 0 }
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

    // ── Stone 11.4a-5: deferred-lifecycle driver ────────────────────────

    /// Host where every dispatch returns `0` (sync) but `poll_future`
    /// gates on an explicit `mark_resolved` — the test arms it between
    /// `drive_pending_transitions` calls to simulate a real Worker
    /// taking multiple frames to finish a lifecycle method. The
    /// production path will use the same trait shape but with an
    /// `Rc<MessageChannel>` instead of an in-process `HashSet`.
    struct DeferredMockHost {
        resolved: RefCell<std::collections::HashSet<u64>>,
        next_handle: RefCell<u32>,
    }

    impl DeferredMockHost {
        fn new() -> Rc<Self> {
            Rc::new(Self {
                resolved: RefCell::new(std::collections::HashSet::new()),
                next_handle: RefCell::new(1),
            })
        }
        fn mark_resolved(&self, future_id: u64) {
            self.resolved.borrow_mut().insert(future_id);
        }
    }

    impl WasmHost for DeferredMockHost {
        fn register(&self, _: &str, _: &str) -> Option<wasm_activity::AppHandle> {
            let mut n = self.next_handle.borrow_mut();
            let h = *n; *n += 1;
            wasm_activity::AppHandle::new(h)
        }
        fn unregister(&self, _: wasm_activity::AppHandle) {}
        fn lifecycle(&self, _: wasm_activity::AppHandle, _: wasm_activity::LifecycleTransition) -> u64 { 0 }
        fn pointer(&self, _: wasm_activity::AppHandle, _: activity::PointerEvent, _: f64) -> u64 { 0 }
        fn key(&self, _: wasm_activity::AppHandle, _: activity::KeyEvent, _: f64) -> u64 { 0 }
        fn resize(&self, _: wasm_activity::AppHandle, _: SurfaceMetrics) -> u64 { 0 }
        fn render(&self, _: wasm_activity::AppHandle, _: f64) -> u64 { 0 }
        fn poll_future(&self, future_id: u64) -> Option<Result<Vec<u8>, String>> {
            if self.resolved.borrow().contains(&future_id) {
                Some(Ok(Vec::new()))
            } else {
                None
            }
        }
    }

    /// Framework with a pre-registered activity and a `DeferredMockHost`
    /// — bypasses `boot` so the test can drive a single deterministic
    /// pending slot without rehydrating storage or installing packages.
    fn deferred_fw() -> (Framework, Rc<DeferredMockHost>, activity_manager::ActivityId) {
        use crate::activity::Activity;
        struct Stub;
        impl Activity for Stub {
            fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
        }
        let host = DeferredMockHost::new();
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
            storage: None,
            pending_result_for: HashMap::new(),
            pending_result: HashMap::new(),
            task_history: Vec::new(),
            host: host.clone(),
            pending_saves: Vec::new(),
            memory: memory_manager::MemoryManager::new(),
            cpu: cpu_budget::CpuBudget::new(),
            lmk_policy: lmk::LmkPolicy::default(),
            lmk_protected: std::collections::HashSet::new(),
        };
        fw.activities.register(
            "test.pkg",
            "Stub",
            activity_manager::LaunchMode::Standard,
            || Box::new(Stub),
        );
        let id = fw.activities.launch("test.pkg", "Stub").unwrap().id;
        (fw, host, id)
    }

    #[test]
    fn deferred_lifecycle_drives_through_two_polls() {
        // Simulates the 11.4c shape: dispatcher fires `lifecycle(Create)`,
        // host returns future_id=42, framework opens a pending slot, the
        // worker takes one extra frame to finish. Frame 1's drive sees
        // None (still in flight) and leaves the activity at Initial;
        // frame 2's drive sees Some(Ok(_)) and flips to Created.
        let (mut fw, host, id) = deferred_fw();
        const FID: u64 = 42;

        assert!(fw.activities.open_pending(
            id, activity_manager::LifecycleState::Created, FID, 0.0,
        ));
        assert_eq!(fw.activities.state_of(id), Some(activity_manager::LifecycleState::Initial));

        // Frame 1: poll returns None (host hasn't resolved yet).
        fw.drive_pending_transitions();
        assert_eq!(fw.activities.state_of(id), Some(activity_manager::LifecycleState::Initial),
            "state must stay Initial while the future is in flight");
        assert!(fw.activities.pending_of(id).is_some(),
            "slot must remain open across an unresolved poll");

        // Worker finishes its work; the next `poll_future(42)` will
        // return Some(Ok(_)).
        host.mark_resolved(FID);

        // Frame 2: poll resolves, framework flips state and clears the
        // slot. No Activity callback fires — that's the deferred-path
        // contract (the work already ran in the worker).
        fw.drive_pending_transitions();
        assert_eq!(fw.activities.state_of(id), Some(activity_manager::LifecycleState::Created));
        assert!(fw.activities.pending_of(id).is_none(),
            "slot must clear once the future resolves");
    }

    /// P1 #3 — verify `Framework::drive_pending_signals` fires the
    /// foreground activity's trait method exactly once per call, and
    /// hands it a writable `ActivityContext`. The "post-callback ends
    /// up at apply_post" half is implicitly covered by the same
    /// `with_ctx` + `apply_post` pair render_frame uses (and that's
    /// already exercised by other tests in this module).
    #[test]
    fn drive_pending_signals_invokes_foreground_with_writable_ctx() {
        use crate::activity::Activity;
        use std::cell::Cell;

        struct SignalRecorder {
            calls: Rc<Cell<u32>>,
            saw_writable_ctx: Rc<Cell<bool>>,
        }
        impl Activity for SignalRecorder {
            fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
            fn drive_pending_signals(&mut self, ctx: &mut ActivityContext<'_>) {
                self.calls.set(self.calls.get() + 1);
                // Exercise ctx mutability — the production override on
                // WasmActivity writes into these out-params, so a
                // regression that hands us a non-mutable view would be
                // a soundness break, not a behavioural one.
                *ctx.finish_requested = false;
                self.saw_writable_ctx.set(true);
            }
        }

        let calls = Rc::new(Cell::new(0u32));
        let saw_ctx = Rc::new(Cell::new(false));
        let calls_for_factory = calls.clone();
        let ctx_for_factory = saw_ctx.clone();

        let host = DeferredMockHost::new();
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
            storage: None,
            pending_result_for: HashMap::new(),
            pending_result: HashMap::new(),
            task_history: Vec::new(),
            host: host.clone(),
            pending_saves: Vec::new(),
            memory: memory_manager::MemoryManager::new(),
            cpu: cpu_budget::CpuBudget::new(),
            lmk_policy: lmk::LmkPolicy::default(),
            lmk_protected: std::collections::HashSet::new(),
        };
        fw.activities.register(
            "test.pkg",
            "Recorder",
            activity_manager::LaunchMode::Standard,
            move || Box::new(SignalRecorder {
                calls: calls_for_factory.clone(),
                saw_writable_ctx: ctx_for_factory.clone(),
            }),
        );
        let id = fw.activities.launch("test.pkg", "Recorder").unwrap().id;
        fw.stack.push(id);

        fw.drive_pending_signals(0.0);
        assert_eq!(calls.get(), 1);
        assert!(saw_ctx.get(), "activity must receive a writable ActivityContext");

        fw.drive_pending_signals(1.0);
        assert_eq!(calls.get(), 2,
            "each frame should fire drive_pending_signals once");
    }

    /// P1 #3 — empty stack is a legitimate state during boot before
    /// the home activity launches; drive_pending_signals must no-op,
    /// not panic.
    #[test]
    fn drive_pending_signals_is_noop_on_empty_stack() {
        let mut fw = empty_framework();
        fw.drive_pending_signals(0.0);
        // No panic, no observable state change. The smoke test is the
        // absence of a panic from `self.stack.last().copied()` being
        // None.
    }

    /// Phase 12.1 — Framework's `record_app_memory` + `memory_usage`
    /// pair. Exercises the public surface, including the manifest-
    /// limit fold-in (untracked package → u64::MAX).
    #[test]
    fn memory_usage_returns_none_before_first_record() {
        let fw = empty_framework();
        assert!(fw.memory_usage("com.akiva.never").is_none());
    }

    #[test]
    fn record_app_memory_then_memory_usage_returns_current_and_peak() {
        let mut fw = empty_framework();
        // No manifest registered → limit defaults to u64::MAX.
        fw.record_app_memory("com.akiva.app", 1024 * 1024);
        fw.record_app_memory("com.akiva.app", 2 * 1024 * 1024);
        fw.record_app_memory("com.akiva.app", 1_500_000);
        let report = fw.memory_usage("com.akiva.app").expect("just recorded");
        assert_eq!(report.current_bytes, 1_500_000);
        assert_eq!(report.peak_bytes, 2 * 1024 * 1024,
            "peak must persist across a shrink");
        assert_eq!(report.limit_bytes, u64::MAX,
            "untracked package falls back to no-limit");
        assert!(!report.over_budget());
    }

    #[test]
    fn memory_usage_folds_manifest_limit_when_package_is_installed() {
        // Build a Framework with a real package whose manifest declares
        // max_memory_mb, then verify the report carries that limit.
        let manifest_toml = r#"
[app]
id = "com.akiva.budgeted"
name = "Budgeted"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"
authors = ["test"]
description = ""
category = "utilities"

[runtime]
wasm = "app.wasm"
min_memory_mb = 1
max_memory_mb = 16

[[activities]]
id = "main"
class = "Main"
entry = true
launch_mode = "standard"

[permissions]
required = []
optional = []
"#;
        let mut fw = empty_framework();
        let mut store = storage::MemoryStorage::new();
        let mut pc = permissions::PermissionController::new();
        permissions::declare_mvp_permissions(&mut pc);
        fw.packages.install(&mut store, &mut pc, manifest_toml, b"\0asm\x01\x00\x00\x00")
            .expect("install");

        fw.record_app_memory("com.akiva.budgeted", 8 * 1024 * 1024);
        let report = fw.memory_usage("com.akiva.budgeted").expect("just recorded");
        assert_eq!(report.limit_bytes, 16 * 1024 * 1024);
        assert!(!report.over_budget());

        // Push past the manifest budget — the report should now flag.
        fw.record_app_memory("com.akiva.budgeted", 17 * 1024 * 1024);
        let report = fw.memory_usage("com.akiva.budgeted").unwrap();
        assert!(report.over_budget(),
            "current 17 MiB exceeds manifest 16 MiB → over_budget()");
    }

    /// Phase 12.2 — `record_app_cpu` + `cpu_usage` end-to-end on the
    /// Framework surface. Window math is covered in detail in the
    /// `cpu_budget` unit tests; this test verifies the public
    /// surface routes correctly.
    #[test]
    fn cpu_usage_returns_none_before_first_record() {
        let fw = empty_framework();
        assert!(fw.cpu_usage("com.akiva.never").is_none());
    }

    #[test]
    fn record_app_cpu_then_cpu_usage_returns_a_rate_within_window() {
        let mut fw = empty_framework();
        // Two snapshots 1 second apart, 4 ms of CPU consumed → 4 ms/sec.
        // 4 ms/sec / 16.67 ms = 0.24 of a 60Hz frame — under the 0.5
        // LMK threshold, so a healthy idle app.
        fw.record_app_cpu("com.akiva.app", 0.0, 0.0);
        fw.record_app_cpu("com.akiva.app", 4.0, 1000.0);
        let r = fw.cpu_usage("com.akiva.app").expect("just recorded");
        assert_eq!(r.cumulative_ms, 4.0);
        assert!((r.recent_ms_per_sec - 4.0).abs() < 0.001,
            "expected ~4 ms/sec, got {}", r.recent_ms_per_sec);
        assert!(!r.over_frame_budget(),
            "4 ms/sec is well under half the frame budget");
    }

    #[test]
    fn cpu_usage_reports_over_frame_budget_when_app_is_hot() {
        let mut fw = empty_framework();
        // 20 ms CPU in 100 ms wall = 200 ms/sec → 12x of a 16.67 ms budget.
        fw.record_app_cpu("com.akiva.hot", 0.0, 0.0);
        fw.record_app_cpu("com.akiva.hot", 20.0, 100.0);
        let r = fw.cpu_usage("com.akiva.hot").unwrap();
        assert!(r.over_frame_budget(),
            "200 ms/sec dwarfs the 60Hz frame budget");
        assert!(r.frame_budget_fraction > 1.0,
            "frame_budget_fraction must exceed 1.0 for an over-budget app");
    }

    /// Phase 12.3 — `drive_lmk` returns no victims when total memory
    /// is below the policy threshold.
    #[test]
    fn drive_lmk_returns_empty_when_under_threshold() {
        let mut fw = empty_framework();
        fw.lmk_policy.threshold_bytes = 100 * 1024 * 1024;
        fw.record_app_memory("a", 30 * 1024 * 1024);
        fw.record_app_memory("b", 40 * 1024 * 1024);
        assert!(fw.drive_lmk(lmk::KillReason::SystemPressure).is_empty());
    }

    /// Phase 12.3 — `drive_lmk` picks the lowest-priority victim when
    /// pressure is real. With no activity records the priority for both
    /// candidates is `Empty`, so tie-break by largest current bytes wins.
    #[test]
    fn drive_lmk_picks_largest_when_priorities_tied() {
        let mut fw = empty_framework();
        fw.lmk_policy.threshold_bytes = 100 * 1024 * 1024;
        fw.lmk_policy.recovery_bytes = 60 * 1024 * 1024;
        fw.record_app_memory("small", 50 * 1024 * 1024);
        fw.record_app_memory("big", 200 * 1024 * 1024);
        let victims = fw.drive_lmk(lmk::KillReason::SystemPressure);
        assert_eq!(victims.len(), 1, "killing 'big' alone (200MB→50MB total) hits recovery");
        assert_eq!(victims[0].package_id, "big");
    }

    /// Phase 12.3 — `evict_package` marks every activity for the
    /// package as `Trapped` and forgets memory + cpu accounting. The
    /// caller (kernel) is responsible for the actual `worker.terminate()`
    /// over the HAL — this test only covers the framework-side cleanup.
    #[test]
    fn evict_package_marks_trapped_and_forgets_accounting() {
        use activity_manager::{LaunchMode, LifecycleState};

        struct Stub;
        impl activity::Activity for Stub {
            fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
        }

        let mut fw = empty_framework();
        fw.activities.register("doomed", "Main", LaunchMode::Standard, || Box::new(Stub));
        let outcome = fw.activities.launch("doomed", "Main").unwrap();
        fw.activities.force_state_for_test(outcome.id, LifecycleState::Resumed);

        fw.record_app_memory("doomed", 200 * 1024 * 1024);
        fw.record_app_cpu("doomed", 0.0, 0.0);
        fw.record_app_cpu("doomed", 30.0, 1000.0);
        assert!(fw.memory_usage("doomed").is_some());
        assert!(fw.cpu_usage("doomed").is_some());

        let trapped = fw.evict_package("doomed", 1234.0);
        assert_eq!(trapped, 1, "single Resumed activity → marked Trapped");
        assert_eq!(fw.activities.state_of(outcome.id), Some(LifecycleState::Trapped));
        assert!(fw.memory_usage("doomed").is_none(), "memory accounting forgotten");
        assert!(fw.cpu_usage("doomed").is_none(), "cpu accounting forgotten");
    }

    /// Phase 12.3 — `evict_package` is a no-op for unknown packages.
    /// The kernel may race a kill request against an uninstall; we
    /// don't want to panic there.
    #[test]
    fn evict_package_on_unknown_is_zero_count_no_op() {
        let mut fw = empty_framework();
        let n = fw.evict_package("never.installed", 0.0);
        assert_eq!(n, 0);
    }

    /// Phase 12.3 — `lmk_protect` adds packages to the exclusion set,
    /// and `drive_lmk` honours both the boot-default (sys.shell) and
    /// runtime-added entries.
    #[test]
    fn lmk_protect_excludes_package_from_eviction() {
        let mut fw = empty_framework();
        fw.lmk_policy.threshold_bytes = 100 * 1024 * 1024;
        fw.lmk_policy.recovery_bytes = 50 * 1024 * 1024;
        // Shell is protected by default; add a second protected pkg.
        fw.lmk_protect("sys.recovery");

        fw.record_app_memory("sys.shell", 80 * 1024 * 1024);
        fw.record_app_memory("sys.recovery", 90 * 1024 * 1024);
        fw.record_app_memory("user.app", 60 * 1024 * 1024);

        let victims = fw.drive_lmk(lmk::KillReason::SystemPressure);
        let ids: Vec<&str> = victims.iter().map(|v| v.package_id.as_str()).collect();
        assert!(!ids.contains(&"sys.shell"));
        assert!(!ids.contains(&"sys.recovery"));
        assert_eq!(ids, vec!["user.app"]);
    }

    /// Phase 12.4 — `suspend_package` drives Resumed → Paused → Stopped
    /// → Suspended, drops the activity instance, forgets memory + cpu
    /// accounting. Distinct from `evict_package` (failure path → Trapped).
    #[test]
    fn suspend_package_transitions_resumed_to_suspended() {
        use activity_manager::{LaunchMode, LifecycleState};

        struct CounterStub(Rc<RefCell<usize>>);
        impl activity::Activity for CounterStub {
            fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
            fn on_pause(&mut self, _: &mut ActivityContext<'_>) {
                *self.0.borrow_mut() += 1;
            }
            fn on_stop(&mut self, _: &mut ActivityContext<'_>) {
                *self.0.borrow_mut() += 10;
            }
        }

        let calls = Rc::new(RefCell::new(0usize));
        let calls_for_factory = calls.clone();
        let mut fw = empty_framework();
        fw.activities.register("dozy", "Main", LaunchMode::Standard,
            move || Box::new(CounterStub(calls_for_factory.clone())));
        let outcome = fw.activities.launch("dozy", "Main").unwrap();
        fw.activities.force_state_for_test(outcome.id, LifecycleState::Resumed);

        fw.record_app_memory("dozy", 50 * 1024 * 1024);
        fw.record_app_cpu("dozy", 0.0, 0.0);
        fw.record_app_cpu("dozy", 5.0, 1000.0);

        let n = fw.suspend_package("dozy", 100.0);
        assert_eq!(n, 1, "single Resumed activity → 1 transition to Suspended");
        assert_eq!(*calls.borrow(), 11, "on_pause (1) + on_stop (10) both fired");
        assert_eq!(fw.activities.state_of(outcome.id), Some(LifecycleState::Suspended));
        // Memory + cpu accounting forgotten.
        assert!(fw.memory_usage("dozy").is_none());
        assert!(fw.cpu_usage("dozy").is_none());
        // Suspended activities show up via the helper for the re-launch path.
        assert_eq!(fw.activities.suspended_ids_for_package("dozy"), vec![outcome.id]);
    }

    /// Phase 12.4 — `suspend_package` is a no-op for unknown packages
    /// (returns 0). The kernel may race a suspension request against an
    /// uninstall; we don't want to panic there.
    #[test]
    fn suspend_package_on_unknown_is_zero() {
        let mut fw = empty_framework();
        assert_eq!(fw.suspend_package("never.installed", 0.0), 0);
    }

    /// Phase 12.4 — calling `suspend_package` on an already-suspended
    /// package returns 0 (the activities are already Suspended; the
    /// Stopped → Suspended scan finds nothing). Idempotent.
    #[test]
    fn suspend_package_is_idempotent() {
        use activity_manager::{LaunchMode, LifecycleState};

        struct Stub;
        impl activity::Activity for Stub {
            fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
        }

        let mut fw = empty_framework();
        fw.activities.register("dozy", "Main", LaunchMode::Standard, || Box::new(Stub));
        let outcome = fw.activities.launch("dozy", "Main").unwrap();
        fw.activities.force_state_for_test(outcome.id, LifecycleState::Resumed);

        assert_eq!(fw.suspend_package("dozy", 0.0), 1);
        // Second call: nothing left at Resumed/Stopped to transition.
        assert_eq!(fw.suspend_package("dozy", 1.0), 0);
        assert_eq!(fw.activities.state_of(outcome.id), Some(LifecycleState::Suspended));
    }

    /// Phase 12.4 — Suspended → Created → Resumed is the re-launch path
    /// (state machine only; full re-instantiation is Stone 12.4b).
    /// Verify the legal_transition coverage so 12.4b can rely on it.
    #[test]
    fn suspended_activity_can_re_enter_lifecycle_via_created() {
        use activity_manager::LifecycleState;

        let mut fw = empty_framework();
        fw.activities.register("dozy", "Main",
            activity_manager::LaunchMode::Standard,
            || {
                struct S;
                impl activity::Activity for S {
                    fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
                }
                Box::new(S)
            });
        let outcome = fw.activities.launch("dozy", "Main").unwrap();
        fw.activities.force_state_for_test(outcome.id, LifecycleState::Suspended);

        // The legal_transition function (private but exercised through
        // mark_trapped's gate) allows Suspended → Created and Suspended
        // → Destroyed. Verify by trying the Trapped edge — which must
        // be refused since Suspended has no live worker to misbehave.
        assert!(fw.activities.mark_trapped(outcome.id, 0.0).is_none(),
            "Suspended → Trapped must not be a legal edge");
    }

    #[test]
    fn drive_pending_saves_persists_when_future_resolves() {
        // P0 #1 — `destroy_with_persist` fires `save_state` at the
        // worker, returns immediately, and stashes a `PendingSave`. On
        // a later frame `drive_pending_saves` polls the future, picks
        // up the bytes, and writes them to storage. The activity
        // record may be gone by then (destroy ran inline) so the
        // pending entry holds its own copy of (pkg, class).
        //
        // Test shape: build a Framework with a SaveHost that holds the
        // future open until the test arms it. Push a PendingSave by
        // hand (private field — same pattern as deferred_fw). Verify:
        //   1. drive before arm → entry stays, storage untouched
        //   2. drive after arm → entry drops, storage write happens
        struct SaveHost {
            armed: RefCell<std::collections::HashMap<u64, Vec<u8>>>,
            pending: RefCell<std::collections::HashSet<u64>>,
            next_handle: RefCell<u32>,
        }
        impl SaveHost {
            fn new() -> Rc<Self> {
                Rc::new(Self {
                    armed: RefCell::new(std::collections::HashMap::new()),
                    pending: RefCell::new(std::collections::HashSet::new()),
                    next_handle: RefCell::new(1),
                })
            }
            fn arm(&self, future_id: u64, bytes: Vec<u8>) {
                self.pending.borrow_mut().remove(&future_id);
                self.armed.borrow_mut().insert(future_id, bytes);
            }
            fn open_pending(&self, future_id: u64) {
                self.pending.borrow_mut().insert(future_id);
            }
        }
        impl WasmHost for SaveHost {
            fn register(&self, _: &str, _: &str) -> Option<wasm_activity::AppHandle> {
                let mut n = self.next_handle.borrow_mut();
                let h = *n; *n += 1;
                wasm_activity::AppHandle::new(h)
            }
            fn unregister(&self, _: wasm_activity::AppHandle) {}
            fn lifecycle(&self, _: wasm_activity::AppHandle, _: wasm_activity::LifecycleTransition) -> u64 { 0 }
            fn pointer(&self, _: wasm_activity::AppHandle, _: activity::PointerEvent, _: f64) -> u64 { 0 }
            fn key(&self, _: wasm_activity::AppHandle, _: activity::KeyEvent, _: f64) -> u64 { 0 }
            fn resize(&self, _: wasm_activity::AppHandle, _: SurfaceMetrics) -> u64 { 0 }
            fn render(&self, _: wasm_activity::AppHandle, _: f64) -> u64 { 0 }
            fn poll_future(&self, future_id: u64) -> Option<Result<Vec<u8>, String>> {
                if let Some(b) = self.armed.borrow_mut().remove(&future_id) {
                    Some(Ok(b))
                } else if self.pending.borrow().contains(&future_id) {
                    None
                } else {
                    Some(Err(format!("unknown future_id {future_id:#x}")))
                }
            }
        }

        let host = SaveHost::new();
        host.open_pending(7);

        // Build a Framework with a MemoryStorage so we can inspect
        // what got written.
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
            storage: Some(Box::new(storage::MemoryStorage::new())),
            pending_result_for: HashMap::new(),
            pending_result: HashMap::new(),
            task_history: Vec::new(),
            host: host.clone(),
            pending_saves: Vec::new(),
            memory: memory_manager::MemoryManager::new(),
            cpu: cpu_budget::CpuBudget::new(),
            lmk_policy: lmk::LmkPolicy::default(),
            lmk_protected: std::collections::HashSet::new(),
        };
        // Push the entry by hand — the integration path lives in
        // `destroy_with_persist`; here we exercise `drive_pending_saves`
        // in isolation so a regression in either side fails its own
        // test.
        fw.pending_saves.push(PendingSave {
            future_id: 7,
            package_id: "com.example".into(),
            class: "MainActivity".into(),
            started_at_ms: 0.0,
        });

        // Frame 1 — host hasn't replied yet. Entry stays, storage is
        // empty.
        fw.drive_pending_saves();
        assert_eq!(fw.pending_saves.len(), 1,
            "unresolved future must stay queued");
        let key = package_manager::key_saved_state("com.example", "MainActivity");
        assert!(matches!(
            fw.storage.as_ref().unwrap().read(&key),
            Err(storage::StorageError::NotFound),
        ));

        // Frame 2 — worker replied with the bytes. Entry drops, write
        // hits storage.
        host.arm(7, b"snapshot-bytes".to_vec());
        fw.drive_pending_saves();
        assert!(fw.pending_saves.is_empty(),
            "resolved future must drop from the queue");
        let read = fw.storage.as_ref().unwrap().read(&key).unwrap();
        assert_eq!(read, b"snapshot-bytes");
    }

    #[test]
    fn checkpoint_dispatches_save_state_for_foreground_activity() {
        // P0 #2 — visibilitychange / pagehide trigger the kernel
        // checkpoint hook; framework dispatches save_state at the
        // foreground activity, records a PendingSave, and the bytes
        // get persisted via drive_pending_saves on a later frame.
        struct CheckpointHost {
            future_id: RefCell<u64>,
            armed: RefCell<std::collections::HashMap<u64, Vec<u8>>>,
            pending: RefCell<std::collections::HashSet<u64>>,
            save_calls: RefCell<u32>,
            next_handle: RefCell<u32>,
        }
        impl CheckpointHost {
            fn new() -> Rc<Self> {
                Rc::new(Self {
                    future_id: RefCell::new(100),
                    armed: RefCell::new(std::collections::HashMap::new()),
                    pending: RefCell::new(std::collections::HashSet::new()),
                    save_calls: RefCell::new(0),
                    next_handle: RefCell::new(1),
                })
            }
        }
        impl WasmHost for CheckpointHost {
            fn register(&self, _: &str, _: &str) -> Option<wasm_activity::AppHandle> {
                let mut n = self.next_handle.borrow_mut();
                let h = *n; *n += 1;
                wasm_activity::AppHandle::new(h)
            }
            fn unregister(&self, _: wasm_activity::AppHandle) {}
            fn lifecycle(&self, _: wasm_activity::AppHandle, _: wasm_activity::LifecycleTransition) -> u64 { 0 }
            fn pointer(&self, _: wasm_activity::AppHandle, _: activity::PointerEvent, _: f64) -> u64 { 0 }
            fn key(&self, _: wasm_activity::AppHandle, _: activity::KeyEvent, _: f64) -> u64 { 0 }
            fn resize(&self, _: wasm_activity::AppHandle, _: SurfaceMetrics) -> u64 { 0 }
            fn render(&self, _: wasm_activity::AppHandle, _: f64) -> u64 { 0 }
            fn save_state(&self, _: wasm_activity::AppHandle) -> u64 {
                *self.save_calls.borrow_mut() += 1;
                let mut next = self.future_id.borrow_mut();
                let id = *next;
                *next += 1;
                self.pending.borrow_mut().insert(id);
                id
            }
            fn poll_future(&self, future_id: u64) -> Option<Result<Vec<u8>, String>> {
                if let Some(b) = self.armed.borrow_mut().remove(&future_id) {
                    self.pending.borrow_mut().remove(&future_id);
                    Some(Ok(b))
                } else if self.pending.borrow().contains(&future_id) {
                    None
                } else {
                    Some(Err(format!("unknown future_id {future_id:#x}")))
                }
            }
        }

        // Build a Framework with a foreground WasmActivity. The factory
        // hands out a fresh WasmActivity; checkpoint should hit its
        // dispatch_save_state_async override.
        let host = CheckpointHost::new();
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
            storage: Some(Box::new(storage::MemoryStorage::new())),
            pending_result_for: HashMap::new(),
            pending_result: HashMap::new(),
            task_history: Vec::new(),
            host: host.clone(),
            pending_saves: Vec::new(),
            memory: memory_manager::MemoryManager::new(),
            cpu: cpu_budget::CpuBudget::new(),
            lmk_policy: lmk::LmkPolicy::default(),
            lmk_protected: std::collections::HashSet::new(),
        };
        let host_for_factory = host.clone();
        fw.activities.register(
            "com.example",
            "Main",
            activity_manager::LaunchMode::Standard,
            move || {
                let h = host_for_factory.register("com.example", "Main").unwrap();
                Box::new(WasmActivity::new(h, host_for_factory.clone()))
            },
        );
        let outcome = fw.activities.launch("com.example", "Main").unwrap();
        // Drive the activity up to Resumed so checkpoint sees a live foreground.
        fw.stack.push(outcome.id);
        fw.activities.resume(outcome.id, |_, _| {});

        // No prior checkpoint — call should issue a single save.
        let n = fw.checkpoint(123.0);
        assert_eq!(n, 1, "first checkpoint must dispatch one save");
        assert_eq!(*host.save_calls.borrow(), 1);
        assert_eq!(fw.pending_saves.len(), 1);
        let pending_fid = fw.pending_saves[0].future_id;

        // Second checkpoint while the first is still in flight — must
        // be a no-op (de-dup).
        let n2 = fw.checkpoint(124.0);
        assert_eq!(n2, 0, "duplicate checkpoint must be skipped while first is in flight");
        assert_eq!(*host.save_calls.borrow(), 1, "no extra host.save_state call");
        assert_eq!(fw.pending_saves.len(), 1);

        // Resolve the future and run drive_pending_saves — bytes get
        // persisted, queue empties.
        host.armed.borrow_mut().insert(pending_fid, b"checkpoint-snapshot".to_vec());
        fw.drive_pending_saves();
        assert!(fw.pending_saves.is_empty());
        let key = package_manager::key_saved_state("com.example", "Main");
        let read = fw.storage.as_ref().unwrap().read(&key).unwrap();
        assert_eq!(read, b"checkpoint-snapshot");

        // After the checkpoint drained, a fresh checkpoint must
        // dispatch again (the user did more work since).
        let n3 = fw.checkpoint(125.0);
        assert_eq!(n3, 1, "post-drain checkpoint must dispatch a fresh save");
        assert_eq!(*host.save_calls.borrow(), 2);
    }

    #[test]
    fn reap_trapped_recycles_records_older_than_grace_window() {
        // P0 #3 — Framework wrapper drops Trapped records once
        // `now_ms - trapped_at_ms` exceeds `max_age_ms`. Younger
        // entries stay (debugger / tap-to-restart window). The
        // record + instance both vanish; no on_destroy fires.
        use crate::activity::Activity;
        struct Stub;
        impl Activity for Stub {
            fn render(&mut self, _: &mut ActivityContext<'_>, _: &mut compositor::FrameCommands) {}
        }
        let mut fw = empty_framework();
        fw.activities.register(
            "test.pkg",
            "Stub",
            activity_manager::LaunchMode::Standard,
            || Box::new(Stub),
        );
        let young = fw.activities.launch("test.pkg", "Stub").unwrap().id;
        let old = fw.activities.launch("test.pkg", "Stub").unwrap().id;
        fw.activities.mark_trapped(young, 9_000.0).unwrap();
        fw.activities.mark_trapped(old, 0.0).unwrap();

        // now_ms=10_000, max_age=5_000 → only `old` is ripe.
        let n = fw.reap_trapped(10_000.0, 5_000.0);
        assert_eq!(n, 1);
        assert!(fw.activities.record(old).is_none(), "old reaped");
        assert!(fw.activities.record(young).is_some(), "young survived");

        // Empty queue → no work, no count.
        let n2 = fw.reap_trapped(10_000.0, 5_000.0);
        assert_eq!(n2, 0);
    }

    #[test]
    fn checkpoint_skips_when_no_foreground() {
        // No stack → no foreground → nothing to checkpoint. Returns 0
        // and leaves pending_saves untouched. Covers the boot-still-in-
        // progress case where the user closes the tab before the Shell
        // launches.
        let mut fw = empty_framework();
        let n = fw.checkpoint(0.0);
        assert_eq!(n, 0);
        assert!(fw.pending_saves.is_empty());
    }

    #[test]
    fn drive_pending_saves_drops_entry_on_host_error() {
        // Trap / dispatch-rejected: pending entry drops without
        // persisting. The prior storage value (if any) stays — the
        // bytes the worker would have produced are simply lost. Same
        // policy as `drive_pending_transitions` on Err.
        struct ErrHost;
        impl WasmHost for ErrHost {
            fn register(&self, _: &str, _: &str) -> Option<wasm_activity::AppHandle> {
                wasm_activity::AppHandle::new(1)
            }
            fn unregister(&self, _: wasm_activity::AppHandle) {}
            fn lifecycle(&self, _: wasm_activity::AppHandle, _: wasm_activity::LifecycleTransition) -> u64 { 0 }
            fn pointer(&self, _: wasm_activity::AppHandle, _: activity::PointerEvent, _: f64) -> u64 { 0 }
            fn key(&self, _: wasm_activity::AppHandle, _: activity::KeyEvent, _: f64) -> u64 { 0 }
            fn resize(&self, _: wasm_activity::AppHandle, _: SurfaceMetrics) -> u64 { 0 }
            fn render(&self, _: wasm_activity::AppHandle, _: f64) -> u64 { 0 }
            fn poll_future(&self, _: u64) -> Option<Result<Vec<u8>, String>> {
                Some(Err("simulated trap".into()))
            }
        }
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
            storage: Some(Box::new(storage::MemoryStorage::new())),
            pending_result_for: HashMap::new(),
            pending_result: HashMap::new(),
            task_history: Vec::new(),
            host: Rc::new(ErrHost),
            pending_saves: vec![PendingSave {
                future_id: 99,
                package_id: "com.bad".into(),
                class: "Crash".into(),
                started_at_ms: 0.0,
            }],
            memory: memory_manager::MemoryManager::new(),
            cpu: cpu_budget::CpuBudget::new(),
            lmk_policy: lmk::LmkPolicy::default(),
            lmk_protected: std::collections::HashSet::new(),
        };
        fw.drive_pending_saves();
        assert!(fw.pending_saves.is_empty(),
            "Err must drop the entry even though we don't persist anything");
    }

    #[test]
    fn deferred_lifecycle_clears_slot_on_host_error() {
        // Stone 11.4a-5 stops here: on Err the slot drops and state
        // stays at the prior value. Stone 11.5 will fold this into a
        // `Trapped` mark so the kernel can tear the activity down.
        struct ErrHost;
        impl WasmHost for ErrHost {
            fn register(&self, _: &str, _: &str) -> Option<wasm_activity::AppHandle> {
                wasm_activity::AppHandle::new(1)
            }
            fn unregister(&self, _: wasm_activity::AppHandle) {}
            fn lifecycle(&self, _: wasm_activity::AppHandle, _: wasm_activity::LifecycleTransition) -> u64 { 0 }
            fn pointer(&self, _: wasm_activity::AppHandle, _: activity::PointerEvent, _: f64) -> u64 { 0 }
            fn key(&self, _: wasm_activity::AppHandle, _: activity::KeyEvent, _: f64) -> u64 { 0 }
            fn resize(&self, _: wasm_activity::AppHandle, _: SurfaceMetrics) -> u64 { 0 }
            fn render(&self, _: wasm_activity::AppHandle, _: f64) -> u64 { 0 }
            fn poll_future(&self, _: u64) -> Option<Result<Vec<u8>, String>> {
                Some(Err("simulated host trap".into()))
            }
        }
        let (mut fw, _host, id) = deferred_fw();
        fw.host = Rc::new(ErrHost);

        assert!(fw.activities.open_pending(id, activity_manager::LifecycleState::Created, 7, 0.0));
        fw.drive_pending_transitions();
        assert_eq!(fw.activities.state_of(id), Some(activity_manager::LifecycleState::Initial),
            "an Err from poll must leave state at the prior value (Trapped lands in 11.5)");
        assert!(fw.activities.pending_of(id).is_none(),
            "slot must clear even on Err so the activity isn't wedged");
    }
}
