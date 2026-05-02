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

### 19.1 🔴 The async executor gap (ה-cardinal issue)

**הבעיה.** ה-`Activity` trait הוא `async fn`:

```rust
async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
    let bytes = storage::read("notes").await;  // ❌ panics
    ...
}
```

אבל ה-executor של ה-SDK הוא [`block_on_ready`](../sdk-rust/src/activity.rs) שעושה `.poll()` פעם אחת בלבד:

```rust
pub fn block_on_ready<F: Future>(mut fut: F) -> F::Output {
    match Future::poll(unsafe { Pin::new_unchecked(&mut fut) }, &mut cx) {
        Poll::Ready(v) => v,
        Poll::Pending  => panic!("block_on_ready: future returned Pending"),
    }
}
```

**מה זה אומר בפועל:**

| Operation | Status |
|---|---|
| ✅ `async fn` בלי await על futures לא-ready | works |
| ✅ `await` על future שresolve immediately | works (e.g., a future that returns `Poll::Ready` on first poll) |
| ❌ `await` על cross-worker host call | panics (release: silent fall-through) |
| ❌ `await` על channel that hasn't received yet | panics |
| ❌ `select!`, `join!` עם branch שעוד לא ready | panics |

**למה זה כך נשאר.** הסיבה ההיסטורית — Stone 11.2 הציג את ה-async-shape API כי ה-vision היה Android/iOS parity. הexecutor האמיתי (embassy) נדחה ל-Stone 11.3 וב-11.4 לתת-stones של isolation. בפועל ה-block_on_ready stub שרד — single-worker עבד עם זה. עם cross-worker (11.4c) ה-stub שבר.

**ה-workaround היום: deferred-handle pattern.** ראה [`storage::ReadHandle`](../sdk-rust/src/storage.rs):

```rust
struct Notes {
    text: String,
    loading: Option<storage::ReadHandle>,
}

impl Activity for Notes {
    async fn on_create(&mut self, _ctx: &mut Context<'_>) {
        self.loading = storage::read("notes");  // returns ReadHandle, doesn't await
    }
    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        if let Some(handle) = &self.loading {
            if let Some(result) = handle.poll() {        // <-- manual polling
                if let Ok(Some(bytes)) = result {
                    self.text = String::from_utf8(bytes).unwrap_or_default();
                }
                self.loading = None;
            }
        }
        // ... render ...
    }
}
```

זה עובד אבל מאלץ apps לנהל state ידנית בין frames. ככל שה-SDK מתרחב (network, BLE, camera), הדפוס הזה יתפוצץ.

**התיקון המלא.** embassy executor (`embassy-executor` + `embassy-sync`):
- replace `block_on_ready` ב-real executor שmamper Pending ע"י לחזור ל-host
- שמירת ה-future state בין dispatches (ה-`akiva_app_dispatch` protocol יצטרך להחזיר future_id mid-callback)
- waker registry ש-`host_op_done` מעיר
- `let bytes = storage::read("k").await` יעבוד fluent

**סקופ:** 1-2 ימים. שינויים:
- `sdk-rust/Cargo.toml` — `embassy-executor`, `embassy-sync` deps
- `sdk-rust/src/activity.rs` — replace `block_on_ready`
- `sdk-rust/src/runtime.rs` — dispatch glue ש-supports resumption
- `runtime/web/app_worker_shell.js` — handle resumption protocol
- `framework/src/wasm_activity.rs` — pipelined dispatch הופך גם לlifecycle calls (כמו render כבר היום)

**תיקון ביניים (אופציונלי).** mini-executor שmamper רק את ה-`host_io_poll` case — הרבה פחות גנרי אבל יעיל יותר ל-MVP. נבחר רק אם ה-embassy bundle bloat (~10KB per app) הופך לבעיה אמיתית.

### 19.2 🟡 `save_state` degraded under cross-worker

**הבעיה.** ה-`Activity::on_save_state` callback הוא חלק מchain pause→stop. ב-Stone 6 design, ה-bytes נכתבים ל-storage לpersistence.

ב-Stone 11.4c (worker-per-app), `WasmActivity::save_state_bytes` עושה:
```rust
let future_id = self.host.save_state(self.handle);
match self.host.poll_future(future_id) {
    Some(Ok(bytes)) => bytes,
    other => { debug_assert!(false, ...); Vec::new() }  // ← release: silent
}
```

עם cross-worker, ה-poll הראשון תמיד `None` (Pending) → release fall-through → empty Vec → app loses state.

**מה זה משבר בפועל:**
- App יוצא מ-foreground (Pause) — saved state ריק
- App חוזר ל-foreground — `on_restore_state` מקבל empty bytes → app starts from default state
- **Phase 6 saved-state is broken in production** under 11.4c.

**התיקון.** pipeline בstate משתנה — ראה הדפוס שכבר הוצאנו ל-render:
- `WasmActivity` שדה חדש `pending_save: Option<u64>`
- בpause — dispatch save_state, store fid כ-pending
- בdestroy — drain prior pending_save (poll). אם READY → use bytes. אם PENDING → wait one frame OR timeout.
- ה-actual write לstorage קורה אחרי ה-drain.

**הexpected complications:** ה-pause→stop chain עובר על הרבה frames מאחר ש-save_state חייב לresolve לפני stop יכול לרוץ. זה יוצר deferred lifecycle path עבור pause-only — דומה ל-render אבל לכל ה-pause/stop edge.

### 19.3 🟡 Multi-frame side-effects (`storage::write` ו-`remove`)

**הבעיה.** היום:
```rust
pub fn write(key: &str, value: &[u8]) {
    let _ = io_start(io::STORAGE_SET, &args);  // fire-and-forget
}
```

ה-app אינו מקבל indication אם ה-write נכשל (quota, IO error, denied). ה-host logs את ה-failure אבל ה-app רץ לכאורה successfully.

**מה זה משבר בפועל.** apps שדורשים durability (settings, notes, drafts) לא יכולים לדעת אם ה-write בוצע. ב-OS אמיתי (iOS, Android) ה-API מחזיר Result.

**התיקון (קל יחסית).** `WriteHandle` parallel ל-`ReadHandle`:
```rust
pub fn write(key: &str, value: &[u8]) -> Option<WriteHandle>;
impl WriteHandle {
    pub fn poll(&self) -> Option<Result<(), StorageError>>;
}
```

המעבר ל-async-await (§19.1) ירכז את כל ה-handles הללו ל-`.await` נורמלי.

### 19.4 🟢 Other minor SDK gaps

- **`pkgs_wire` decoder duplicated 3x.** [`framework/src/wire/pkgs.rs`](../framework/src/wire/pkgs.rs), [`sdk-rust/src/pkgs.rs`](../sdk-rust/src/pkgs.rs), ו-`runtime/web/app_registry.js::_warmupWorkers` משתפים את אותו פורמט. drift risk כשmosbim שדה. הפתרון או JSON (לpaths שאינם performance-critical), או schema-driven generation. ~1-2h.

- **No public `ctx.is_trapped(other_handle)` query.** Activity לא יכולה לזהות אם activity אחרת ש-launched להיא קרסה. relevant ל-launcher UI (Stone 11.5d UI נדחה לעת אחרי הבסיס).

- **Activity trait `async fn` הוא קצת שקר ארכיטקטוני** עד שיש executor אמיתי. ה-API חושב כאילו זה async, אבל בפועל הכל single-poll synchronous-with-await-fudge. עם §19.1 השקר נסגר.

### Status & ordering

ראה [`docs/isolation-design.md` §10.5 Phase 11 Foundation Gaps](isolation-design.md) לתכנית עבודה מלאה. בקצרה:

- **P0** (חייבים): §19.2 (save_state pipeline), graceful shutdown, Trapped recycle, **§19.1 (async executor) — cardinal**
- **P1** (כדאי): §19.3 (WriteHandle), magic-numbers config, drain phase separation, §19.4 (pkgs_wire dedup)
- **P2** (future): e2e tests, worker shell unit tests
