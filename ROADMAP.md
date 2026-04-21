# תוכנית פיתוח — Akiva Platform

## פילוסופיה

**שלד רזה, אבל שלד אמיתי.** כל תת-מערכת של OS חייבת להתקיים. אף אחת מהן לא יכולה להיות mock או simulation. מותר להן להיות מצומצמות — חובה שיהיו ממשיות.

**הכלל המנחה:** אם אי פעם נזדקק להוסיף תת-מערכת בדיעבד, היא תדרוש שכתוב. לכן כל תת-מערכת נבנית מההתחלה, גם אם מינימלית מאוד.

## מפת מרכיבים — מה חייב להתקיים עד MVP

מכל אחד מאלה יהיה v0.1 מינימלי:

- ✅ **JS HAL** (Tab + Worker) — מחוברים דרך MessagePort
- ✅ **Kernel**: WasmLoader, MemoryManager, Scheduler, EventQueue, StorageManager, IPCBus, DisplayManager
- ✅ **Framework**: ActivityManager, PackageManager, IntentRouter, WindowManager, ResourceManager, PermissionManager
- ✅ **UI stack**: LayoutInflater, LayoutEngine, Compositor
- ✅ **AKML parser** עם תמיכה ב-XML בסיסי וב-resource references
- ✅ **SDK ב-Rust** עם ABI bindings + Activity lifecycle
- ✅ **Shell app** (אפליקציית בית פשוטה)
- ✅ **Calculator app** (הוכחת יכולת אמיתית)
- ✅ **CLI** (`akiva new`, `akiva build`, `akiva run`)

---

## שלב 0 — תכנון

**תוצרים:**
- [x] vision.md — החזון
- [x] ARCHITECTURE.md — הארכיטקטורה
- [x] ROADMAP.md — מסמך זה
- [x] docs/abi.md — חוזה ה-ABI (WIT-based)
- [x] docs/manifest.md — פורמט ה-manifest
- [x] docs/akml.md — שפת ה-layout
- [x] docs/resources.md — מערכת ה-resources
- [x] docs/activity-model.md — מודל Activity
- [x] docs/intents.md — מערכת Intents
- [x] docs/window-manager.md — Window Manager ו-Shell
- [x] docs/sdk-rust.md — SDK
- [x] docs/notifications.md — Notifications (MVP-thin)
- [x] docs/testing.md — אסטרטגיית בדיקות
- [x] docs/observability.md — Logging & debugging

**Exit criteria:** יש הסכמה על כל תת-מערכת ברמת design.

---

## שלב 1a — Wasm Hello World בתוך Browser ✅ הושלם

**סטטוס:** הושלם באפריל 2026. Kernel wasm נטען ב-Dedicated Worker, מצייר על OffscreenCanvas דרך Canvas2DBackend, מדפיס את הטקסט "Akiva Kernel v0.0.1". מלווה ב-HAL ראשוני (tab.js + worker.js + leader.js) ומבנה מלא של workspace. ראה [docs/runtime-web.md](docs/runtime-web.md).

**מטרה:** Rust wasm module קטן מחובר ל-OffscreenCanvas דרך Worker, מציג "Akiva Kernel v0.1" שחור על לבן. **אין** עדיין leader election, multi-tab, או ABI.

**רכיבים שנבנים:**

### 1a.1 Tooling
- `Cargo.toml` workspace ברמה עליונה
- `kernel/Cargo.toml` — target `wasm32-unknown-unknown`
- `kernel/src/lib.rs` — `#[no_mangle] fn boot() -> ()` + placeholder frame loop
- CI: `cargo build --target wasm32-unknown-unknown --release`
- wasm-opt post-build

### 1a.2 Web shell
- `runtime/web/index.html`
- `runtime/web/tab.js` — טוען `worker.js`, עושה `canvas.transferControlToOffscreen`, שולח ל-worker
- `runtime/web/worker.js` — טוען `akiva-kernel.wasm` (fetch + `WebAssembly.instantiate`), מקבל OffscreenCanvas, קורא `boot()` של ה-wasm
- imports מינימליים: `akiva_fill_rect`, `akiva_fill_text`, `akiva_log`

### 1a.3 First frame
- ה-wasm קורא `akiva_fill_rect` לרקע
- `akiva_fill_text` לטקסט
- Worker מממש את ה-imports דרך Canvas 2D על ה-OffscreenCanvas

**Exit criteria:**
- `cargo build --release` + `akiva run` → `http://localhost:4173` → tab מציג רקע שחור + "Akiva Kernel v0.1" לבן
- `console.info` מה-wasm מופיע ב-DevTools של ה-Worker

**הערכת זמן:** 1 שבוע

---

## שלב 1b — HAL Structure + Leader Election + Multi-tab 🟡 חלקי

**סטטוס (אפריל 2026):**
- ✅ Leader election דרך `navigator.locks.request` + BroadcastChannel probe — ב-`runtime/web/leader.js` (מחלקת `Session`).
- ✅ Multi-tab: followers רשומים אצל ה-leader. טאב שני מזהה את עצמו כ-follower אוטומטית.
- ✅ Leader failover: סגירת ה-leader משחררת את ה-lock; ה-callback של ה-follower הממתין נקרא אוטומטית, מרים worker חדש, מעביר את ה-canvas שלו. ראה [docs/runtime-web.md §3.4](docs/runtime-web.md).
- ✅ Event pipeline שלם end-to-end: DOM → tab.js → Session → Worker → `akiva_on_event` → EventQueue → `Kernel::handle_event` → DisplayManager → Canvas2D. הוכחה חזותית: מחוון עכבר שעוקב אחרי הפואינטר, משנה צבע בלחיצה.
- ✅ Resize: המידות נשלחות ל-worker ומתקדכנות ב-backend וב-display metrics של ה-kernel.
- 🟡 **Follower rendering** — עדיין לא מצויר. טאב follower לא רואה כלום עד שהוא מקודם. Cross-tab composition נדחה ל-Phase 2.
- ✅ **Storage** — OPFS + SyncAccessHandle מחוברים. `kernel::storage::{read,write,delete}` סינכרוניים מול in-memory map שמגובה ב-`akiva-store.v1.bin`. Boot-counter כ-smoke test. ראה [docs/storage.md](docs/storage.md).

**מטרה:** tab אחד מתחרה על Web Locks, מרים Worker, ומגיש displays ל-tabs נוספים. Runtime עדיין שלד — אין framework. אבל Multi-tab עובד end-to-end.

**רכיבים שנבנים:**

### 1b.1 Leader election + BroadcastChannel coordination
- `runtime/web/tab.js` מקבל הרחבה:
  - `probeForLeader()` ב-BroadcastChannel `akiva.os`
  - אם אף אחד לא עונה → `navigator.locks.request("akiva.os.leader")`
  - Leader מרים Worker + רושם handler לפניות `who-is-leader?`
  - Follower מקבל MessagePort מה-Leader

### 1b.2 Display registration
- Tab HAL שולח `register-display { dpr, width_px, height_px, OffscreenCanvas }` ל-Worker
- Worker HAL מחזיק `HashMap<DisplayId, MessagePort + Canvas>`

### 1b.3 Kernel skeleton (Rust → wasm, מלא)
- `kernel/src/memory.rs` — `MemoryManager` (quota tracker, אין enforcement עדיין)
- `kernel/src/scheduler.rs` — frame loop דרך `requestAnimationFrame` של ה-Worker
- `kernel/src/event_queue.rs` — FIFO של events, router לפי display_id
- `kernel/src/display_manager.rs` — הוספת/הסרת displays
- `kernel/src/storage.rs` — stub (קורא מ-Worker דרך import)
- `kernel/src/wasm_loader.rs` — stub

### 1b.4 Leader failover
- כשה-leader נסגר, follower next מקבל את ה-lock
- מרים Worker חדש, Runtime קורא state (ריק ב-1b) מ-OPFS
- Tabs אחרים פותחים MessagePort חדש

**Exit criteria:**
- 2 tabs פתוחים → שני displays רשומים ב-Worker, שניהם מציגים "Akiva Kernel v0.1 — display #1/2"
- סגירת ה-leader → ה-tab השני הופך ל-leader תוך <500ms, ממשיך להציג
- Console log מראה transition של leadership
- Resize של tab → display-metrics נשלח ל-Worker, הרנדור מעודכן

**הערכת זמן:** 2 שבועות

---

## שלב 2 — Framework Core: Activity + Package + Resource Managers

**מטרה:** Framework שיודע על אפליקציות, טוען manifest, ומנהל activity lifecycle. עדיין אין אפליקציות אמיתיות.

**רכיבים שנבנים:**

### 2.1 PackageManager
- `framework/package_manager/`
- קורא `apps/*/manifest.toml` מה-storage
- בונה רשימת `InstalledApp { id, name, activities, intent_filters, permissions }`
- API: `list_apps()`, `get_app(id)`, `query_intent(intent)`

### 2.2 ResourceManager
- `framework/resource_manager/`
- טוען device config (locale, orientation, night mode)
- Resource qualifier matching algorithm
- API: `get_string(ref)`, `get_color(ref)`, `get_dimen(ref)`, `get_layout(ref)`
- תמיכה ב-`@string/`, `@color/`, `@dimen/`, `@layout/`, `@style/`, `@drawable/`

### 2.3 ActivityManager
- `framework/activity_manager/`
- Task stack (Vec<ActivityInstance>)
- Lifecycle state machine (Created → Started → Resumed → Paused → Stopped → Destroyed)
- API: `launch(intent)`, `finish(activity_id)`, `back()`
- Saved state support

### 2.4 IntentRouter
- `framework/intent/`
- `Intent` struct
- `IntentFilter` matching
- Delegates to PackageManager for resolution, ActivityManager for launch

### 2.5 PermissionManager
- `framework/permissions/`
- מודל בסיסי: `required` ב-manifest auto-granted, enforcement ב-ABI
- Storage של grants

### 2.6 WindowManager
- `framework/window_manager/`
- ActivityManager → WindowManager: "render activity X on display Y"
- Transition bookkeeping (ללא animations ב-MVP)

**Exit criteria:**
- `PackageManager.list_apps()` מחזיר רשימה ריקה (אין עדיין אפליקציות)
- `ActivityManager.launch(intent_for_nonexistent_app)` מחזיר שגיאה מובנת
- ResourceManager טוען device config ומוכן ל-lookup

**הערכת זמן:** 4-6 שבועות

---

## שלב 3 — Rendering Pipeline: AKML + Layout + Compositor

**מטרה:** מערכת הרנדור המלאה. נותנים widget tree → יוצא draw commands. AKML לפיענוח.

**רכיבים שנבנים:**

### 3.1 AKML Parser
- `framework/akml/parser.rs`
- XML tokenizer → AST
- תמיכה ב-attributes: text, padding, background, flex, onClick, tag, id, וכו'
- Resource reference resolution (`@string/foo`, `@color/bar`, etc.)
- הצהרת id: `@+id/name`

### 3.2 LayoutInflater
- `framework/akml/inflater.rs`
- AKML AST + ResourceManager → WidgetTree
- רישום callbacks בטבלת האפליקציה
- Error handling עם location info

### 3.3 Widget types (מצומצם)
- `framework/widgets/`
- `Text`, `Button`, `Image`, `Rect`
- Containers: `VStack`, `HStack`, `Grid`
- כל widget יודע: measure(constraints), layout(position)

### 3.4 LayoutEngine
- `framework/layout/`
- Measure pass (bottom-up)
- Layout pass (top-down)
- תמיכה ב-dp/sp units, percentage, flex

### 3.5 Compositor
- `framework/compositor/`
- WidgetTree (after layout) → Vec<DrawCommand>
- DrawCommands: `FillRect`, `FillText`, `FillImage`, `SetClip`, וכו'
- שליחה ל-HAL דרך IPC

### 3.6 Frame Loop
- 60fps טיימר ב-Scheduler
- Dirty tracking: אם widget השתנה → re-layout → re-composite
- אם לא השתנה → skip frame

**Exit criteria:**
- נתונה קובץ AKML ידני (test fixture) — המערכת מציירת אותו
- טקסט נראה, padding מכובד, layout של VStack/Grid עובד
- RTL text direction מכובד

**הערכת זמן:** 6-8 שבועות (זה השלב הכבד)

---

## שלב 4 — ABI (WIT) + SDK + First Real App

**מטרה:** אפליקציה אמיתית (Rust → wasm component) נטענת ע"י ה-runtime, מציגה layout, מגיבה לקליק.

**רכיבים שנבנים:**

### 4.1 WIT definitions
- `wit/akiva.wit` — מימוש של כל ה-interfaces שב-[docs/abi.md](docs/abi.md): lifecycle, events, widget, resources, intent, activity-api, storage, log, system
- `wit-bindgen` מפיק bindings ל-kernel (host side) ול-SDK (guest side)
- ה-kernel מממש את ה-`host` world; apps מיישמות את ה-`guest` world

### 4.2 Component loader
- `kernel/src/wasm_loader.rs`
- טוען wasm component (מכיל `component-type` section)
- בונה import map לפי capability set של ה-app (ראה `ARCHITECTURE.md §8`)
- Wasmtime-style instantiation (דרך `wasm-tools component` או `jco`-style polyfill ב-host)
- ניהול lifecycle של ה-instance

### 4.3 SDK ב-Rust
- `sdk-rust/Cargo.toml`
- `sdk-rust/src/lib.rs` — prelude
- `sdk-rust/src/app.rs` — `trait App`, `#[akiva::app]` macro
- `sdk-rust/src/activity.rs` — `ActivityContext`, navigation APIs
- `sdk-rust/src/callbacks.rs` — callback registry, event dispatch
- `sdk-rust/src/resources.rs` — `@string/`, `@color/` helpers
- `sdk-rust/src/storage.rs` — key-value wrapper
- `sdk-rust/src/log.rs` — logging macros

### 4.4 אפליקציה ראשונה: "Hello Akiva"
- `apps/hello/manifest.toml`
- `apps/hello/res/layout/main.akml`
- `apps/hello/res/values/strings.toml`
- `apps/hello/src/lib.rs` — מציג טקסט + כפתור שמעלה counter

**Exit criteria:**
- Hello app נטען אוטומטית ב-boot אחרי הוספה ל-`apps/`
- Shell (ראה שלב 5) יכולה לפתוח אותה
- Counter עובד: click → state update → re-render

**הערכת זמן:** 3-4 שבועות

---

## שלב 5 — Shell App

**מטרה:** אפליקציית בית שמציגה אפליקציות מותקנות ומאפשרת לפתוח אותן.

**רכיבים שנבנים:**

### 5.1 Shell codebase
- `apps/shell/manifest.toml` — עם `intent_filter: category=HOME`
- `apps/shell/res/layout/home.akml` — Grid של icons
- `apps/shell/src/lib.rs`
  - `on_create`: קורא `PackageManager.list_apps()`
  - מציג Grid של buttons, כל button עם icon + label
  - `on_app_click`: שולח `Intent(action=MAIN, target=app_id)`

### 5.2 השלמות Framework
- Boot flow: auto-launch של Shell
- Intent resolution מלאה
- Task stack: back button חוזר ל-Shell

**Exit criteria:**
- ב-boot רואים את Shell עם רשימת apps
- לחיצה על app פותחת אותה
- Back חוזר ל-Shell

**הערכת זמן:** 2 שבועות

---

## שלב 6 — Calculator (אפליקציית הדגל)

**מטרה:** מחשבון פונקציונלי מלא, כתוב ב-Rust, רץ ב-Akiva OS דרך כל ה-stack.

**רכיבים שנבנים:**

### 6.1 Calculator codebase
- `apps/calculator/manifest.toml`
- `apps/calculator/res/layout/main.akml` — Grid של כפתורים + display
- `apps/calculator/res/layout-land/main.akml` — גרסה אופקית (הדגמה של qualifier!)
- `apps/calculator/res/values/strings.toml`
- `apps/calculator/res/values-he/strings.toml` — תרגום לעברית
- `apps/calculator/res/values/colors.toml`
- `apps/calculator/res/values-night/colors.toml` — dark mode
- `apps/calculator/src/lib.rs`
  - State: display string, operand, operation
  - Callbacks: `on_digit(tag)`, `on_op(tag)`, `on_equals`, `on_clear`
  - Logic: + − × ÷ =
  - `on_save_state` / restore

**Exit criteria:**
- משתמש יכול לבצע חישובים (2 + 3 = 5, 10 × 0.5 = 5, וכו')
- סיבוב orientation (גם אם מזויף ע"י dev tool) טוען layout אחר
- שינוי locale ל-he → strings בעברית
- שינוי dark mode → צבעים מתחלפים
- kill של App → relaunch משחזר display value

**הערכת זמן:** 2-3 שבועות

---

## שלב 7 — Dev Tooling: CLI

**מטרה:** חווית פיתוח סבירה — `akiva new`, `akiva build`, `akiva run`.

**רכיבים שנבנים:**

### 7.1 CLI binary
- `cli/Cargo.toml` (Rust-native, לא wasm)
- `cli/src/main.rs` עם clap / argh
- פקודות:
  - `akiva new <name>` — יוצר template app
  - `akiva build` — `cargo build --target wasm32 --release` + optional wasm-opt
  - `akiva run` — מרים dev server (axum/warp) שמגיש את runtime + apps
  - `akiva validate` — בודק manifest + AKML
  - `akiva install <path>` — מכניס app ל-storage המקומי

### 7.2 Dev server features
- Static file serving של `runtime/web/`
- Dynamic של `apps/`
- WebSocket למסר "app changed → reload"
- לא hot reload אמיתי ב-v0.1, אבל notification שיש לטעון מחדש

**Exit criteria:**
- `akiva new foo` → `akiva build` → `akiva run` עובד end-to-end
- App חדשה מופיעה ב-Shell

**הערכת זמן:** 2-3 שבועות

---

## סיכום זמנים

| שלב | תיאור | זמן |
|---|---|---|
| 0  | תכנון (docs) | הושלם |
| 1a | Wasm hello world ב-browser | 1 שבוע |
| 1b | HAL + Leader Election + Multi-tab | 2 שבועות |
| 2  | Framework managers | 4-6 שבועות |
| 3  | AKML + Layout + Compositor | 6-8 שבועות |
| 4  | ABI (WIT) + SDK + hello app | 3-4 שבועות |
| 5  | Shell | 2 שבועות |
| 6  | Calculator | 2-3 שבועות |
| 7  | CLI | 2-3 שבועות |

**סה"כ MVP: ~5-7 חודשים** עבודה ממוקדת של מפתח יחיד.

לא מהיר. אבל ב-סוף השלב הזה יש לנו **OS אמיתית עם אפליקציה אמיתית**, לא demo. זה משהו שאפשר להראות, להסביר, ולהרחיב.

---

## מה *לא* ב-MVP (רשימה מכוונת)

רזה **בעומק**, לא בשלמות:

- ❌ Animations (רק transitions מיידיים)
- ❌ Multi-window / split screen (activity אחד display אחד)
- ❌ Intent chooser UI (אם יש matches מרובים — קחי את הראשון)
- ❌ App store / distribution (קבצים לוקליים בלבד)
- ❌ Network APIs (לא נחשף ל-apps עדיין)
- ❌ Accessibility (יוסף ב-v0.2, אבל הארכיטקטורה מוכנה)
- ❌ Themes (צבעים בלבד, לא styles)
- ❌ Sophisticated layouts (רק VStack/HStack/Grid)
- ❌ SVG / JPG drawables (PNG בלבד — ראה `docs/resources.md §8`)
- ❌ WebGPU (Canvas 2D דרך abstract `RendererBackend` — החלפה קלה post-MVP)
- ❌ Hebrew / RTL / BiDi text rendering (Latin-only `fillText` ב-MVP; HarfBuzz + FriBidi ב-v0.3+)
- ❌ IME / virtual keyboard לעברית
- ❌ Multi-threading ב-app (wasm single-threaded)
- ❌ Inter-app storage sharing
- ❌ Hot reload אמיתי
- ❌ Debugger / inspector
- ❌ PendingIntent + Broadcast receivers (ראה `docs/intents.md §13-14`)
- ❌ Notifications מלאים (רק toasts — ראה `docs/notifications.md`)
- ❌ SDKs ל-non-Rust (Kotlin/Dart/Go post-MVP — ראה "מפת דרכים ל-SDKs" למטה)

**כל אחד מאלה יכול להתווסף ב-v0.2+ בלי לשכתב מהבסיס.**

---

## מפת דרכים ל-SDKs

ה-ABI מבוסס **WIT / Component Model**, מה שפותח את הדרך ל-SDKs בשפות נוספות בלי שינוי ב-runtime.

| SDK | זמן | הערה |
|---|---|---|
| Rust (MVP) | שלב 4 | `wit-bindgen` מייצר bindings אוטומטית |
| C / C++ (v0.3) | post-MVP | `wit-bindgen c` — tooling בשל |
| Go (v0.3) | post-MVP | TinyGo + `wit-bindgen` (או רכיב חדש של Go upstream) |
| Kotlin / Dart (v0.4) | post-MVP | תלוי ב-WasmGC maturity — נכון ל-2026 WasmGC ב-browsers stable; binary sizes קטנים משמעותית מ-Go |
| Python (v0.5) | post-MVP | Componentize-py; מתאים לאפליקציות data-heavy |
| JavaScript / TypeScript | לא מתוכנן | מסוג שאנחנו מנסים להתרחק ממנו בכוונה |

---

## תלויות סביבתיות

לפני שמתחילים:
- `rustc` + `cargo` (rustup) — nightly ב-MVP (עבור `target-wasm` ו-`wit-bindgen` latest)
- `wasm32-unknown-unknown` target
- `wasm-tools` (`cargo install wasm-tools`) — לבחינת components ו-WIT
- `wit-bindgen-cli` (`cargo install wit-bindgen-cli`) — לייצור bindings ל-Rust מה-WIT
- `wasm-opt` (מ-binaryen) ל-production builds
- Node.js 20+ (Playwright ב-CI; אופציונלי dev server כ-axum ב-Rust)
- Playwright (`npm i -D @playwright/test`) ל-E2E
- דפדפן: Chrome ≥ 119, Firefox ≥ 120, Safari ≥ 18.2 (בגלל WasmGC + Web Locks + OPFS SyncAccessHandle)
