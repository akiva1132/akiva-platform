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
