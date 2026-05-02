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

### src/lib.rs

```rust
use akiva_sdk::prelude::*;

#[derive(Default)]
pub struct HelloApp {
    count: i32,
}

#[async_trait(?Send)]
impl Activity for HelloApp {
    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(0xff202030);
        c.draw_text(
            20.0, 40.0, 24.0, 0xffffffff,
            &format!("Clicks: {}", self.count),
        );
        c.fill_round_rect(20.0, 60.0, 100.0, 36.0, 4.0, 0xff3070ff);
        c.draw_text(34.0, 84.0, 16.0, 0xffffffff, "Click me");

        // Pointer state lives on `ctx`; consult it to flip behaviour.
        let _ = ctx; // unused in this minimal sketch
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        if ev.phase == PointerPhase::Down
            && (20.0..120.0).contains(&ev.x) && (60.0..96.0).contains(&ev.y)
        {
            self.count += 1;
        }
    }

    fn on_save_state(&self, state: &mut SavedState) {
        state.put_int("count", self.count as i64);
    }
    fn on_restore_state(&mut self, state: &SavedState) {
        if let Some(v) = state.get_int("count") { self.count = v as i32; }
    }
}

akiva_sdk::akiva_app!(HelloApp);
```

ה-app הזה נכתב imperative — משתמש ב-`Canvas` ישירות. ל-app אמיתי תרצה את `Ui` builder (§9) או AKML layout (§10). שלוש דוגמאות מקיפות יותר לפי השלד של `akiva new --template` ב-§16-§18.

### build

```bash
cd apps/my-app
akiva build        # shortcut ל-cargo build --target wasm32-unknown-unknown + wasm-opt
```

## 5. ה-Macro `akiva_app!`

ה-SDK מספק macro יחיד שמוסיף את כל ה-wasm exports שדרושים ל-App ABI v0:

```rust
akiva_sdk::akiva_app!(HelloApp);
```

מה הוא מייצר:
- `#[no_mangle] extern "C" fn akiva_app_dispatch(...)`, `akiva_app_poll_dispatch(...)`, `akiva_app_get_result(...)` — שלוש exports שמחליפות את ה-zoo של per-callback exports שהיה לפני Stone 11.6.5
- `akiva_app_drive_spawned` — מנגנון re-poll ל-spawned tasks ב-`TaskScope` (נדחה לעת עתה — ראה §19.5)
- allocator hooks ו-instance management (`Default::default()` נקרא פעם אחת על handle הראשון; instance ממוחזר ל-handles הבאים)
- כל המנגנון של executor המיני שמריץ את ה-`async fn` callbacks (`crate::executor`)

הקריאה חייבת להיות **פעם אחת בלבד** ב-`lib.rs` של ה-app. שני calls באותו crate הם linker error כי ה-symbols `#[no_mangle]`.

ה-type שמועבר ל-macro חייב להיות `Default` (ה-runtime יוצר instance אחד per handle בלי להעביר constructor דרך ה-ABI) ו-`Activity` (ראה §6).

`#[akiva::activity]` macro לאפליקציות עם כמה activities עוד לא קיים. לעת עתה — manifest יכול להצהיר על מספר activity records, אבל כולם מצביעים על אותו `Activity` impl. הפרדה אמיתית מתוכננת לאחרי Phase 13.

## 6. ה-`Activity` trait

הצורה הנוכחית (post-Stone 11.2). כל ה-callbacks הם `async fn` חוץ מ-`on_save_state`/`on_restore_state` שמכוונים סנכרוניים — ראה הערה ב-`activity.rs:314-321`.

```rust
#[async_trait(?Send)]
pub trait Activity: 'static {
    // ── Lifecycle (docs/activity-model.md §2) ──────────────────────
    async fn on_create(&mut self, _ctx: &mut Context<'_>) {}
    async fn on_start(&mut self, _ctx: &mut Context<'_>) {}
    async fn on_resume(&mut self, _ctx: &mut Context<'_>) {}
    async fn on_pause(&mut self, _ctx: &mut Context<'_>) {}
    async fn on_stop(&mut self, _ctx: &mut Context<'_>) {}
    async fn on_destroy(&mut self, _ctx: &mut Context<'_>) {}

    // ── Input ───────────────────────────────────────────────────────
    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, _ev: PointerEvent) {}
    async fn on_key(&mut self, _ctx: &mut Context<'_>, _ev: KeyEvent) {}
    async fn on_resize(&mut self, _ctx: &mut Context<'_>, _m: Metrics) {}

    // ── Render ──────────────────────────────────────────────────────
    /// Paint one frame. Called at rAF cadence (typically 60Hz). Must be
    /// fast — every op is serialised immediately.
    async fn on_render(&mut self, _ctx: &mut Context<'_>, _c: &mut Canvas<'_>) {}

    // ── Intent redelivery (Stone 9.4) + activity-result (9.5) ───────
    async fn on_new_intent(&mut self, _ctx: &mut Context<'_>, _intent: &Intent) {}
    async fn on_activity_result(
        &mut self,
        _ctx: &mut Context<'_>,
        _request_code: u32,
        _result: &ActivityResult,
    ) {}

    // ── Saved state (sync on purpose) ───────────────────────────────
    fn on_save_state(&self, _state: &mut SavedState) {}
    fn on_restore_state(&mut self, _saved: &SavedState) {}
}
```

**מינימום מימוש.** רק `on_render` מתחייב בפועל (המסך תמיד מצריך פאינט); כל השאר defaults.

**מה השתנה מ-Stone 11.2.** לפני 11.2 ה-trait היה סנכרוני; ה-`async fn` הוצג עם הרצת `block_on_ready` בלולאה אחת לכל callback. החל מ-Stone 11.6.5 ה-runtime מריץ executor אמיתי (ראה §19.1) שיודע לעצור על `Pending` ולחזור ב-frame הבא. אין שינוי ב-trait shape — אפליקציות שעבדו עם `block_on_ready` עובדות גם ב-executor האמיתי.

**`Send` bound.** ה-`async_trait(?Send)` כי wasm worker הוא single-threaded; אין יתרון להוסיף `Send`/`Sync` requirements שיגרמו ל-`Rc`/`RefCell` interior של ה-app להיתקע.

**Saved state — למה sync.** `on_save_state` נקרא מ-destroy path סינכרוני (`framework::wasm_activity::dispatch_save_state_async`). הפיכתו ל-async הייתה כופה future allocation per save בלי benefit; ה-bytes יוצאים מתוך ה-app instance, לא קוראים מ-host.

## 7. `Context`

ה-handle שמועבר לכל async callback. שדה ציבורי `metrics: Metrics` חושף את ה-surface (width/height/density) ו-`timestamp_ms: f64` הוא frame-stable monotonic timestamp. ה-methods מבצעים host calls שיכולים `Pending` — חתום async כי הם משתעבדים ב-frame הבא.

```rust
pub struct Context<'a> {
    pub metrics: Metrics,           // width_px, height_px, density
    pub timestamp_ms: f64,          // frame timestamp (monotonic)
    // … internals (host handle, pending signal slots) …
}

impl<'a> Context<'a> {
    // Navigation (async — kernel resolves the dispatch)
    pub async fn start_activity(&mut self, intent: &Intent);
    pub async fn start_activity_for_result(
        &mut self, intent: &Intent, request_code: u32,
    );
    pub async fn finish(&mut self);
    pub async fn set_result(&mut self, result: &ActivityResult);

    // Spawned tasks (Stone 11.6.5; cancellation: ראה §19.5)
    pub fn activity_scope(&self) -> TaskScope; // lives until on_destroy
    pub fn resumed_scope(&self) -> TaskScope;  // cancelled at on_pause
}
```

**שינויים מ-Stone 11.0 baseline.** `set_content_view`, `find_view`, `inflate`, ו-`show_toast` הוסרו — ה-`Canvas` הישיר (§8) ו-`Ui` builder (§9) החליפו את ה-AKML view-handle model. AKML עוד נתמכת (§10) אבל דרך `inflate_str_with` כל frame, לא דרך handles persistent.

**`set_content_view` בעבר.** היה דרוש כדי לקשור activity ל-AKML root. עכשיו `on_render` הוא ה-entry point ל-paint; כל frame ה-app בונה את ה-tree ב-`Canvas` או דרך `Ui`/AKML inflate. אין שלב "set the layout" נפרד.

## 8. `Canvas`

Immediate-mode painter. ה-API מחויב לרוץ ב-`on_render`; קריאה מחוץ ל-render context לא תפיק ops לא ברורים אבל אין הגנת compile-time. כל הצבעים הם `u32` ב-`0xAARRGGBB` (alpha first).

```rust
impl<'a> Canvas<'a> {
    pub fn clear(&mut self, rgba: u32);
    pub fn fill_rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgba: u32);
    pub fn fill_round_rect(
        &mut self, x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32,
    );
    pub fn draw_text(
        &mut self, x: f32, y: f32, size: f32, rgba: u32, text: &str,
    );

    // Clipping + transforms (push/pop pairs)
    pub fn push_clip(&mut self, x: f32, y: f32, w: f32, h: f32);
    pub fn pop_clip(&mut self);
    pub fn push_transform(&mut self, dx: f32, dy: f32, scale: f32);
    pub fn pop_transform(&mut self);
}
```

`Canvas` עצמו לא מציג כלום — הוא serialiser. ה-ops זורמים ל-host דרך wire encoding ועוברים ל-Canvas2D / WebGL2 שם. זה אומר ש-paint code חייב להיות זריז (≤ 16ms/frame כולל logic) אבל לא משחקי GPU.

## 9. `Ui` builder

Builder layer מעל `Canvas` שמטפל ב-layout (VStack/HStack/grid), pointer hit-testing, ו-widget state. שמירת state בין frames — ב-`UiState` שה-app מחזיק כ-field.

```rust
impl<'s, 'c, 'b> Ui<'s, 'c, 'b> {
    pub fn new(state: &'s mut UiState, ctx: &'c mut Context<'_>, c: &'b mut Canvas<'_>) -> Self;

    // Leaf widgets
    pub fn text(&mut self, text: &str) -> TextBuilder;       // .size(), .color(), .build()
    pub fn button(&mut self, action: &str, label: &str) -> NodeId;

    // Containers
    pub fn v_stack(&mut self, children: &[NodeId]) -> StackBuilder;
    pub fn h_stack(&mut self, children: &[NodeId]) -> StackBuilder;

    // Commit + dispatch
    pub fn present(&mut self, root: NodeId);
    pub fn was_clicked(&self, action: &str) -> bool;
    pub fn was_triggered(&self, action: &str) -> bool;  // alias used in compose-style
}
```

`feed_pointer` ב-`UiState` נקרא מ-`on_pointer` כדי להזין את ה-builder ב-coords של ה-event האחרון. הקריאה היא:

```rust
async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
    self.ui.feed_pointer(ev);
}
```

ראה §16 לדוגמה מלאה (template `default`).

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

API async (Stone 11.6.7 #1). `read` מחזיר `Option<ReadFuture>` — `None` אם ה-app אסר על עצמו storage capability ב-manifest (no `STORAGE_SANDBOX` grant); אחרת `Future` שיתחזיר `Result<Option<Vec<u8>>, StorageError>`. `write` מחזיר `WriteHandle` שאם תזרק (`drop` בלי `await`) ה-write נשאר fire-and-forget — אבל `#[must_use]` יכריז warning ב-`storage::write(k, v);` בלי `let _ =`.

```rust
use akiva_sdk::storage;

async fn on_create(&mut self, _ctx: &mut Context<'_>) {
    if let Some(fut) = storage::read("notes") {
        if let Ok(Some(bytes)) = fut.await {
            self.text = String::from_utf8_lossy(&bytes).into_owned();
        }
    }
}

async fn save(&self) {
    // Awaiting the WriteHandle gets durability confirmation:
    let _ = storage::write("notes", self.text.as_bytes()).await;
}

fn fire_and_forget_write(&self, value: &[u8]) {
    // `let _ =` silences the must_use; the host_op is still sent.
    let _ = storage::write("ephemeral", value);
}
```

`storage::remove(key)` קיים סנכרוני (אין durability ack). אין `keys()` — Stone 11.6.7 הסיר את זה (כל use case שראינו אפשר להחליף ב-cache פנימי או manifest declaration).

## 13. Logging

Macros מ-`prelude` (לא module path):

```rust
use akiva_sdk::prelude::*;

log_debug!("Counter is {}", app.count);
log_info!("App started");
log_warn!("Unusual value: {:?}", value);
log_error!("Failed to parse: {:?}", err);
log_trace!("very fine-grained: x={x:.4}, y={y:.4}");
```

ה-macros מקודדים את ה-message + level דרך `akiva_host_log`. ה-host מסנן על `package_id` + level (debug על debug builds; info+ על release). Stone 11.6.7 הוסיף שלb mode שמעביר log emission ב-batch אחת ל-frame כדי לחסוך postMessage overhead — שקוף ל-app.

## 14. Build & Run

```bash
# Create new app — defaults to `default` template (§16). Pass --template
# compose-style (§17) for AKML+Bindings, --template game (§18) for a
# per-frame physics scaffold.
akiva new my-app
akiva new mynotes --template compose-style
akiva new pong --template game

# Build wasm
cd my-app
akiva build

# Run (dev server + auto-install)
akiva run

# Open the SDK docs in a browser
akiva docs
```

## 15. שלוש דוגמאות מקיפות — אחת לכל template

ה-`akiva new` CLI מספק 3 templates. כל אחת מציגה דרך עבודה שונה:

| template | UI shape | מתי | §  |
|---|---|---|---|
| `default` | imperative `Ui` builder | מתחילים פשוטים, hot-loops state | §16 |
| `compose-style` | declarative AKML + Bindings | UI טבלאי, theming, RTL | §17 |
| `game` | direct `Canvas` ב-`on_render`, dt-driven | משחקים, animations, simulations | §18 |

כל הדוגמאות עובדות עם אותו `Activity` trait — הבחירה היא של ה-app author איך לארגן את ה-render. ניתן גם לערבב (game-style canvas + Ui overlay).

## 16. דוגמה — `default` template (counter)

```rust
use akiva_sdk::prelude::*;

#[derive(Default)]
pub struct Counter {
    ui: UiState,
    count: i32,
}

#[async_trait(?Send)]
impl Activity for Counter {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        log_info!("Counter created ({}x{} @ {:.1}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density);
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        self.ui.feed_pointer(ev);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(0xff0f1420);
        let mut ui = Ui::new(&mut self.ui, ctx, c);

        let title = ui.text("Counter").size(28.0).color(0xfff0f4fa).build();
        let counter = ui.text(&format!("count = {}", self.count))
            .size(20.0).color(0xffe0e6f0).build();
        let inc = ui.button("inc", "+1");

        let root = ui.v_stack(&[title, counter, inc])
            .gap(16.0).padding(24.0).build();
        ui.present(root);

        if ui.was_clicked("inc") { self.count += 1; }
    }

    fn on_save_state(&self, state: &mut SavedState) {
        state.put_int("count", self.count as i64);
    }
    fn on_restore_state(&mut self, state: &SavedState) {
        if let Some(v) = state.get_int("count") { self.count = v as i32; }
    }
}

akiva_sdk::akiva_app!(Counter);
```

**Notes.** `UiState` שומר על widget state בין frames (focused button, click-in-progress); `feed_pointer` מזין את ה-state ב-coords של ה-event האחרון. ה-`was_clicked("inc")` נבדק רק אחרי `present()` — ה-builder יודע מי נלחץ אחרי ש-layout נסגר. אין ID stability requirements: ה-string `"inc"` הוא ה-action key, hash-able.

## 17. דוגמה — `compose-style` template (notes-like)

ה-UI מוגדר ב-`res/layout/main.akml`:

```xml
<?akml version="1.0"?>
<VStack padding="24" gap="16" cross="stretch">
    <Text text="@string/app_name" size="28" color="@color/title_fg" />
    <Text text="{greeting}, world!" size="36" color="@color/greeting_fg" />
    <Text text="Clicks: {count}" size="18" color="@color/body_fg" />
    <HStack gap="12">
        <Button id="greet" label="@string/cta_greet" onClick="greet" flex="1" />
        <Button id="reset" label="@string/cta_reset" onClick="reset" />
    </HStack>
</VStack>
```

```rust
use akiva_sdk::prelude::*;
use akiva_sdk::akml::{self, Bindings};
use akiva_sdk::res;

const LAYOUT: &str = include_str!("../res/layout/main.akml");
const GREETINGS: &[&str] = &["Hello", "Hola", "Bonjour", "Shalom"];

#[derive(Default)]
pub struct Greeter {
    ui: UiState,
    greeting_idx: i32,
    count: i32,
}

#[async_trait(?Send)]
impl Activity for Greeter {
    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        self.ui.feed_pointer(ev);
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(res::color("bg_window"));

        let greeting = GREETINGS[(self.greeting_idx as usize) % GREETINGS.len()];
        let bindings = Bindings::new()
            .text("greeting", greeting.to_string())
            .text("count", format!("{}", self.count));

        let mut ui = Ui::new(&mut self.ui, ctx, c);
        let root = akml::inflate_str_with(&mut ui, LAYOUT, &bindings)
            .expect("layout is statically valid — inflate failure is a build bug");
        ui.present(root);

        if ui.was_triggered("greet") {
            self.greeting_idx = (self.greeting_idx + 1) % (GREETINGS.len() as i32);
            self.count += 1;
        }
        if ui.was_triggered("reset") {
            self.greeting_idx = 0;
            self.count = 0;
        }
    }

    fn on_save_state(&self, state: &mut SavedState) {
        state.put_int("greeting_idx", self.greeting_idx as i64);
        state.put_int("count", self.count as i64);
    }
    fn on_restore_state(&mut self, state: &SavedState) {
        if let Some(v) = state.get_int("greeting_idx") { self.greeting_idx = v as i32; }
        if let Some(v) = state.get_int("count") { self.count = v as i32; }
    }
}

akiva_sdk::akiva_app!(Greeter);
```

**Notes.** `Bindings` ממלא `{placeholder}` tokens ב-AKML עם strings runtime. `@string/...` ו-`@color/...` נפתרים מ-`res/values/`; `res/values-night/` נטען אוטומטית כשה-OS מדווח על dark mode. ה-`onClick="greet"` ב-AKML מזוהה דרך `ui.was_triggered("greet")` — ה-string הוא הקשר היציב.

ה-AKML inflate מתבצע **כל frame** מחדש. ה-cost ידוע (פרסור ~5µs לפי `bench_akml_parse`); ה-tradeoff מועדף על pretty-printing tree management ב-Rust.

## 18. דוגמה — `game` template (ball physics)

`on_render` רץ ב-rAF cadence; משתמשים ב-`ctx.timestamp_ms` כ-monotonic time source כדי לחשב dt. הדוגמה: כדור שקופץ מהקירות, tap לבעוט.

```rust
use akiva_sdk::prelude::*;

const BALL_R: f32     = 24.0;
const INIT_SPEED: f32 = 280.0;
const MAX_DT_MS: f32  = 100.0;
const BG_COLOR: u32   = 0xff0a0e1a;
const BALL_COLOR: u32 = 0xfff97316;

pub struct Ball {
    cx: f32, cy: f32,
    vx: f32, vy: f32,
    last_ts: Option<f64>,
    bounds: (f32, f32),
}

impl Default for Ball {
    fn default() -> Self {
        Self {
            cx: 0.0, cy: 0.0, vx: INIT_SPEED, vy: INIT_SPEED * 0.85,
            last_ts: None, bounds: (1.0, 1.0),
        }
    }
}

#[async_trait(?Send)]
impl Activity for Ball {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        let w = ctx.metrics.width_px as f32;
        let h = ctx.metrics.height_px as f32;
        if self.cx == 0.0 && self.cy == 0.0 {
            self.cx = w * 0.5;
            self.cy = h * 0.5;
        }
        self.bounds = (w, h);
    }

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {
        if ev.phase == PointerPhase::Down {
            self.cx = ev.x; self.cy = ev.y;
            let (w, h) = self.bounds;
            let dx = ev.x - w * 0.5;
            let dy = ev.y - h * 0.5;
            let len = (dx*dx + dy*dy).sqrt().max(1.0);
            self.vx = INIT_SPEED * (dx / len);
            self.vy = INIT_SPEED * (dy / len);
        }
    }

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        let w = ctx.metrics.width_px as f32;
        let h = ctx.metrics.height_px as f32;
        self.bounds = (w, h);

        // dt in seconds, clamped — first frame OR resume after suspend
        // both have stale `last_ts`; clamp prevents tunneling.
        let dt = match self.last_ts {
            Some(prev) => ((ctx.timestamp_ms - prev) as f32 / 1000.0)
                .clamp(0.0, MAX_DT_MS / 1000.0),
            None => 0.0,
        };
        self.last_ts = Some(ctx.timestamp_ms);

        self.cx += self.vx * dt;
        self.cy += self.vy * dt;

        if self.cx - BALL_R < 0.0 { self.cx = BALL_R;       self.vx = self.vx.abs(); }
        if self.cx + BALL_R > w   { self.cx = w - BALL_R;   self.vx = -self.vx.abs(); }
        if self.cy - BALL_R < 0.0 { self.cy = BALL_R;       self.vy = self.vy.abs(); }
        if self.cy + BALL_R > h   { self.cy = h - BALL_R;   self.vy = -self.vy.abs(); }

        c.clear(BG_COLOR);
        c.fill_round_rect(
            self.cx - BALL_R, self.cy - BALL_R,
            BALL_R * 2.0, BALL_R * 2.0,
            BALL_R, BALL_COLOR,
        );
    }

    fn on_save_state(&self, state: &mut SavedState) {
        state.put_int("cx", self.cx.to_bits() as i64);
        state.put_int("cy", self.cy.to_bits() as i64);
        state.put_int("vx", self.vx.to_bits() as i64);
        state.put_int("vy", self.vy.to_bits() as i64);
    }
    fn on_restore_state(&mut self, state: &SavedState) {
        if let Some(v) = state.get_int("cx") { self.cx = f32::from_bits(v as u32); }
        if let Some(v) = state.get_int("cy") { self.cy = f32::from_bits(v as u32); }
        if let Some(v) = state.get_int("vx") { self.vx = f32::from_bits(v as u32); }
        if let Some(v) = state.get_int("vy") { self.vy = f32::from_bits(v as u32); }
    }
}

akiva_sdk::akiva_app!(Ball);
```

**Notes.** ה-`f32::to_bits` ↔ `from_bits` round-trip שומר על ה-velocity וה-position דרך SavedState (ש-tags הם integer-only ב-wire format). ה-`MAX_DT_MS` clamp הוא הbarrier להחזרה אחרי tab-switch ארוך — בלי זה frame ראשון אחרי resume היה מקפיץ את הכדור על פני המסך כולו.

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

### 19.4 ✅ `pkgs_wire` decoder dedup — fixed (Stone 11.6.7 #2, May 2026)

**מה היה הבעיה.** ה-decoder היה משוכפל 3 פעמים: [`framework/src/wire/pkgs.rs`](../framework/src/wire/pkgs.rs), [`sdk-rust/src/pkgs.rs`](../sdk-rust/src/pkgs.rs), ו-`runtime/web/app_registry.js::_warmupWorkers`. drift risk כשמוסיפים שדה.

**התיקון.** crate חדש בworkspace `akiva-wire` (ב-`wire/src/pkgs.rs`) הוא ה-canonical source. שני ה-Rust crates שעובדים עם הwire format (framework + sdk-rust) מייבאים משם דרך `workspace.dependencies.akiva-wire`; הקבצים הקיימים שלהם הופכים thin re-exports. הJS side לא מסוגל לייבא Rust crate, אבל `_warmupWorkers` נושא עכשיו `// SCHEMA:` comment שמצביע על `wire/src/pkgs.rs` — drift נתפס בcode review כשהlayout משתנה.

תוצאת המעבר: `PackageEntry.entry_class` ב-SDK שונה ל-`PackageEntry.class` (consistency עם wire crate; apps השתמשו רק ב-`id` ו-`name` ולכן אין migration). 8 wire tests + 2 wrapper tests + cross-crate parity מובטח by-construction (שני הצרכנים ה-Rust עוברים דרך אותה פונקציה).

### 19.4a 🟢 Other minor SDK gaps

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
