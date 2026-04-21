# Akiva ABI — חוזה בין Runtime ל-Applications

## 1. מטרה

ה-ABI מגדיר איך wasm component של אפליקציה מדבר עם ה-runtime. חוזה יציב, מתועד, version-tracked, מתואר ב-**WIT (WebAssembly Interface Types)** של ה-Component Model.

## 2. עקרונות

1. **Component Model + WIT** — ה-ABI מוגדר דרך [WIT](https://component-model.bytecodealliance.org/design/wit.html). אין `extern "C"`, אין raw pointers ידניים, אין alloc/free ידני בצד ה-host. `wit-bindgen` מייצר את קוד ה-glue אוטומטית לכל שפה (Rust, C, Go, Python, JS).
2. **Typed resources** — widgets, activities, intents הם `resource` של WIT, לא `u32 handle`. זה נותן type safety ו-lifetime management שה-runtime אוכף.
3. **Strings, records, lists, options, results** — ברמה native של WIT. אין marshalling ידני.
4. **Version-tracked** — כל world מקבל SemVer (`akiva:app/activity@0.1.0`). breaking changes → major bump.
5. **Capability via imports** — אפליקציה מקבלת רק imports שה-runtime החליט להעניק. Permission denied = import חסר = link error, לא runtime check. ראה סעיף 5.
6. **Additive only בתוך major** — תוספות תואמות-אחורה בתוך gap של major version.

## 3. מבנה המודול

אפליקציית Akiva היא **Component** שמיישמת את ה-world `akiva:app/guest` ומייבאת את ה-world `akiva:app/host`.

```
akiva:app package:
├── world guest      ← מה שהאפליקציה חייבת לייצא (lifecycle + event dispatch)
├── world host       ← מה שה-runtime חושפת (widgets, resources, intents, storage, ...)
└── interfaces:
    ├── lifecycle
    ├── events
    ├── widget
    ├── resources
    ├── intent
    ├── storage
    ├── system
    ├── log
    └── types (shared)
```

---

## 4. WIT Definitions

### 4.1 Shared types

```wit
package akiva:app@0.1.0;

interface types {
    /// Widget identity. Stable for the lifetime of the widget in the tree.
    resource widget;

    /// An active Activity instance (distinct from the declared manifest entry).
    resource activity;

    /// An Intent — a composable message for IPC.
    resource intent;

    /// Opaque handle returned when registering an event listener; used for unregister.
    type listener-id = u32;

    /// Logical pixel value (dp). Runtime applies density.
    type dp = f32;

    /// Scale-independent pixel value (sp). Runtime applies font scale + density.
    type sp = f32;

    /// RGBA in 0xRRGGBBAA layout.
    type rgba = u32;

    /// Monotonic elapsed nanoseconds since boot. Clock source is the host.
    type mono-nanos = u64;

    /// Wall-clock milliseconds since Unix epoch.
    type wall-millis = u64;

    record dimension {
        /// Explicit value; if absent, the widget uses intrinsic sizing.
        value: option<size-value>,
    }

    variant size-value {
        dp(dp),
        percent(f32),
        match-parent,
        wrap-content,
    }

    record insets {
        top: dp,
        right: dp,
        bottom: dp,
        left: dp,
    }

    enum visibility {
        visible,
        invisible,  // takes space, not drawn
        gone,       // no space, not drawn
    }

    enum text-align {
        start,
        center,
        end,
    }

    enum event-kind {
        click,
        long-click,
        text-change,
        focus,
        blur,
        key-down,
        key-up,
        pointer-enter,
        pointer-leave,
    }

    variant event-payload {
        point(tuple<f32, f32>),       // click, pointer
        text(string),                  // text-change
        key(key-event),                // key-down/up
        none,                          // focus/blur/enter/leave
    }

    record key-event {
        code: u32,
        modifiers: u32,  // bitmask: 1=shift 2=ctrl 4=alt 8=meta
    }

    variant widget-error {
        invalid-parent,
        widget-destroyed,
        invalid-value(string),
    }

    variant resource-error {
        not-found,
        wrong-type,
    }

    variant storage-error {
        not-found,
        quota-exceeded,
        io-error(string),
    }

    variant intent-error {
        no-match,
        permission-denied,
        invalid,
    }
}
```

### 4.2 Lifecycle (exports — guest must implement)

```wit
interface lifecycle {
    use types.{activity};

    /// ABI semver the guest targets. Runtime rejects if major mismatch.
    abi-version: func() -> string;  // e.g. "0.1.0"

    on-create: func(act: borrow<activity>, saved-state: option<list<u8>>);
    on-start: func(act: borrow<activity>);
    on-resume: func(act: borrow<activity>);
    on-pause: func(act: borrow<activity>);
    on-stop: func(act: borrow<activity>);
    on-destroy: func(act: borrow<activity>);

    /// Called before on-stop. Returned bytes persist across process kills (<= 64 KiB).
    on-save-state: func(act: borrow<activity>) -> list<u8>;

    /// Config change with bitmask (see types.config-changes). Only called if the
    /// activity declared it handles these changes in manifest; otherwise recreate.
    on-config-change: func(act: borrow<activity>, changes: u32);

    /// Delivered when a new intent arrives for an already-existing instance
    /// (launch modes singleTop / singleTask).
    on-new-intent: func(act: borrow<activity>, in: intent);

    /// Result from start-activity-for-result. `data` is none if canceled.
    on-activity-result: func(
        act: borrow<activity>,
        request-code: u32,
        result-code: s32,
        data: option<intent>,
    );
}

interface events {
    use types.{activity, widget, event-kind, event-payload};

    /// Dispatched for every widget event wired via widget.set-listener.
    on-event: func(
        act: borrow<activity>,
        w: borrow<widget>,
        kind: event-kind,
        payload: event-payload,
    );
}
```

### 4.3 Widget (imports — runtime provides)

```wit
interface widget {
    use types.{widget, activity, insets, size-value, visibility,
              text-align, rgba, sp, dp, event-kind, listener-id, widget-error};

    // ---- Creation ----
    // parent: none => root of the activity's content view.
    create-vstack:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-hstack:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-grid:    func(parent: option<borrow<widget>>, cols: u32, rows: u32)
                      -> result<widget, widget-error>;
    create-frame:   func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-text:    func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-button:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-image:   func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-spacer:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;

    // ---- Lookup ----
    find-by-id: func(act: borrow<activity>, id: string) -> option<widget>;

    // ---- Size & spacing ----
    set-width:   func(w: borrow<widget>, v: size-value);
    set-height:  func(w: borrow<widget>, v: size-value);
    set-padding: func(w: borrow<widget>, p: insets);
    set-margin:  func(w: borrow<widget>, m: insets);
    set-flex:    func(w: borrow<widget>, v: f32);

    // ---- Color ----
    set-background-color: func(w: borrow<widget>, c: rgba);
    set-foreground-color: func(w: borrow<widget>, c: rgba);

    // ---- Visibility & state ----
    set-visibility: func(w: borrow<widget>, v: visibility);
    set-enabled:    func(w: borrow<widget>, on: bool);

    // ---- Text widget ----
    set-text:        func(w: borrow<widget>, text: string);
    set-text-size:   func(w: borrow<widget>, size: sp);
    set-text-align:  func(w: borrow<widget>, a: text-align);
    set-font-weight: func(w: borrow<widget>, weight: u32);   // 400/500/700

    // ---- Arbitrary tag (useful in click handlers) ----
    set-tag: func(w: borrow<widget>, tag: string);
    get-tag: func(w: borrow<widget>) -> option<string>;

    // ---- Tree ----
    clear-children: func(w: borrow<widget>);
    remove:         func(w: widget);                         // own handle → drop
    parent:         func(w: borrow<widget>) -> option<widget>;
    child-count:    func(w: borrow<widget>) -> u32;
    child-at:       func(w: borrow<widget>, index: u32) -> option<widget>;

    // ---- Events ----
    set-listener:    func(w: borrow<widget>, kind: event-kind) -> listener-id;
    clear-listener:  func(w: borrow<widget>, kind: event-kind);
}
```

### 4.4 Resources (imports)

```wit
interface resources {
    use types.{activity, widget, rgba, dp, resource-error};

    get-string: func(ref: string) -> result<string, resource-error>;
    get-string-fmt: func(ref: string, args: list<tuple<string, string>>)
                     -> result<string, resource-error>;
    get-color:  func(ref: string) -> result<rgba, resource-error>;
    get-dimen:  func(ref: string) -> result<dp, resource-error>;

    /// Inflate an AKML layout as a child of `parent` (root of activity if none).
    /// All @+id children become findable via widget.find-by-id.
    inflate-layout: func(
        act: borrow<activity>,
        layout-ref: string,
        parent: option<borrow<widget>>,
    ) -> result<widget, resource-error>;

    /// Drawable returned as opaque handle the Image widget understands.
    resource drawable;
    get-drawable: func(ref: string) -> result<drawable, resource-error>;
    set-image-drawable: func(w: borrow<widget>, d: borrow<drawable>);
}
```

### 4.5 Activity & Intent (imports)

```wit
interface intent {
    use types.{activity, intent, intent-error};

    // ---- Construction ----
    new: func(action: string) -> intent;
    set-data:     func(i: borrow<intent>, uri: string);
    set-mime:     func(i: borrow<intent>, mime: string);
    add-category: func(i: borrow<intent>, category: string);
    set-target:   func(i: borrow<intent>, package: string, activity: string);

    put-string: func(i: borrow<intent>, key: string, value: string);
    put-int:    func(i: borrow<intent>, key: string, value: s64);
    put-bool:   func(i: borrow<intent>, key: string, value: bool);
    put-bytes:  func(i: borrow<intent>, key: string, value: list<u8>);

    // ---- Inspection ----
    get-action:     func(i: borrow<intent>) -> string;
    get-data:       func(i: borrow<intent>) -> option<string>;
    get-mime:       func(i: borrow<intent>) -> option<string>;
    get-categories: func(i: borrow<intent>) -> list<string>;

    get-string:  func(i: borrow<intent>, key: string) -> option<string>;
    get-int:     func(i: borrow<intent>, key: string) -> option<s64>;
    get-bool:    func(i: borrow<intent>, key: string) -> option<bool>;
    get-bytes:   func(i: borrow<intent>, key: string) -> option<list<u8>>;
}

interface activity-api {
    use types.{activity, intent, intent-error};

    set-content-view: func(act: borrow<activity>, layout-ref: string);

    /// Returns an opaque request-id the runtime maps back to the caller on result.
    start-activity: func(act: borrow<activity>, i: intent) -> result<_, intent-error>;

    start-activity-for-result: func(
        act: borrow<activity>,
        i: intent,
        request-code: u32,
    ) -> result<_, intent-error>;

    finish: func(act: borrow<activity>);

    set-result: func(
        act: borrow<activity>,
        code: s32,
        data: option<intent>,
    );

    /// Current intent that started this activity (or the one last delivered
    /// via on-new-intent).
    current-intent: func(act: borrow<activity>) -> option<intent>;
}
```

### 4.6 Storage (imports) — capability gated

The storage interface is **only linked** if the app's manifest declared
`akiva.permission.STORAGE_SANDBOX` (which is auto-granted — it's the app's own
sandbox). Cross-app storage is a separate, user-granted capability.

```wit
interface storage {
    use types.{storage-error};

    /// Per-app sandboxed key-value store backed by OPFS.
    set:    func(key: string, value: list<u8>) -> result<_, storage-error>;
    get:    func(key: string) -> result<list<u8>, storage-error>;
    remove: func(key: string) -> result<_, storage-error>;
    keys:   func() -> list<string>;
    size-of: func(key: string) -> option<u64>;
}
```

### 4.7 System & logging (imports)

```wit
interface log {
    enum level { debug, info, warn, error }
    write: func(level: level, message: string);

    /// Tracing spans — handle is returned as an opaque id that must be passed
    /// to span-end. Runtime emits structured traces to the host observability sink.
    span-begin: func(name: string) -> u32;
    span-end:   func(id: u32);
}

interface system {
    use types.{mono-nanos, wall-millis};

    // ---- Device config snapshot ----
    record device-config {
        locale: string,              // BCP-47, e.g. "en-US"
        layout-direction: layout-dir,
        orientation: orientation,
        ui-mode: ui-mode,
        density: f32,                // 1.0 = 160 dpi baseline
        screen-width-dp: u32,
        screen-height-dp: u32,
    }
    enum layout-dir   { ltr, rtl }
    enum orientation  { portrait, landscape }
    enum ui-mode      { light, dark }

    current-config: func() -> device-config;

    // ---- Clock ----
    monotonic-nanos: func() -> mono-nanos;
    wall-millis:     func() -> wall-millis;

    // ---- Toasts (lightweight notifications, non-interactive) ----
    enum toast-duration { short, long }
    show-toast: func(message: string, duration: toast-duration);

    // ---- Secure RNG ----
    // Backed by the host's CSPRNG (crypto.getRandomValues). Always available.
    random-bytes: func(len: u32) -> list<u8>;
}
```

### 4.8 World composition

```wit
world guest {
    export lifecycle;
    export events;
}

world host {
    import widget;
    import resources;
    import intent;
    import activity-api;
    import storage;      // gated by capability (see section 5)
    import log;
    import system;
}
```

An Akiva app is a component that imports `host` and exports `guest`. The runtime
inspects the component's imports, validates each against the granted capability
set, and instantiates only if every import is permitted.

---

## 5. Capability-via-imports Model

Permissions in Akiva are enforced at **instantiation time**, not at call time.

- The `manifest.toml` declares which capabilities the app requires / may use.
- The PackageManager maps each capability to a concrete set of WIT imports.
- On instantiate, the runtime builds an import map containing **only** the
  imports the app is authorized to use. Calls to missing imports fail at link
  time, not with a runtime permission error.

Example mapping:

| Capability                         | Imports gated                        |
|------------------------------------|--------------------------------------|
| `akiva.permission.STORAGE_SANDBOX` | `storage` (always granted)           |
| `akiva.permission.NETWORK`         | `network` (post-MVP)                 |
| `akiva.permission.CAMERA`          | `camera` (post-MVP)                  |
| `akiva.permission.QUERY_PACKAGES`  | extra methods on `activity-api`      |

Benefits over a runtime-check model:
- **Fast**: no branch on every API call.
- **Unforgeable**: an app with no NETWORK capability cannot synthesize a network
  call by any trick — the function doesn't exist in its import namespace.
- **Auditable**: capability set == linked imports, visible in `akiva build`
  output.

---

## 6. Observability hooks

The `log` interface is always imported. Output is sinked in the host as
follows:

| Sink             | Audience        | Routing                                       |
|------------------|-----------------|-----------------------------------------------|
| Browser console  | Developer       | `log.write` at info+ ; all debug if devtools flag |
| In-app log view  | End user / QA   | Shell's "Developer → Logs" (post-MVP)         |
| Structured trace | Host tooling    | `span-begin` / `span-end` → OT-compatible events |

See [docs/observability.md](observability.md) for how the host wires this up.

---

## 7. Example — Calculator boot (conceptual)

```
Runtime:       instantiate(calc.wasm, imports={widget, resources, intent,
                                                activity-api, storage, log,
                                                system})
Runtime → App: lifecycle.on-create(act, saved-state=none)
App:           activity-api.set-content-view(act, "@layout/main")
Runtime:       (layout inflater walks AKML, calls widget.create-* internally,
                registers listeners declared via onClick="on_digit")
Runtime → App: lifecycle.on-start(act)
Runtime → App: lifecycle.on-resume(act)

User taps "7":
Tab HAL:       pointer-down(100, 300) → Worker
Runtime:       hit-test → widget w12 (button "7")
Runtime → App: events.on-event(act, w12, click, point(100.0, 300.0))
App:           widget.get-tag(w12) → "7"
App:           widget.set-text(display_widget, "7")
Runtime:       next frame re-layout + re-composite
```

---

## 8. Not in v0.1 ABI

- Network (`network` interface — post-MVP).
- Camera / microphone / sensors.
- Custom font loading.
- Animation driver (post-MVP; fade/slide handled by runtime transitions).
- WASI-Threads (when it stabilizes on the browser).
- Broadcast receivers (post-MVP).
- PendingIntent (post-MVP).

## 9. Versioning

- Each interface has its own SemVer. Guest declares via `abi-version()`.
- Runtime keeps a compatibility matrix: "runtime X can host guests with ABI
  0.1.0–0.1.z".
- Breaking changes require major bump; guests pin to a range in manifest
  (`min_abi_version = "0.1.0"`).
