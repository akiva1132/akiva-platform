# Akiva SDK — Rust

## 1. מטרה

ה-SDK הוא Rust crate שאפליקציות תלויות בו. הוא עוטף את ה-WIT bindings (`akiva:app@0.1.0`) ב-API ידומי, בטוח ו-idiomatic. המפתח לא רואה את ה-component model ישירות — כל ה-lifecycle/events/resources/widgets נגישים דרך types גבוהי-רמה.

**הבנייה מתחת למכסה:** `wit-bindgen rust --world guest` מפיק את ה-bindings הגולמיים מה-`wit/akiva.wit`. ה-SDK עוטף אותם ב-macros (`#[akiva::app]`, `#[akiva::callback]`) כדי ש-app-author לא צריך לגעת בהם.

## 2. מודל המנטלי

**אפליקציה = struct + AKML + callbacks.**

- **Struct**: state של האפליקציה. field-ים שמשתנים במהלך הריצה.
- **AKML**: ה-UI מוגדר בחיצוני, ב-`res/layout/*.akml`.
- **Callbacks**: פונקציות שנקראות כשמשהו קורה (click, lifecycle event).

ה-SDK מפעיל את המכונה מאחורי הקלעים — macros מייצרים את כל ה-lifecycle hooks, את registry ה-callbacks, ואת הקישור ל-ABI.

## 3. הגדרת פרויקט

```toml
# apps/my-app/Cargo.toml
[package]
name = "my-akiva-app"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
akiva-sdk = { version = "0.1" }

[profile.release]
lto = true
opt-level = "z"
codegen-units = 1
```

**חשוב:** `crate-type = ["cdylib"]` כדי ש-wasm יהיה module שלם.

```toml
# apps/my-app/manifest.toml
[app]
id = "com.example.my_app"
name = "My App"
version = "0.1.0"
version_code = 1
min_abi_version = "0.1.0"

[runtime]
wasm = "my_akiva_app.wasm"

[[activities]]
id = "main"
entry = true

[[activities.intent_filters]]
actions = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]
```

## 4. Hello World

### res/layout/main.akml

```xml
<?akml version="1.0"?>
<VStack padding="16dp" spacing="8dp" background="@color/surface">
    <Text
        id="@+id/message"
        text="@string/click_counter"
        textSize="20sp"
        textColor="@color/on_surface" />
    <Button
        text="@string/click_me"
        onClick="on_click" />
</VStack>
```

### res/values/strings.toml

```toml
[strings]
app_name = "Hello"
click_counter = "Clicks: 0"
click_me = "Click me"
```

### res/values/colors.toml

```toml
[colors]
surface = "#FFFFFF"
on_surface = "#0C0C10"
```

### src/lib.rs

```rust
use akiva_sdk::prelude::*;

#[akiva::app]
pub struct HelloApp {
    count: i32,
}

impl App for HelloApp {
    fn new() -> Self {
        Self { count: 0 }
    }

    fn on_create(&mut self, ctx: &ActivityContext) {
        ctx.set_content_view("@layout/main");
    }

    fn on_save_state(&self, state: &mut SavedState) {
        state.put_i32("count", self.count);
    }

    fn on_restore_state(&mut self, state: &SavedState) {
        self.count = state.get_i32("count").unwrap_or(0);
    }
}

#[akiva::callback]
fn on_click(app: &mut HelloApp, ctx: &CallbackContext) {
    app.count += 1;
    ctx.update_text("@id/message", &format!("Clicks: {}", app.count));
}
```

### build

```bash
cd apps/my-app
akiva build        # shortcut ל-cargo build --target wasm32-unknown-unknown + wasm-opt
```

## 5. ה-Macros שה-SDK מספק

### `#[akiva::app]`

מסמן את ה-struct של האפליקציה. המאקרו מייצר:
- `akiva_on_create`, `akiva_on_start`, וכו' — כל ה-lifecycle exports
- `akiva_alloc`, `akiva_free` — memory allocator
- static instance management (אפליקציה יש לה instance אחד)
- callback registry

### `#[akiva::callback]`

מסמן פונקציה כ-callback. המאקרו:
- רושם אותה ב-registry עם id אוטומטי
- מאפשר referencing מ-AKML (`onClick="on_click"`)

### `#[akiva::activity]`

(עתידי, לאפליקציות עם כמה activities)

```rust
#[akiva::activity(id = "settings")]
pub struct SettingsActivity { ... }
```

## 6. ה-`App` trait

```rust
pub trait App: Sized + 'static {
    // Required
    fn new() -> Self;
    fn on_create(&mut self, ctx: &ActivityContext);

    // Optional lifecycle
    fn on_start(&mut self, _ctx: &ActivityContext) {}
    fn on_resume(&mut self, _ctx: &ActivityContext) {}
    fn on_pause(&mut self, _ctx: &ActivityContext) {}
    fn on_stop(&mut self, _ctx: &ActivityContext) {}
    fn on_destroy(&mut self, _ctx: &ActivityContext) {}

    // State persistence
    fn on_save_state(&self, _state: &mut SavedState) {}
    fn on_restore_state(&mut self, _state: &SavedState) {}

    // Intent handling
    fn on_new_intent(&mut self, _ctx: &ActivityContext, _intent: &Intent) {}
    fn on_activity_result(
        &mut self,
        _ctx: &ActivityContext,
        _request_code: u32,
        _result: ActivityResult,
    ) {}

    // Config changes (only if declared in manifest)
    fn on_config_change(&mut self, _ctx: &ActivityContext, _changes: ConfigChanges) {}

    // Custom back handling — return true to consume
    fn on_back_pressed(&mut self, _ctx: &ActivityContext) -> bool { false }

    // Raw key events
    fn on_key_event(&mut self, _ctx: &ActivityContext, _event: &KeyEvent) -> bool { false }
}
```

## 7. `ActivityContext`

Handle שמועבר לכל lifecycle callback. דרכו האפליקציה מדברת עם ה-framework.

```rust
impl ActivityContext {
    // UI
    pub fn set_content_view(&self, layout_ref: &str);
    pub fn find_view(&self, id: &str) -> ViewRef;
    pub fn inflate(&self, layout_ref: &str) -> ViewRef;

    // Navigation
    pub fn start_activity(&self, intent: Intent);
    pub fn start_activity_for_result(&self, intent: Intent, request_code: u32);
    pub fn finish(&self);
    pub fn set_result(&self, code: i32, data: Option<Intent>);

    // Intent of current launch
    pub fn intent(&self) -> &Intent;

    // System services
    pub fn package_manager(&self) -> &PackageManager;
    pub fn resources(&self) -> &Resources;

    // UI convenience
    pub fn show_toast(&self, msg: &str);
    pub fn show_toast_long(&self, msg: &str);
}
```

## 8. `ViewRef`

Handle ל-widget spec הוא הדרך לעדכן UI בזמן ריצה:

```rust
impl ViewRef {
    pub fn set_text(&self, text: &str);
    pub fn set_visible(&self, visible: Visibility);
    pub fn set_enabled(&self, enabled: bool);
    pub fn set_background_color(&self, color: Color);
    pub fn set_click_handler(&self, callback_name: &str);
    pub fn set_tag(&self, tag: &str);

    // For containers
    pub fn clear_children(&self);
    pub fn add_child(&self, child: ViewRef);
    pub fn remove(&self);
}
```

## 9. `CallbackContext`

Passed to every `#[akiva::callback]`:

```rust
impl CallbackContext {
    pub fn widget_id(&self) -> WidgetId;
    pub fn tag(&self) -> &str;
    pub fn activity(&self) -> &ActivityContext;

    // Shortcut for finding and updating
    pub fn find(&self, id: &str) -> ViewRef;
    pub fn update_text(&self, id: &str, text: &str);

    // For click callbacks
    pub fn click_position(&self) -> Option<(f32, f32)>;
    pub fn key_event(&self) -> Option<&KeyEvent>;
    pub fn text_input(&self) -> Option<&str>;
}
```

## 10. `Intent` construction

```rust
use akiva_sdk::intent::*;

let intent = Intent::new(actions::MAIN)
    .with_category(categories::LAUNCHER)
    .with_package("com.example.app");

let intent = Intent::new(actions::VIEW)
    .with_data("https://example.com");

let intent = Intent::new(actions::SEND)
    .with_mime_type("text/plain")
    .with_extra("TEXT", IntentValue::Str("Hello!".into()));
```

Constants for actions/categories בהתאם לממסד ב-`docs/intents.md`.

## 11. Resources

```rust
use akiva_sdk::res;

// Get resources manually (rare — most uses go through AKML)
let welcome: String = res::string("@string/welcome");
let primary: Color = res::color("@color/primary");
let padding: Dp = res::dimen("@dimen/spacing_large");

// Format args
let greeting = res::string_fmt("@string/hello_name", &[("name", "Akiva")]);
```

## 12. Storage

```rust
use akiva_sdk::storage;

storage::set("last_value", b"42");

if let Some(bytes) = storage::get("last_value") {
    let s = std::str::from_utf8(&bytes).unwrap_or("");
    // ...
}

storage::remove("last_value");

for key in storage::keys() {
    // ...
}
```

## 13. Logging

```rust
use akiva_sdk::log;

log::debug!("Counter is {}", app.count);
log::info!("App started");
log::warn!("Unusual value: {:?}", value);
log::error!("Failed to parse: {:?}", err);
```

## 14. דוגמה מלאה — Calculator skeleton

### src/lib.rs

```rust
use akiva_sdk::prelude::*;

#[akiva::app]
pub struct Calculator {
    display: String,
    operand: Option<f64>,
    op: Option<Operation>,
    new_entry: bool,
}

#[derive(Clone, Copy)]
enum Operation { Add, Sub, Mul, Div }

impl App for Calculator {
    fn new() -> Self {
        Self {
            display: "0".into(),
            operand: None,
            op: None,
            new_entry: true,
        }
    }

    fn on_create(&mut self, ctx: &ActivityContext) {
        ctx.set_content_view("@layout/main");
    }

    fn on_save_state(&self, state: &mut SavedState) {
        state.put_str("display", &self.display);
        if let Some(op) = self.operand {
            state.put_f64("operand", op);
        }
        // ... (op serialize as string)
    }

    fn on_restore_state(&mut self, state: &SavedState) {
        self.display = state.get_str("display").unwrap_or_else(|| "0".into());
        self.operand = state.get_f64("operand");
    }
}

impl Calculator {
    fn update_display(&self, ctx: &CallbackContext) {
        ctx.update_text("@id/display", &self.display);
    }

    fn current_value(&self) -> f64 {
        self.display.parse().unwrap_or(0.0)
    }
}

#[akiva::callback]
fn on_digit(app: &mut Calculator, ctx: &CallbackContext) {
    let digit = ctx.tag();
    if app.new_entry || app.display == "0" {
        app.display = digit.to_string();
        app.new_entry = false;
    } else {
        app.display.push_str(digit);
    }
    app.update_display(ctx);
}

#[akiva::callback]
fn on_op(app: &mut Calculator, ctx: &CallbackContext) {
    let op = match ctx.tag() {
        "add" => Operation::Add,
        "sub" => Operation::Sub,
        "mul" => Operation::Mul,
        "div" => Operation::Div,
        _ => return,
    };
    app.operand = Some(app.current_value());
    app.op = Some(op);
    app.new_entry = true;
}

#[akiva::callback]
fn on_equals(app: &mut Calculator, ctx: &CallbackContext) {
    if let (Some(lhs), Some(op)) = (app.operand, app.op) {
        let rhs = app.current_value();
        let result = match op {
            Operation::Add => lhs + rhs,
            Operation::Sub => lhs - rhs,
            Operation::Mul => lhs * rhs,
            Operation::Div => if rhs != 0.0 { lhs / rhs } else { f64::NAN },
        };
        app.display = format_number(result);
        app.operand = None;
        app.op = None;
        app.new_entry = true;
        app.update_display(ctx);
    }
}

#[akiva::callback]
fn on_clear(app: &mut Calculator, ctx: &CallbackContext) {
    app.display = "0".into();
    app.operand = None;
    app.op = None;
    app.new_entry = true;
    app.update_display(ctx);
}

fn format_number(v: f64) -> String {
    if v.is_nan() {
        "Error".into()
    } else if v == v.trunc() {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}
```

## 15. Build & Run

```bash
# Create new app
akiva new my-app

# Build wasm
cd my-app
akiva build

# Run (dev server + auto-install)
akiva run
```

## 16. מה ה-SDK **לא** חושף (מכוון)

- ❌ גישה ל-DOM APIs: אין `document`, `window`, `fetch`
- ❌ גישה ישירה ל-canvas: רנדור עובר רק דרך widget tree
- ❌ `std::fs`: storage דרך `storage` API
- ❌ `std::net`: network ב-v0.2+
- ❌ threads: wasm הוא single-threaded לעת עתה
- ❌ JavaScript interop: לא דרוש, ה-runtime כבר מפצה

## 17. עקרונות API

- **Immediate mode friendly**: ViewRef-ים הם סמליים, לא מחזיקים state. האפליקציה יכולה לקבל את אותו ViewRef שוב ושוב.
- **Strong types**: Color, Dp, Sp, Duration — כולם types נפרדים, לא חשופים כ-u32.
- **Result-based errors**: API שיכול להיכשל מחזיר `Result<T, AkivaError>`.
- **Zero-cost abstractions**: ה-macros יוצרים קוד מינימלי נדרש, ללא reflection בזמן ריצה.

## 18. עתיד

- `async` API לפעולות ארוכות (network, file I/O)
- `#[akiva::activity]` macro למספר activities
- Custom widgets (ירושה מ-widget בסיסי)
- Animations API
- Dependency injection של system services

## 19. Known Limitations (Stone 11.6+ backlog)

מסמך זה מתעד פערים אמיתיים של ה-SDK שזיהינו אחרי 11.4c (worker-per-app) ו-11.5 (fault containment) על ה-platform. ראה [`docs/isolation-design.md` §10.5 Phase 11 Status](isolation-design.md) ל-full work plan.

### 19.1 ✅ The async executor — תוקן (P0 #4, May 2026)

**הבעיה הייתה.** ה-`Activity` trait הוא `async fn`, אבל `block_on_ready` עשה `.poll()` פעם אחת בלבד ו-panic על Pending. כל future cross-worker (storage::read) חייב להיות wrapped ב-deferred-handle (`ReadHandle::poll()` ב-on_render) — leaky abstraction שאילצה apps לנהל state ידנית בין frames.

**התיקון בפועל.** ABI חדש (breaking — bumped ABI_VERSION ל-`0.2.0`) + mini-executor משלנו ב-`sdk-rust/src/executor.rs` (~120 שורות, לא embassy). שלושה exports שמחליפים את הזוג הקודם:

```rust
akiva_app_dispatch(dispatch_id: u64, cmd: u32, ptr, len) -> u32   // 0=accept, !=0=refuse
akiva_app_poll_dispatch(dispatch_id: u64) -> u64                   // packed (status, len)
akiva_app_get_result(dispatch_id: u64, out_len_ptr) -> *const u8   // single-shot
```

ה-runtime ב-wasm מחזיק עכשיו `Pin<Box<dyn Future + 'static>>` per-handle, captured-by-raw-ptr ל-`slot.activity` ו-`slot.ctx`. self-referential אבל סגור: ה-Slot יושב ב-`HashMap<u32, Box<Slot>>` (heap-pinned), ה-framework מבטיח dispatch-per-handle אחד בכל זמן (PendingTransition discipline). ראה `crate::executor` ו-`crate::runtime` לכל ה-SAFETY invariants.

**Render ו-save_state נשארים סנכרוניים** — paint code לא צריך לחכות על host I/O, ו-`save_state` הוא `&self` ב-trait. `executor::run_to_ready` עדיין panics על Pending — accidental await ב-on_render יקרוס מיד.

**Storage::read הופך ל-real `Future`:**

```rust
async fn on_create(&mut self, _ctx: &mut Context<'_>) {
    if let Some(fut) = storage::read("notes") {
        if let Ok(Some(bytes)) = fut.await {
            self.text = String::from_utf8_lossy(&bytes).into_owned();
        }
    }
}
```

ה-`.await` legitimately suspends the dispatch — ה-shell rights `pendingDispatches`, מחכה ל-`host_op_done`, re-polls. ReadHandle נמחק.

**Code:** `sdk-rust/src/{executor.rs (new), runtime.rs, storage.rs, lib.rs, activity.rs}`, `runtime/web/app_worker_shell.js`. Tests: 9 חדשים (executor poll_once/run_to_ready, runtime sync dispatch, intern_class dedupe, ReadFuture stub).

**Why not embassy?** embassy-executor ~10KB per app, מובנה ל-interrupt-driven wakers ול-timer drivers שלא רלוונטיים ב-wasm worker. Custom 120-line executor משלנו tightly fitted ל-pattern של one-future-per-handle, with re-poll triggered externally by `host_op_done`. Zero bundle bloat over the legacy stub.

### 19.2 ✅ `save_state` degraded under cross-worker — תוקן (P0 #1, May 2026)

**הבעיה הייתה.** ב-Stone 11.4c, `WasmActivity::save_state_bytes` עשה `host.save_state()` + `poll_future()` באותו frame; cross-worker → poll הראשון תמיד `Pending` → release fall-through ל-`Vec::new()` → Phase 6 saved-state broken.

**התיקון בפועל.** הוסיפו method חדש ל-`Activity` trait — `dispatch_save_state_async(&self) -> u64`. `WasmActivity` override שלו מחזיר את ה-`future_id` מ-`host.save_state()`; ה-`save_state_bytes` הסנכרוני מחזיר `Vec::new()` ל-wasm activities. ה-Framework מחזיק `pending_saves: Vec<PendingSave>`; ב-`destroy_with_persist` הוא קורא ל-`dispatch_save_state_async` לפני `pause→stop→destroy` (ה-worker FIFO מטפל בכולם בסדר), משאיר entry ב-pending_saves. method חדש `Framework::drive_pending_saves` נקרא פעם בכל frame מ-`akiva_frame`, polls כל future, ו-persists bytes כשה-worker עונה. ריק → delete; `Err` → drop ללא persist.

In-process activities (תsts, future built-ins) ממשיכים להשתמש ב-`save_state_bytes` הסנכרוני — ה-framework consults אותו רק כש-`dispatch_save_state_async` החזיר `0`.

**Code surfaces.** `framework/src/{activity.rs, wasm_activity.rs, activity_manager.rs, lib.rs}`, `kernel/src/lib.rs::akiva_frame`. Tests: `drive_pending_saves_persists_when_future_resolves` + `drive_pending_saves_drops_entry_on_host_error`.

### 19.3 ✅ `storage::write` returns `WriteHandle` (Stone 11.6.7 #1, May 2026)

**מה היה הבעיה.** קודם — `pub fn write(key, value)` ללא ערך החזר. ה-host מבצע את ה-write ושולח host_op_done עם status, אבל ה-SDK פשוט abandons את ה-future_id. apps שדורשים durability (settings, notes, drafts) לא יכלו לדעת אם נכשל.

**התיקון.** `pub fn write(key: &str, value: &[u8]) -> WriteHandle` — `WriteHandle: Future<Output = Result<(), StorageError>>`. `let _ = storage::write(k, v).await?;` מקבל confirmation; `let _ = storage::write(k, v);` (drop בלי await) משאיר את ה-pre-Stone fire-and-forget shape — ה-`STORAGE_SET` host_op כבר נשלח בתוך הפונקציה. `WriteHandle` הוא `#[must_use]`, אז `storage::write(k, v);` (ללא let _) מקבל compiler warning לאפליקציות שכותבות data חשוב.

**Errors הצפויים.** `BadKey` (empty key — pre-resolved, ה-host אפילו לא נקרא); `Denied` (host דחה את ה-call לפני שהקצה future_id — חסר `STORAGE_SANDBOX` grant); `Io` (quota / OPFS / kernel-side reason).

**Code.** `sdk-rust/src/storage.rs::{WriteHandle, write, poll_write_inner}`. אין שינוי ב-`runtime/web/app_registry.js::_dispatchStorage` — ה-host כבר היה שולח host_op_done עם status מתאים. 174 sdk tests green (3 חדשים: empty-key BadKey, non-wasm stub Denied, must_use shape).

### 19.4 🟢 Other minor SDK gaps

- **`pkgs_wire` decoder duplicated 3x.** [`framework/src/wire/pkgs.rs`](../framework/src/wire/pkgs.rs), [`sdk-rust/src/pkgs.rs`](../sdk-rust/src/pkgs.rs), ו-`runtime/web/app_registry.js::_warmupWorkers` משתפים את אותו פורמט. drift risk כשmosbim שדה. הפתרון או JSON (לpaths שאינם performance-critical), או schema-driven generation. ~1-2h.

- **No public `ctx.is_trapped(other_handle)` query.** Activity לא יכולה לזהות אם activity אחרת ש-launched להיא קרסה. relevant ל-launcher UI (Stone 11.5d UI נדחה לעת אחרי הבסיס).

- **Activity trait `async fn` הוא קצת שקר ארכיטקטוני** עד שיש executor אמיתי. ה-API חושב כאילו זה async, אבל בפועל הכל single-poll synchronous-with-await-fudge. עם §19.1 השקר נסגר.

### 19.5 🔴 TaskScope cancellation הוא façade (Phase-12 blocker)

**הבעיה.** `Context::activity_scope()` ו-`Context::resumed_scope()` מחזירים `&'static TaskScope`. ה-`spawn(fut)` שלהם הוא ממש [`block_on_ready(fut)`](../sdk-rust/src/activity.rs#L156). אין retention של ה-future בין callbacks, אין cancellation ב-on_pause/on_destroy, אין lifecycle awareness בכלל. ה-name של ה-API מבטיח Android `lifecycleScope` equivalence; ה-body הוא no-op על cancel.

ה-doc-comment בקובץ אומר את זה במפורש (activity.rs:117-127):

> *"In Stone 11.2 every spawned future completes in a single poll ... TaskScope::spawn is just a renamed block_on_ready — runs the future to completion right then."*

**מה זה משבר בפועל.** מפתח שיכתוב:

```rust
async fn on_resume(&mut self, ctx: &mut Context<'_>) {
    ctx.resumed_scope().spawn(async {
        loop {
            sensor.next().await;
            // process
        }
    });
}
```

יקבל infinite loop שגומר את ה-watchdog שלו ב-1.5s ויהרוג את ה-app. לא task שמת ב-on_pause כמו ש-`resumed_scope` מבטיח. ה-API משקר.

**ה-blocker ל-Phase 12.** App suspension (12.4 / CACHED state) דורש sudah scope-aware cancellation. בלי זה, "suspend" יוכל רק לעשות `worker.terminate()` — סמנטית = crash. כל ה-design של LMK priority tiers (12.3) מניח ש-`Resumed`-scoped tasks מתבטלים ב-pause; אם לא, ה-tiers איבדו משמעות.

**ה-blocker ל-Stone 11.7.** ה-docs יתעדו את ה-API. אם ה-API משקר — הdocs לוכדים את השקר.

**התיקון.** הוסף שדה ל-`Slot`: `spawned: Vec<SpawnedTask>` (או slab עם stable IDs). כל `SpawnedTask` מחזיק `Pin<Box<dyn Future + 'static>>` + `kind: ScopeKind`. `TaskScope::spawn` דוחף לרשימה, לא poles. ה-app worker shell drives them frame-by-frame (אותו mechanism כמו pending dispatches). on_pause נכנס → ה-runtime מסיר את כל ה-`ScopeKind::Resumed` בלי re-poll. on_destroy → מסיר את הכולם.

**דרישה משלימה.** cancellation token שאpps יכולים לקרוא — `ctx.is_cancelled() -> bool` או דרך `Poll::Pending` כשyielded after cancellation. אחרת tight loops לא יכולים לבדוק selber.

**Code surfaces.** `sdk-rust/src/runtime.rs` (Slot extension), `sdk-rust/src/activity.rs:140-168` (TaskScope body, `ACTIVITY_SCOPE`/`RESUMED_SCOPE` static migration), `runtime/web/app_worker_shell.js` (re-poll integration). פורמליזציה ב-[`docs/isolation-design.md` §10.5 #11](isolation-design.md). **Stone 11.6.5 owns this.**

### 19.6 🔴 Concurrent host calls per activity blocked (Phase-12 blocker)

**הבעיה.** `Slot.pending: Option<PendingDispatch>` מקצה slot יחיד ל-handle שלם ([sdk-rust/src/executor.rs:38-42](../sdk-rust/src/executor.rs#L38-L42)). שני `storage::read("a")` ו-`storage::read("b")` באותה activity → השני panics או דורס. ה-invariant נכון ל-callback FIFO של ה-Framework (PendingTransition discipline) אבל שגוי ל-user-spawned tasks שלא עוברים דרך PendingTransition.

**מה זה משבר בפועל.** דוגמה שכל מפתח iOS יצפה שתעבוד:

```rust
async fn on_create(&mut self, ctx: &mut Context<'_>) {
    let (notes, settings) = futures::join!(
        storage::read("notes"),
        storage::read("settings"),
    );
    // ...
}
```

ב-iOS זה מקבילות חינם. ב-Akiva היום זה panic — ה-second read קורא ל-`io_start` כש-`Slot.pending` כבר Some. הinvariant doc-comment ב-`executor.rs:31-45` מקובע על ה-טעות.

**ה-blocker ל-Phase 12.** Memory accounting (12.1) מודד baseline של "memory בשימוש כש-app אקטיבי". App עם data כבד מקצה batches מקבילים — אם ה-API מאלץ אותו לסדר אותם, ה-baseline שמודד שגוי, וה-`max_memory_mb` enforcement (12.5) לא יואכף על העומס האמיתי. בנוסף suspension צריך לבטל מספר tasks בו-זמנית; היום אין יותר מ-task אחד ב-flight.

**ה-blocker ל-Stone 11.7.** כל דוגמה ב-docs שתשתמש ב-`join!` או ב-spawn-then-await pattern — תשבר.

**התלות במcient §19.5.** מקבילות host calls מצריכה את TaskScope אמיתי — בלי spawn אמיתי אין דרך ליצור concurrency. אבל TaskScope לבד לא מספיק; ה-Slot צריך להיות מסוגל לפלוט dispatches מקבילים. שניהם נצרכים יחד ב-11.6.5.

**התיקון.** `Slot.pending` הופך ל-`Slab<PendingDispatch>` או `HashMap<u64, PendingDispatch>` עם dispatch_id ייחודי בתוך ה-slot. `akiva_app_poll_dispatch(id)` polls רק את הspeci האחד; `host_op_done` re-polls רק את ה-future ש-parked על ה-host_op הזה. invariant ה-callback FIFO מתחזק; user-spawned tasks יכולים להיות מקבילים.

**Tests חיוניים.**
- ordering: `join!(read("a"), read("b"))` — שני reads רצים מקבילים, completion order עצמאי, ה-tuple נכון.
- mid-flight cancellation: read.await בתוך resumed-scoped task; on_pause → ה-pending dispatch נקרע.
- re-entry: read.await ב-`activity_scope` ב-on_create; on_create returns לפני resolution; ה-await ב-on_resume תופס את הbytes.

**Code surfaces.** `sdk-rust/src/{executor.rs, runtime.rs}`, `runtime/web/app_worker_shell.js::pendingDispatches`, invariant doc-comment update ב-`executor.rs:31-45`. פורמליזציה ב-[`docs/isolation-design.md` §10.5 #12](isolation-design.md). **Stone 11.6.5 owns this.**

### 19.7 🟡 No streams primitive (Phase-13 prep, לא חוסם 12)

**הבעיה.** ה-ABI יודע one-shot futures בלבד (`io_start` → eventual `io_poll` → result, פעם אחת). אין primitive ל-`stream<T>`: sensor readings, audio samples, file watch, animation tickers.

**מה זה משבר.** כל capability שטבעי-stream צריך להיות wrapped כ-polling-as-future, מה שמאבד backpressure ו-ordering guarantees. למשל drag/scroll gesture עם sub-frame ordering היה מסבך את ה-app event loop במקום להיות `stream.next().await` נקי.

**מה זה לא משבר עכשיו.** Phase 12 לא מציג capability חדש שדורש stream.

**ה-blocker ל-Phase 13+.** WindowManager event routing, animation tickers, real-time data — הכל streams. WASI 0.3 (v0.5+ migration) בנוי סביב `stream<T>`. כל יום של דחייה יעלה ב-migration cost.

**התיקון מתוכנן.** op_kind חדש `STREAM_OPEN` שמחזיר `stream_id`; `stream_poll(stream_id)` מחזיר `Vec<Item>` של values שגרהrouped מאז ה-poll הקודם; `stream_close(stream_id)`. wire shape דומה ל-Component Model `stream<T>`.

**המלצה.** דחה ל-Phase 13 prep — ה-problem domain יהיה ברור יותר אז כש-WindowManager צריך event streams. **לא ב-Stone 11.6.5.** מתועד ב-[`docs/isolation-design.md` §10.5 #13](isolation-design.md).

### Status & ordering

ראה [`docs/isolation-design.md` §10.5 Phase 11 Foundation Gaps](isolation-design.md) לתכנית עבודה מלאה. בקצרה:

- **P0** (סגור): ✅ §19.1 (async executor — cardinal), ✅ §19.2 (save_state pipeline), graceful shutdown, Trapped recycle.
- **P1** (חלקי): ✅ magic-numbers config, ✅ drain phase separation. **פתוחים — חבילה ב-11.6.7:** §19.3 (WriteHandle, ~30 דק'), §19.4 (pkgs_wire dedup, ~1-2h).
- **🔴 Stone 11.6.5 — חוסם Phase 12** (3-5 ימים): ✅ §19.5 (TaskScope אמיתי), ✅ §19.6 (concurrent host calls per activity).
- **🔴 Stone 11.6.7 — חוסם 11.7** (3-5 ימים): WriteHandle (§19.3) + pkgs_wire dedup (§19.4) + direct readonly storage handles + lazy-spawn workers + watchdog tunability. ראה [ROADMAP.md Stone 11.6.7](../ROADMAP.md) ל-DoD מלא.
- **P2** (Phase 11.8): e2e tests, worker shell unit tests.
- **Phase 13 prep:** §19.7 (streams).
