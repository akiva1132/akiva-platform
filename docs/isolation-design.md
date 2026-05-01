# Phase 11 — Process / Fault Isolation (Design v2)

> **Design v2** — שכתוב מקיף של v1 בעקבות עדכון החזון: **חוויית מפתח ברמת Android/iOS, תחושת ריצה כמה שיותר native, ובניית תשתיות יסודיות לטווח הארוך.** המסמך הזה הוא הסמך המנחה לכל Phase 11 (≈12-20 שבועות).

מסמך התכנון של עמוד 1 ב-v0.3. כל החלטה שלא תיסגר כאן — תילקח בחיפזון בזמן הקוד.

המסמך לא מתאר את המצב הסופי באופן מלא. הוא מתאר את **המודל**, את ה-**trade-offs שנבחרו על בסיס מחקר**, ואת ה-**מסלול** — לא כל פרט קוד. פרטי קוד נכתבים ב-stones.

---

## 1. החזון (priorities לפי סדר)

1. **DX ברמת Android/iOS.** מפתחי app כותבים `async fn on_create(&mut self) { ... }`, מקבלים lifecycle scopes שמבטלים tasks אוטומטית, ו-API שמרגיש מוכר. זמן ramp-up: דקות, לא שעות.
2. **native-feel ריצה** בתוך מגבלות הדפדפן. 60fps יציב, lifecycle transitions חלקים, host calls < 1ms בwarm path. אנחנו מקבלים שזה דפדפן — לא ננסה להגיע ל-100,000 syscall/s כמו Linux.
3. **תשתית יסודית** — שכבות שמ-100 אפליקציות עתידיות יוכלו להישען עליהן. Documentation מקיף, primitives שמרכיבים יפה, error messages מעולים.

הבר: כל מפתח חדש שמתחיל כותב app שעובד בתוך 5 דקות מ-`akiva new`.

---

## 2. למה Phase 11 קיים — תזכורת קצרה

כל אפליקציה ב-Akiva רצה היום ב-**אותו Web Worker**, על אותו event loop. אפליקציה שתקועה / מתפוצצת / מקצה memory בלי גבול — מפילה את כל המערכת.

שלושה תרחישי כשל:
- **infinite loop** ב-`on_pointer` → ה-Worker חסום, ה-Shell קופאת.
- **panic** ב-`on_render` → ה-`ActivityRecord` נשאר ב-Resumed, framework מבולבל.
- **memory leak** → ה-tab כולו נהרג, אין attribution לאפליקציה.

ב-OS אמיתי כל אפליקציה ב-process משלה: ניתן להרוג, למדוד, להחליט מי הכי פחות חשוב. הדפדפן לא נותן processes ישירות, אבל **Web Workers** הם thread-isolation ברמה דומה לprocess-isolation מבחינת user-space.

המטרה של Phase 11 בשורה אחת: **כל אפליקציה ב-Worker משלה, ה-kernel ב-Worker הראשי, כשל באפליקציה לא מפיל את המערכת.** אבל בנוסף — ה-API שמפתחים יראו חייב להיות async, מודרני, ו-foundational ל-100 האפליקציות הבאות.

---

## 3. ממצאי מחקר (May 2026)

לפני ההחלטות, הממצאים שמדריכים אותן.

### 3.1 WASI 0.3 / Component Model — **לא מוכן עדיין**

WASI 0.3 (native async, `future<T>`, `stream<T>`) שוחרר preview ב-Wasmtime 37 (פברואר 2026). WASI 1.0 stable מתוכנן ל-late 2026 / early 2027. **דפדפנים לא תומכים ב-Component Model native** — נדרש transpilation דרך `jco`. `wit-bindgen` Rust async API עדיין משתנה (לדוגמה issue 1152 מינואר 2025 על mapping של `stream<T>`).

**מסקנה:** Component Model הוא ה-future, אבל אנחנו לא ב-future. לאמץ אותו עכשיו = לחיות עם spec churn ל-12-18 חודשים. **נחליט: roll-our-own ABI ל-Phase 11, planned migration ל-Component Model ב-v0.5+.** ה-design של ה-ABI שלנו ייקח השראה מ-Component Model semantics (futures, streams) כך ש-migration יהיה straight-forward.

### 3.2 OPFS — multiple readonly handles **כן** נתמכים

הממצא שמשנה את ה-design: ב-Chrome 121+ (ובFirefox/Safari עוקבים), `FileSystemSyncAccessHandle` תומך ב-3 modes:
- **`readwrite`** (default) — exclusive lock. Handle יחיד.
- **`read-only`** — multiple handles allowed simultaneously.
- **`readwrite-unsafe`** (Chrome 121+) — multiple handles, caller manages locking.

**מסקנה:** apps יכולות לקבל readonly handles ישירים לקבציהן (קריאה מהירה, ב-app-worker עצמו, אין round-trip). writes ממשיכים דרך kernel-worker (sandbox enforcement, atomic).

### 3.3 Document Isolation Policy (Chrome 137+) — **COI הפך פשוט**

Chrome 137 (יצא ~אמצע 2025, עכשיו ב-production) הוסיף `Document-Isolation: ...` header שנותן `crossOriginIsolated === true` ב-per-frame בלי לדרוש COEP-cascade על subframes. SharedArrayBuffer זמין ללא ה-headache של COEP.

**מסקנה:** SAB ו-Atomics זמינים בקלות. אנחנו לא תקועים עם COEP גרורים בכל iframe.

### 3.4 postMessage performance (2026) — **מהיר מספיק**

מדידות מודרניות:
- Simple message: 0-1ms round-trip
- Transferables: 80kB/ms throughput
- structuredClone: 5-40% מהיר יותר מ-2024 (אופטימיזציות ב-V8/SpiderMonkey)

**מסקנה:** postMessage לא bottleneck. SAB+Atomics רק כשבאמת צריך blocking (מקרים בודדים — synchronous storage reads מתוך wasm function שלא יכולה להחזיר Future).

### 3.5 Worker spawn cost — **~40ms**

יציב מאז 2015. אומר: לא לעשות spawn-per-activity-launch. אסטרטגיה: lazy-spawn-per-package, או pre-spawn במקרים קריטיים (Shell).

### 3.6 Modern app frameworks — **scope-based async cancellation**

הממצאים החשובים ביותר ל-DX:

**Android (Kotlin Coroutines + Compose):**
- `lifecycleScope.launch { ... }` — coroutine שמבוטל אוטומטית כש-Activity נהרסת.
- `viewModelScope.launch { ... }` — coroutine שמבוטל כש-ViewModel נהרסת.
- `LaunchedEffect(key) { ... }` — coroutine שמבוטל אם ה-key משתנה או ה-Composable יוצא.
- `repeatOnLifecycle(STARTED) { flow.collect { ... } }` — collection רק כש-UI נראה.

**SwiftUI:**
- `.task { await load() }` — task שmotor automatically מתבטל ב-`onDisappear`.
- `.task(id: someValue) { ... }` — task שמתבטל אם `someValue` משתנה.
- structured concurrency: child tasks מתבטלים עם parent.

**הדפוס המשותף:** **המפתח לא מנהל cancellation ידנית.** ה-API נותן scope, ה-scope מבטל אוטומטית. אין `if (mounted)` checks, אין memory leak מ-orphaned listeners.

**מסקנה:** SDK-rust חייב לחשוף scope-based primitives:
- `&mut Context` חושף `task!` macro: `task!(self, async { ... })` — task scoped ל-activity, מבוטל ב-`on_destroy`.
- `task_until!(self, state == Resumed, async { ... })` — task שמבוטל ביציאה מ-Resumed.
- כל ה-async I/O בSDK מ honored cancellation tokens.

### 3.7 embassy executor — **מתאים ל-Akiva**

Embassy הוא executor async/await ל-no_std embedded. תומך ב-wasm. yes alloc, yes nightly, אבל ב-stable עם feature flags. גודל: 5-10KB code per app, scales to 1000s of tasks. **זה ה-runtime שאפליקציות Akiva ירוצו עליו** — מקביל ל-Tokio, אבל מתאים לסביבת no_std-friendly של app וסם.

### 3.8 cross-origin iframe-per-app — **דחוי**

לקבל "Spectre-grade" isolation, צריך כל app על registered domain שונה (לא subdomain — Chrome מתעלם מ-subdomains). זה דורש DNS infrastructure שלא רלוונטית ל-MVP. Worker isolation מספקת thread+heap isolation מלא, וזה sufficient עד שיש exposure לdata-leak attacks (לא רלוונטי לפלטפורמה שמריצה apps של המשתמש בלבד).

**מסקנה:** worker-per-app עכשיו, document/iframe-per-app נשאר future-work אם יום אחד נריץ untrusted third-party apps.

### 3.9 WebContainers (StackBlitz) כ-precedent

WebContainers מריצים Node.js שלם בdoubrar בעזרת SAB+SW+wasm. זה מוכיח שאפשר להריץ OS-grade workloads במודל הזה. ה-architecture שלהם דומה למה שאנחנו בונים: wasm + linear memory isolation + virtual networking. **אנחנו לא לבד.**

---

## 4. ההחלטות הקריטיות

### 4.1 ה-ABI יהיה **async-native**, לא sync facade

**הבחירה:** ה-ABI של app↔host יהיה async ברמה הסמנטית. host calls לא חוזרים בעם ערך מיידית — הם מחזירים `Future`. ה-app יריץ executor (embassy) שמטפל ב-await.

```rust
// SDK לאפליקציה — חתימה חדשה
pub trait Activity {
    async fn on_create(&mut self, ctx: &mut Context<'_>);
    async fn on_resume(&mut self, ctx: &mut Context<'_>);
    async fn on_pause(&mut self, ctx: &mut Context<'_>);
    async fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent);
    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>);
    // ...
}

// host calls
impl Context<'_> {
    pub async fn start_activity(&mut self, intent: &Intent);
    pub async fn read_storage(&self, key: &str) -> Option<Vec<u8>>;
    pub async fn list_packages(&self) -> Vec<PackageInfo>;
}
```

**הבחירה הזאת מנצחת את אופציה B של v1** מהסיבות:

1. **DX זהה לAndroid/iOS modern.** מפתח שכותב `async fn on_create` ב-Akiva יודע מה הוא עושה אם הוא הגיע מ-Compose/SwiftUI.
2. **לא נדרש `Atomics.wait`** ב-app worker. ה-worker נשאר event-driven; ה-executor mid await עוצר את ה-task ומשחרר ה-event loop. אין blocking.
3. **structured concurrency באה בחינם.** `task!` macro spawns child task, שמתבטל אם parent task (`on_create`) cancelled.
4. **migration ל-Component Model נקי.** WIT `future<T>` ימופה ל-Rust `impl Future<Output=T>` ב-`wit-bindgen`. אנחנו כבר באותה semantic.

**העלות:**
- כל app הקיים (4) ייצריך rewrite ל-async. אבל ה-rewrite שטחי — רק לחתום על ה-API החדש. הלוגיקה הפנימית לא משתנה.
- embassy executor מוסיף ~5-10KB ל-binary של app.
- Framework-side צריך לnavigate state machine async (כמו ב-v1, אבל עכשיו טבעי כי ה-app גם async).

### 4.2 isolation: **worker-per-app**

יש שלוש granularities אפשריות:
- worker-per-activity: יותר מדי — overhead של 40ms × N activities.
- worker-per-task: לא משקף את ה-pkg boundary שהמשתמש מצפה לו.
- **worker-per-app: package = unit of isolation.**

**הבחירה: worker-per-app.** מתואם ל-mental model של המשתמש ("הרוג את calculator"), ל-storage sandbox (`{package_id}/...`), ול-permission model (grants per package).

### 4.3 transport: **postMessage primary, SAB אופציונלי**

ב-v1 בחרתי SAB+Atomics. ה-research מוכיח ש-postMessage ב-Chrome 2026 הוא 0-1ms — לא bottleneck. ה-SAB נדרש רק כש-wasm function חייב להחזיר ערך *לפני שהוא חוזר ל-event loop*.

עם async ABI **אין מקרה כזה** — wasm function תמיד יכולה לחזור (`Poll::Pending`) ולהמתין. ה-host כותב את התוצאה לbuffer וקורא ל-app worker שוב, app worker resumes את ה-future.

**מסקנה: postMessage כתחבורה ראשית.** SAB יישמר בidiom של "shared memory עבור frame buffer" (אם נרצה zero-copy rendering בעתיד) או "live state observer" (subscriptions ל-state changes), אבל **לא ל-blocking RPC**.

### 4.4 OPFS: **hybrid**

עם הממצא של multiple readonly handles:

- **קריאות של app מקבציה:** ה-app-worker פותח `readonly` handle. round-trip ישיר ל-disk, אין round-trip ל-kernel.
- **כתיבות:** ה-app-worker שולח ל-kernel-worker, kernel writes (יש לו `readwrite` exclusive). ה-kernel אוכף sandbox prefix `{package_id}/`.
- **gone-system files (sys/):** kernel-worker בלבד.
- **uninstall:** kernel-worker מורה ל-app-worker לסגור readonly handles, ממתין ack, ואז writes/deletes.

זה נותן fast read path עבור apps (calc, hello — בעיקר reads ב-`on_create`), עם safety של write-through-kernel.

**עלות:** קצת יותר logic ב-app-worker (פתיחה+מעקב אחרי handles). אבל בdoument'ed ב-storage layer של ה-SDK.

### 4.5 Component Model: **נדחה ל-v0.5+, ה-ABI שלנו ייקח השראה ממנו**

קל לדמיין rewrite גדול ל-Component Model — אבל ספירת המחיר:
- WASI 0.3 stable: late 2026
- Browser support דרך jco (transpile step)
- wit-bindgen Rust async עדיין משתנה
- אנחנו צריכים את Phase 11 working **עכשיו**

**הבחירה:** roll-our-own ABI עם semantics שמהדהדות Component Model:
- `request_id: u64` per host call — שכפול של "task" ב-Canonical ABI.
- "future" = future ID שה-app polls; "stream" = stream ID + chunked notifications.
- כששדורגים ל-Component Model ב-v0.5, ה-host translation layer תהיה straightforward — הסמנטיקה כבר תואמת.

### 4.6 async runtime באפליקציה: **embassy**

`embassy-executor` היא ה-runtime שיריץ אפליקציות Akiva. בחירה זו על פני אלטרנטיבות (`futures-executor`, `smol`):
- **no_std-friendly** — חשוב לwasm bundle size.
- **deterministic** — תזמון task-stealing דטרמיניסטי, חשוב לreproducibility בtests.
- **ecosystem רחב** — `embassy-time`, `embassy-sync` נותנים primitives שאנחנו צריכים (mutex, channel, signal).
- **WASM target נתמך** — ראה [embassy.dev/book](https://embassy.dev/book/).

ה-SDK יחביא את embassy תחת abstraction (`akiva_sdk::task::spawn`), אבל מתחת — embassy.

### 4.7 lifecycle scopes (Android/iOS-inspired)

ה-SDK יחשוף 3 scopes:

```rust
// 1. activity scope — בuto-cancelled at on_destroy
impl Context<'_> {
    pub fn activity_scope(&self) -> &TaskScope;  // tied to ActivityHandle
}

// 2. resumed scope — auto-cancelled at on_pause
impl Context<'_> {
    pub fn resumed_scope(&self) -> &TaskScope;
}

// 3. one-shot — מתבטל מיד אחרי שהפונקציה הקוראת חוזרת
// (זה ה-default אם משתמשים ב-await ישירות)
```

```rust
// דוגמה: app טוענת data ב-resume, מבטל אם user navigates back
async fn on_resume(&mut self, ctx: &mut Context<'_>) {
    ctx.resumed_scope().spawn(async {
        let data = fetch_from_storage("config").await;
        // אם user יוצא לפני שזה חוזר → cancelled, אין panic
        update_ui(data);
    });
}
```

זה שכפול 1:1 ל-`lifecycleScope` ב-Android. **DX זהה.**

### 4.8 watchdog: ping/pong ב-JS-level (ללא שינוי מ-v1)

V1 ההחלטה הזו הייתה נכונה ועוברת. ה-pong handler ב-`app_worker_shell.js` הוא JS-level — לא wasm — כך ש-`loop {}` ב-app code לא חוסם heartbeat.

עם async, יש פעם אחת ש-pong אינטראקטיבי באמת:
- אם ה-executor ב-app stuck (לא מספק callback ל-pong handler ב-due time) — זה bug ב-runtime שלנו, או memory exhaustion. גם אז watchdog יזהה.

---

## 5. ה-SDK החדש — חוויית מפתח

זה הסעיף המנחה לא רק ל-Phase 11 אלא לעמוד שלם בעצמו. הנקודה: מפתח חדש שכתב Android/iOS צריך להרגיש בבית.

### 5.1 hello-world

```rust
use akiva_sdk::prelude::*;

#[derive(Default)]
struct HelloActivity {
    name: String,
}

impl Activity for HelloActivity {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        // קריאת config, התחלת fetch, ניהול state — הכל async טבעי
        self.name = ctx.storage().get("user_name").await
            .unwrap_or_else(|| "World".to_string());
    }

    async fn on_render(&mut self, _ctx: &mut Context<'_>, c: &mut Canvas<'_>) {
        c.clear(0xff202020);
        c.draw_text(20.0, 80.0, 32.0, 0xffffffff, &format!("Hello, {}!", self.name));
    }
}

akiva_app!(HelloActivity);
```

הקוד הזה יוצא 70KB wasm (אחרי `wasm-opt`). הוא יותר ארוך מהגרסה הסינכרונית הנוכחית בכ-3 שורות, וה-ergonomics זהים ל-Android Compose `LaunchedEffect`.

### 5.2 lifecycle scopes — דוגמה מורחבת

```rust
struct WeatherActivity {
    state: WeatherState,
}

impl Activity for WeatherActivity {
    async fn on_resume(&mut self, ctx: &mut Context<'_>) {
        // Listener שחי כל זמן ש-Activity ב-Resumed
        ctx.resumed_scope().spawn(async {
            loop {
                let report = fetch_weather().await;
                update_ui(report);
                sleep(Duration::from_secs(60)).await;
            }
        });
        // אין צורך ב-on_pause cleanup — ה-scope מטפל
    }

    async fn on_pointer(&mut self, ctx: &mut Context<'_>, ev: PointerEvent) {
        if ev.is_refresh_button() {
            // task חיים שזורם רק עד ש-activity נהרסת
            ctx.activity_scope().spawn(async {
                let force = fetch_weather_force_refresh().await;
                update_ui(force);
            });
        }
    }
}
```

זה דפוס Compose/SwiftUI מדויק. אין "cleanup pattern" — ה-scopes עושים cleanup.

### 5.3 storage API — async first

```rust
// היום (sync)
let value: Option<Vec<u8>> = ctx.storage().get("key");

// מחר (async)
let value: Option<Vec<u8>> = ctx.storage().get("key").await;

// reads מקומיים מהירים ב-app-worker (readonly handle direct)
// writes עוברים round-trip לkernel (atomic, sandboxed)
let _ = ctx.storage().set("key", b"value").await;
```

API שטוח. הblocking הסמוי שב-v1 (Atomics.wait מתחת) נעלם — `await` מסביר את הזמן.

### 5.4 channels & signals (embassy-sync wrappers)

אפליקציות מורכבות יותר ירצו channel-based architecture. SDK יחשוף:

```rust
use akiva_sdk::sync::{Channel, Signal};

let (tx, rx) = Channel::new(16);  // bounded MPSC
ctx.activity_scope().spawn(async move {
    while let Some(msg) = rx.recv().await {
        handle(msg);
    }
});
```

זה אותו דפוס של Kotlin `Channel` או Swift `AsyncStream`.

### 5.5 dev tooling — onboarding ב-5 דקות

```bash
$ akiva new my-app
Created my-app/
$ cd my-app
$ akiva run
[akiva] devserver: http://127.0.0.1:4173
[akiva] my-app installed, ready
```

ה-`akiva new` יוצר:
- `Cargo.toml` עם ה-SDK pinned
- `manifest.toml` עם defaults הגיוניים
- `src/lib.rs` עם hello-world async
- `README.md` עם התוויית פיתוח

זה ה-**`create-react-app`** של Akiva. בלעדיו ה-DX לא מתחיל.

---

## 6. ABI החדש — איך wasm app מדבר עם host

### 6.1 הקונספט: future IDs עם polling

ה-app מבקש פעולה async, מקבל future ID, ועושה poll עד שיש תשובה.

```
app:                          host:
─────                         ─────
akiva_host_io_start(           
  op_kind: u32,
  args_ptr: *const u8,
  args_len: u32,
) -> u64 (future_id)          ◄──── allocates future, returns id
                               
akiva_host_io_poll(            
  future_id: u64,
  out_ptr: *mut u8,
  out_cap: u32,
) -> u32 (status)             ◄──── 0 = pending, 1 = ready, 2 = error
                               (if ready: writes bytes to out_ptr, returns u32 length encoded in status' top bits or via separate akiva_host_io_status query)
```

ה-app worker (ב-`app_worker_shell.js`) מקשיב לpostMessage `{type: "future_ready", id}`. כשמגיע, executor (embassy) wake's את הtask המבטל מהtoken המקושר ל-id, ובdraft הback ל-`akiva_host_io_poll` של ה-future.

**Cross-worker protocol (Stone 11.4c).** ה-`io_start` / `io_poll` / `fire` רצים בתוך app-worker, אבל חלק מה-op_kinds דורשים שירות מ-kernel-worker (QUERY_PACKAGES, START_ACTIVITY, STORAGE_SET, …). הזרימה:

1. **App-worker, on `io_start`:** מקצה `future_id` מ-local registry (counter פרטי ל-worker); רושם entry `{status: PENDING, packageId, payload: null}`; מחליט אם ה-op_kind מוגש locally (TEXT_WIDTH דרך OffscreenCanvas פרטי) או דורש kernel-worker. אם kernel — posts `{type: "host_op", future_id, op_kind, args}` ל-kernel-worker. מחזיר `future_id` ל-wasm מיד.
2. **Kernel-worker:** מעבד את ה-op (consults grants, runs handler), posts `{type: "host_op_done", future_id, status, payload}` חזרה לאותו app-worker. ה-payload מועבר כ-Transferable Uint8Array.
3. **App-worker, on `host_op_done`:** מעדכן `_futures[future_id] = {status, payload}`; ה-embassy executor רואה את השינוי ב-poll הבא של ה-task המבטל.
4. **App-worker, on `io_poll(future_id, out_ptr, out_cap)`:** קורא מה-local `_futures`. ה-API קונסיסטנטי עם 11.3 — זהה two-phase contract.

**`fire` ops** עוברים אותה זרימה אבל ללא future_id (ה-app-worker שולח, לא מחכה לתשובה). LOG מטופל locally ב-app-worker (כותב ל-console עם prefix `[worker:<package>]`); FINISH/START_ACTIVITY/SET_RESULT מועברים ל-kernel-worker שמעדכן את `_pendingFinish` / `_pendingIntent` / `_pendingSetResult` של ה-handle ב-AppRegistry של ה-kernel.

**Lifecycle direction (kernel → app)** מקבילה: kernel-worker posts `{type: "dispatch", dispatch_id, cmd_kind, payload}` ל-app-worker. app-worker בvokes `akiva_app_dispatch(cmd_kind, ptr, len)` (Stone 11.4b — single export), מקבל future_id wasm-side, polls עד resolved (ה-async fn יכול ל-await על host calls באמצע), ואז posts `{type: "dispatch_done", dispatch_id, result_bytes}` חזרה. kernel-worker מעדכן את `WasmHost::poll_future(dispatch_id)`.

**`dispatch_id` vs `future_id`.** שני מספרים נפרדים: `dispatch_id` הוא ה-tracking של kernel→app (kernel-worker מקצה, app-worker עונה); `future_id` הוא ה-tracking של app→host (app-worker מקצה, kernel-worker עונה). אין collision כי הם רצים ב-namespaces נפרדים על ערוצים מנוגדים.

### 6.2 fire-and-forget calls

לcalls בלי result (`log_info!`, `start_activity`, `set_result`):

```
akiva_host_fire(
  op_kind: u32,
  args_ptr: *const u8,
  args_len: u32,
) -> u32 (0 = ok, !=0 = error)
```

חוזר מיידית, אין future. ה-args מועתקים ל-host queue, ולא משנה אם ה-app ממשיך לרוץ. דוגמה:

```rust
// SDK:
pub fn log_info(msg: &str) {
    unsafe { akiva_host_fire(OP_LOG_INFO, msg.as_ptr(), msg.len() as u32); }
}
```

### 6.3 lifecycle dispatch

ה-host (kernel-worker) קורא ל-app-worker עם lifecycle command:

```
app exports:
akiva_app_dispatch(
  cmd_kind: u32,        // 0 = create, 1 = start, 2 = resume, ..., 6 = pointer, 7 = render, ...
  payload_ptr: *const u8,
  payload_len: u32,
) -> u64 (lifecycle_future_id)
```

ה-host מחזיק `pending` slot ב-`ActivityRecord`; כל frame בודק אם ה-future ready. ה-state machine async (כמו ב-v1) ימשיך ל-frame הבא רק אחרי ack.

### 6.4 wire format — דמיון ל-Component Model

כל op_kind יקבל encoder/decoder ב-WIT-style:

```
// היום, ב-pkgs_wire.rs
pub fn encode(refs: &[PackageRef]) -> Vec<u8> { ... }
pub fn decode(bytes: &[u8]) -> Result<Vec<PackageEntry>> { ... }
```

ב-Phase 11 נכליל את הדפוס לבונדל פרוטוקול:

```
framework/src/wire/
├── mod.rs             // re-exports
├── op_kinds.rs        // u32 constants per op + cross-crate parity test
├── pkgs.rs            // QUERY_PACKAGES encoder/decoder
└── (per-op modules added as new ops land — log/start_activity/storage encode inline today)
```

**מצב נוכחי (post-Stone 11.3):** רק `op_kinds.rs` + `pkgs.rs` נדרשו בפועל. ה-args layouts של LOG / FINISH / START_ACTIVITY / TEXT_WIDTH פשוטים מספיק (header u32 + bytes) שה-dispatcher ב-`runtime/web/app_registry.js` מקודד inline ללא module ייעודי. ה-rule פשוט: op_kind ש-args/result שלו מצריכים ≥3 שדות structured, או shared encode/decode בין SDK ל-host — מקבל module משלו ב-`wire/`. אחרים נשארים inline. STORAGE_GET/SET ב-Stone 11.6 כנראה יקבלו `wire/storage.rs` משלהם.

כשנעבור ל-Component Model ב-v0.5, ה-encoders/decoders האלה ייהפכו ל-WIT-generated code אוטומטית.

---

## 7. lifecycle כ-state machine async

### 7.1 ה-state machine

```rust
pub enum LifecycleState {
    Initialized,
    Creating,    // pending: create future ID
    Created,
    Starting,
    Started,
    Resuming,
    Resumed,
    Pausing,
    Paused,
    Stopping,
    Stopped,
    Destroying,
    Destroyed,
    Trapped,    // terminal: wasm trap, panic, or watchdog timeout
}

pub struct ActivityRecord {
    pub handle: ActivityHandle,
    pub state: LifecycleState,
    pub pending: Option<PendingTransition>,
    pub trapped_reason: Option<String>,
    // ...
}

pub struct PendingTransition {
    pub target: LifecycleState,
    pub future_id: u64,
    pub started_at: Timestamp,
    pub deadline: Timestamp,  // for watchdog
}
```

### 7.2 frame loop

ב-kernel-worker בכל frame:

```rust
fn drive_lifecycle_transitions(&mut self, now: Timestamp) {
    for record in &mut self.activities {
        if let Some(pending) = &record.pending {
            if let Some(result) = self.workers.poll_future(pending.future_id) {
                record.state = pending.target;
                record.pending = None;
                self.advance_to_next_state(record);
            } else if now > pending.deadline {
                self.mark_trapped(record, "lifecycle timeout".into());
            }
        }
    }
}

fn advance_to_next_state(&mut self, record: &mut ActivityRecord) {
    let next = match record.state {
        Created => Some(Started),
        Started => Some(Resumed),
        Pausing => Some(Paused),
        // ...
        _ => None,
    };
    if let Some(target) = next {
        let future_id = self.workers.dispatch_lifecycle(record.handle, target);
        record.pending = Some(PendingTransition { target, future_id, ... });
    }
}
```

### 7.3 testing — MockHost נשארת sync

כמו ב-v1: `WasmHost` trait מצהיר על async methods, `MockHost` מימוש sync (resolve immediately), `WorkerHost` מימוש async דרך postMessage.

ב-tests:
```rust
let host = MockHost::new();
let mut fw = framework::boot(storage, host);
fw.activities.drive_to_resumed(handle).await;  // immediate in MockHost
assert_eq!(fw.activities.state_of(handle), Resumed);
```

ה-141 tests הקיימים ייבנו מחדש עם `.await` בכל מקום שצריך, אבל הסמנטיקה תישמר.

---

## 8. fault containment

### 8.1 דרכי כשל

1. **wasm trap** (panic / unreachable / OOB): `try { exports.akiva_app_dispatch(...) } catch (e) { ... }` ב-`app_worker_shell.js` → postMessage ל-kernel "trapped" עם reason.
2. **app-worker JS-level error**: `worker.onerror` ב-kernel-worker → trap.
3. **app-worker hang**: watchdog ping timeout → terminate → trap.
4. **executor panic** (embassy panics בתוך ה-wasm — אם user spawns task שpanics): גם נתפס כ-trap.

### 8.2 cleanup

```rust
fn mark_trapped(&mut self, record: &mut ActivityRecord, reason: String) {
    record.state = Trapped;
    record.trapped_reason = Some(reason.clone());
    record.pending = None;
    
    // 1. כל activities של אותו package — Trapped (heap משותף)
    let pkg = record.package_id.clone();
    for other in &mut self.activities {
        if other.package_id == pkg && !matches!(other.state, Trapped | Destroyed) {
            other.state = Trapped;
            other.trapped_reason = Some(format!("co-located trap in {}", reason));
        }
    }
    
    // 2. terminate worker
    self.workers.terminate(&pkg);
    
    // 3. activity הקודם ב-back-stack מקודם ל-Resumed
    self.tasks.pop_trapped_from_foreground();
}
```

### 8.3 re-launch

App שעברה Trapped יכולה להיפתח שוב מ-Shell — kernel יראה שאין worker, יspawn worker חדש, יטען wasm מ-OPFS, יתחיל clean. saved state נשמר בOPFS אם היה.

**אין retry אוטומטי.** אם ה-app crashed ב-`on_create`, retry יחזור ל-trap. עדיף clean restart גרידא.

---

## 9. watchdog

ללא שינוי מ-v1:

- ping פעם בשנייה (60 frames @ 60fps).
- pong handler ב-`app_worker_shell.js` ב-JS-level — לא עובר דרך wasm.
- timeout: 1 שנייה. עברה — `worker.terminate()` + `Trapped`.

הסעיף הזה נשמר זהה כי ה-design הזה הוא orthogonal ל-async ABI.

---

## 10. Stones (revised)

### Stone 11.1 — Design doc (זה) ✅
**Done.** מסמך זה.

### Stone 11.2 — async SDK + Activity trait (3-4 שבועות)
**מטרה:** SDK חדש עם async API. עדיין רץ ב-single worker (לא isolation עדיין).
- `embassy-executor` integration ב-SDK.
- `Activity` trait async.
- `Context` async methods.
- 3 lifecycle scopes (`activity_scope`, `resumed_scope`, one-shot).
- migration של 4 האפליקציות הקיימות (calc, hello, shell, tic-tac-toe) ל-async API.
- כל ה-141 framework tests ממשיכים לעבור.

**Definition of Done:** כל 4 האפליקציות רצות עם async API, אין regressions.

### Stone 11.3 — async ABI + future registry (2-3 שבועות)
**מטרה:** ה-host calls לא חוזרים מיידית. ה-app polls future IDs.
- `akiva_host_io_start`, `akiva_host_io_poll`, `akiva_host_fire` imports.
- `FutureRegistry` ב-kernel-worker.
- wire encoders ב-`framework/src/wire/`.
- `runtime/web/app_registry.js` משתנה ל-future-based dispatch.
- עדיין single worker — אבל ה-flow הוא כבר async-ready.

**Definition of Done:** host calls עוברות דרך future registry ב-single worker; latency benchmark ≤ 1ms.

### Stone 11.4 — worker-per-app (3-4 שבועות)
**מטרה:** **ה-isolation**. כל app ב-Worker משלה.

**מחולק ל-3 sub-stones** כי זה ה-stone הגדול של Phase 11 ו-refactor של ה-framework state machine מצדיק commit נפרד ממה שמערב את ה-postMessage transport.

#### Stone 11.4a — async WasmHost trait + framework state machine
**מטרה:** ה-framework מוכן לעולם async. עדיין worker יחיד.
- `LifecycleState` enum מורחב לפי §7.1: `Initialized → Creating → Created → Starting → Started → Resuming → Resumed → Pausing → Paused → Stopping → Stopped → Destroying → Destroyed`. `Trapped` מצטרף ב-11.5.
- `ActivityRecord` מקבל `pending: Option<PendingTransition>` עם `target`, `future_id`, `started_at`, `deadline`.
- `WasmHost` trait: כל ה-mutating ops (lifecycle, render, save_state, restore_state, pointer, key, resize, new_intent, activity_result) מחזירים `u64 future_id` במקום לבצע inline. שתי מתודות חדשות: `poll_future(future_id) -> Option<Result<Vec<u8>, String>>` ו-`drop_future(future_id)`.
- `MockHost` ב-tests: resolve מיד (ה-future_id `1` תמיד `Some(Ok(...))` בקריאה ראשונה ל-poll). שימור compatibility עם ה-tests הקיימים.
- `KernelWasmHost` ב-runtime: באותה רוח — single-worker, ה-call ל-`AppRegistry` ב-host מתבצע מיד והתוצאה נשמרת ב-future_id. רק ה-shape של ה-trait משתנה.
- `ActivityManager`: לולאת frame קוראת `drive_lifecycle_transitions(now)` שדוחה כל `ActivityRecord` עם `pending` עד שהיא ready. עד אז הקריאה הבאה ל-`Activity::on_*` נדחית.
- כל הבדיקות הקיימות מסתגלות (~141 framework tests).

**Definition of Done:** כל ה-test suites עוברות עם ה-trait async. ה-runtime עדיין single-worker, אבל ה-flow כבר מתאים ל-11.4c.

#### Stone 11.4b — collapse lifecycle exports ל-`akiva_app_dispatch`
**מטרה:** קריאת lifecycle יחידה בצד ה-app, לפי §6.3.
- SDK: `akiva_app!(MyActivity)` macro מייצר export יחיד `akiva_app_dispatch(cmd_kind: u32, payload_ptr: *const u8, payload_len: u32) -> u64`. הפנימי שלו decodes cmd_kind ל-create/start/resume/pause/stop/destroy/pointer/key/resize/render/save_state/restore_state/new_intent/activity_result. מחזיר future_id (כל הקריאות עכשיו async ב-app side — ה-executor של embassy מחזיר `Pending` אם ה-`async fn on_*` מ-`await`'s על host call).
- אם הקריאה הסתיימה sync (כמו on_render שלא await על שום דבר), ה-future_id `0` יוחזר ו-host יודע שהוא יכול לקרוא ל-`akiva_app_get_result(future_id=0)` מיד. שאר ה-future_ids הם 1+.
- `runtime/web/app_registry.js` ו-`framework/src/wasm_activity.rs::KernelWasmHost`: לקרוא ל-`akiva_app_dispatch(cmd_kind, ...)` במקום ל-13 ה-exports הנפרדים. `REQUIRED_EXPORTS` מתכווץ.
- 4 ה-apps מוסתגלות (כולן עוברות דרך ה-macro, אבל ה-macro מעדכן בהן).
- abi.md §2.2 מתעדכן.

**Definition of Done:** 4 apps רצות עם export יחיד; כל ה-tests עוברות; ה-app שmissing the dispatch export נדחה ב-load.

#### Stone 11.4c — `app_worker_shell.js` + WorkerPool isolation
**מטרה:** **ה-isolation האמיתי**. כל app ב-Worker משלה. lifecycle ו-host ops עוברים postMessage.
- `runtime/web/app_worker_shell.js` חדש: ה-Worker bootstraps את wasm app יחיד, מחזיק future_registry מקומי, מקשיב ל-`{type:"dispatch", future_id, cmd_kind, payload}` מ-kernel-worker, מבצע `akiva_app_dispatch`, מחזיר `{type:"dispatch_done", future_id, result}`.
- App-worker מטפל ב-host imports בתוכו: `akiva_host_fire` / `akiva_host_io_start` / `akiva_host_io_poll` יורדים ל-app-worker-local registry. עבור op_kinds שדורשים שירות מ-kernel (TEXT_WIDTH, QUERY_PACKAGES, START_ACTIVITY וכו') — app-worker מקצה future_id מקומי, posts `{type:"host_op", future_id, op_kind, args}` ל-kernel-worker, מחזיר את ה-future_id מיד. כשה-kernel-worker מחזיר `{type:"host_op_done", future_id, status, payload}` — app-worker מעדכן את ה-local registry; הקריאה הבאה ל-`akiva_host_io_poll` רואה READY.
- App-worker מטפל ב-LOG locally (logs prefixed `[worker:<package>]`); FINISH/START_ACTIVITY/SET_RESULT proxied ל-kernel.
- TEXT_WIDTH: כל app-worker מקבל OffscreenCanvas משלו ל-measureText (אין צורך ב-round-trip — ה-API זמין בכל worker; לא כל op_kind מוסכם בהכרח kernel).
- `runtime/web/app_registry.js` הופך ל-`WorkerPool`: `_processes: Map<packageId, Worker>` במקום `Map<packageId, WebAssembly.Instance>`. `register()` מ-spawn worker ב-first use; `unregister()` משאיר את ה-worker חי (lazy GC).
- Paint-op transfer: ה-app-worker מחזיר `{type:"render_done", future_id, paint_bytes}` כ-Transferable Uint8Array. kernel-worker copies ל-kernel memory.
- pong handler stub ב-`app_worker_shell.js` (מיועד ל-11.5; כרגע רק מאשר alive).
- log breadcrumbs: כל log line נושא tag `[worker:<package>]`. ראייה ויזואלית ל-isolation.

**Definition of Done:** 4 apps ב-4 Workers נפרדים; Shell → calc → back עובד; ה-DevTools חושף 5 workers (kernel + 4 app-workers); ה-tests של app_registry עוברים מחדש על ה-WorkerPool API.

### Stone 11.5 — fault containment + watchdog (2 שבועות)
**מטרה:** Trapped state, cleanup flow, ping/pong watchdog.
- `LifecycleState::Trapped`.
- `worker.onerror` → mark Trapped.
- Try/catch ב-app_worker_shell → mark Trapped.
- ping/pong ב-JS-level, 1s timeout.
- Integration tests: panic-app, infinite-loop-app, OOM-app — כולן מטופלות.

**Definition of Done:** test app שעושה `loop {}` ב-`on_pointer` נהרגת ב-≤ 1.5s, Shell חיה.

### Stone 11.6 — OPFS hybrid + storage SDK (1-2 שבועות)
**מטרה:** readonly handles ישירות ב-app-worker; writes through kernel.
- `akiva_sdk::storage` API — async, transparent.
- app-worker storage layer — readonly handles, fallback ל-kernel ב-error.
- kernel-worker מנהל close-on-uninstall protocol.
- benchmark: read latency comparison (direct vs kernel-routed).

**Definition of Done:** read benchmarks מראים improvement; uninstall flow lockdown מאומת.

### Stone 11.7 — `akiva new` + dev tooling polish (1-2 שבועות)
**מטרה:** onboarding ב-5 דקות.
- `akiva new <name>` — scaffold app חדש.
- Templates: `default`, `compose-style`, `game`.
- README.md generated עם dev steps.
- `akiva docs` — מציג דוקומנטציה רלוונטית של SDK.
- Documentation pass: כל ה-public API ב-`akiva-sdk` עם `///` doc comments + דוגמאות.

**Definition of Done:** מפתח חדש (אדם זר) יכול לעקוב אחרי README ולהריץ hello-world ב-≤ 5 דקות.

### Stone 11.8 — performance pass + benchmarks doc (1 שבוע)
**מטרה:** מדידת overhead, תיעוד baseline.
- `docs/perf-baseline.md` — מדידות לפני/אחרי isolation.
- Optimizations critical paths (אם נמצאו): batching של logs, transferable for canvas operations.
- p99 frame time עם 3 apps ב-foreground.

**Definition of Done:** 60fps יציב עם 3 apps רצים; כל metric מתועד.

**הערכת זמן Total: 13-19 שבועות.** משלב 11.2 (החל מהיום).

---

## 11. Exit criteria של Phase 11

- [ ] 4 אפליקציות (Shell, calc, hello, tictactoe) רצות ב-4 Workers נפרדים. console.log מוכיח.
- [ ] App שעושה `loop {}` ב-`on_pointer` לא מקפיא; נהרגת ב-≤ 1.5s.
- [ ] App שעושה `panic!()` ב-`on_render` — Trapped, framework cleanup, Shell ב-Resumed.
- [ ] **141 framework + 146 sdk + 68 kernel + 50 vitest tests** עוברים. צפויה תוספת של ~30-50 tests חדשים.
- [ ] host call overhead ≤ 1ms ב-Chrome/Firefox latest על Linux x86_64.
- [ ] storage read (direct readonly) ≤ 0.5ms; storage write (through kernel) ≤ 2ms.
- [ ] `akiva new my-app && cd my-app && akiva run` — hello-world על המסך תוך 30 שניות מקבלת command ראשונה.
- [ ] `docs/sdk-rust.md` מעודכן עם async API, 3 דוגמאות מקיפות (`Activity` בסיסית, lifecycle-scoped task, channel-based architecture).
- [ ] `docs/perf-baseline.md` קיים עם מדידות before/after.

---

## 12. סיכונים וtrade-offs

### 12.1 סיכון: embassy executor — wasm bundle bloat

embassy-executor + embassy-sync = ~10KB code overhead per app אחרי `wasm-opt`. 4 apps × 10KB = 40KB extra. אם בעתיד נרצה 100 apps זה לא נורא, אבל worth להחליט.

**Mitigation:** dynamic-link ה-runtime — wasm Component Model מאפשר shared modules. ב-v0.5 (כשנעבור ל-Component Model) זה ייפתר. בינתיים בכל app `embassy` היא static.

### 12.2 סיכון: rewrite של 4 apps + SDK

זה היקף המאמץ ב-Stone 11.2. ה-mitigation: ה-rewrite שטחי (חתימות נשתנו, לוגיקה לא). מומלץ לעבור על app-by-app, לא bulk.

### 12.3 סיכון: spec churn ב-embassy

embassy מתפתחת. ייתכנו breaking changes ב-API שלה ב-12 חודשים הקרובים.

**Mitigation:** pin version, plan upgrade reviews. רוב ה-API stabilized ב-2024-2025 — סיכון ירד.

### 12.4 trade-off: עזב Component Model עכשיו

אם נעבור ל-Component Model ב-v0.5, נצטרך migration. כתבנו את ABI שלנו עם semantics קרובות (futures, streams), אבל זה לא automatic.

**Mitigation:** החלטה שקולה לעלות ה-spec churn. נחזור לבדוק את WASI 0.3 stability ב-Phase 14 review.

### 12.5 סיכון: ה-watchdog הורג app שעובדת על task כבד "חוקי"

App שעושה `for i in 0..1_000_000` — לא loop infinity, אבל לא חוזרת ל-event loop ל-1.2s. תיהרג בטעות.

**Mitigation:** SDK guidance: heavy work ב-`task!` חיצוני, אינו חוסם ה-main thread של הapp. embassy executor יבטיח שגם בתוך task גדול ה-yield-points (await של channel/timer) משחרר ל-event loop. לתעד ב-`docs/sdk-rust.md`.

### 12.6 סיכון: documentation debt

הסיכון הגדול ביותר ל-DX. אם מסיימים את הקוד אבל לא מתעדים — ה-vision (DX זהה ל-Android/iOS) נופל.

**Mitigation:** Stone 11.7 פרק נפרד; ה-Definition of Done שלו תלוי בdocumentation מקיפה. לא lol-bypass אם הזמן נגמר.

### 12.7 סיכון: rabbit-hole של "ergonomic perfectionism"

`task!`, `task_until!`, channels, signals, streams — כל אחד יכול להיות פרק שלם של מימוש. ה-bar הוא Android/iOS, אבל לא להעתיק את כל ה-API surface — לקחת את ה-80% הראשונים.

**Mitigation:** explicit list ב-Stone 11.2 של מה ה-MVP scope. השאר נדחה.

---

## 13. מה Phase 11 **לא** מטפל בו

נשמר ל-Phase 12 (Resource Governance):
- memory accounting per app
- CPU attribution per worker
- Low-Memory Killer policy
- App suspension (CACHED state)

נשמר ל-Phase 13 (WindowManager):
- multi-window
- frame budget arbitration בין apps
- focus routing

נשמר ל-Phase 14+:
- Component Model migration
- jco-style transpilation pipeline
- WIT-defined ABI
- background services

נשמר ל-future-future:
- iframe-per-app (Spectre-grade isolation, דורש DNS)
- third-party app store with untrusted code
- code signing

---

## 14. references

### מסמכי Akiva
- [`ROADMAP.md` Phase 11](../ROADMAP.md)
- [`docs/runtime-web.md`](runtime-web.md)
- [`docs/abi.md`](abi.md)
- [`docs/activity-model.md`](activity-model.md)
- [`docs/storage.md`](storage.md)
- [`docs/sdk-rust.md`](sdk-rust.md)
- [`framework/src/wasm_activity.rs`](../framework/src/wasm_activity.rs)
- [`runtime/web/app_registry.js`](../runtime/web/app_registry.js)

### חיצוני — research basis
- **WASI 0.3 / Component Model:**
  - [WASI Roadmap](https://wasi.dev/roadmap)
  - [WASI 0.3 Native Async (byteiota)](https://byteiota.com/wasi-0-3-native-async-webassembly-gets-concurrent-i-o/)
  - [Component Model status (eunomia)](https://eunomia.dev/blog/2025/02/16/wasi-and-the-webassembly-component-model-current-status/)
- **Browser isolation:**
  - [Chromium Site Isolation](https://chromium.googlesource.com/chromium/src/+/main/docs/process_model_and_site_isolation.md)
  - [Document Isolation Policy (Chrome 137+)](https://developer.chrome.com/blog/document-isolation-policy)
  - [Cross-Origin Isolation Guide](https://web.dev/articles/cross-origin-isolation-guide)
- **OPFS:**
  - [FileSystemSyncAccessHandle (MDN)](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemFileHandle/createSyncAccessHandle)
  - [Multiple readonly handles support](https://developer.mozilla.org/en-US/docs/Web/API/File_System_API/Origin_private_file_system)
- **postMessage performance:**
  - [Is postMessage slow? (Surma 2019)](https://surma.dev/things/is-postmessage-slow/)
  - [Web Worker messages 2026 benchmarks](https://copyprogramming.com/howto/how-fast-are-web-worker-s-messages)
- **Modern app frameworks:**
  - [Android: Kotlin coroutines + lifecycle](https://developer.android.com/topic/libraries/architecture/coroutines)
  - [Compose: side-effects](https://developer.android.com/develop/ui/compose/side-effects)
  - [SwiftUI: .task modifier](https://medium.com/@sarathiskannan/mastering-task-modifier-in-swiftui-89c0e3d53667)
  - [Swift: structured concurrency](https://medium.com/@sraavanchevireddy/mastering-concurrency-in-swiftui-a-deep-dive-into-async-await-and-structured-concurrency-74b2f00a2a24)
- **embassy:**
  - [Embassy book](https://embassy.dev/book/)
  - [embassy-executor on docs.rs](https://docs.rs/embassy-executor)
- **WebContainers (precedent):**
  - [WebContainers intro](https://blog.stackblitz.com/posts/introducing-webcontainers/)

---

**מצב המסמך:** Design v2.1 (2026-05-01). Stones 11.0-11.3 הושלמו. ה-update הזה מוסיף sub-stones (11.4a/b/c) ל-Stone 11.4, מפרט את ה-cross-worker protocol ב-§6 (dispatch_id vs future_id, OffscreenCanvas per app-worker, paint Transferable), ומיישר את §6.4 (`wire/`) למצב בפועל אחרי Stone 11.3. Stone 11.4a רשאי להתחיל.
