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

**סטטוס (אפריל 2026):**
- 🟢 **Phase 2a seed** הושלם: הframework מחובר ל-kernel, Activity state machine אמיתי (Initial→Created→Started→Resumed→Paused→Stopped→Destroyed עם אכיפת מעברים חוקיים), task stack, launch modes Standard + SingleTop, Shell system activity שמצייר banner + pointer indicator דרך ה-compositor. הinput/render flow כעת עובר framework→activity. ראה [framework/src/activity.rs](framework/src/activity.rs), [framework/src/activity_manager.rs](framework/src/activity_manager.rs), [framework/src/shell.rs](framework/src/shell.rs).
- 🟢 **Phase 2b step 1** הושלם: PackageManager עם persistence אמיתית. Manifest parser מלא ([framework/src/manifest.rs](framework/src/manifest.rs)) מפרסר ומוודא את ה-schema מ-[docs/manifest.md](docs/manifest.md) — reverse-domain id, semver, entry activity, duplicate-id, intent filters, launch modes. PackageManager שומר installed apps כ-`packages/{id}/manifest.toml` + `packages/{id}/app.wasm` עם index ב-`sys/packages/index`; rehydrates at boot דרך `framework::boot(boot_count, &mut dyn Storage)`. `framework::storage::Storage` trait מפריד בין framework לבין kernel storage, `KernelStorage` ב-[kernel/src/lib.rs](kernel/src/lib.rs) מאחסן את הגשר. חבילה פגומה מדווחת דרך `BootReport::package_errors` ולא מפילה את ה-boot.
- 🟢 **Phase 2b step 2** הושלם: App ABI v0 contract. ה-`WasmHost` trait ב-[framework/src/wasm_activity.rs](framework/src/wasm_activity.rs) + `WasmActivity` proxy שמיישם את ה-`Activity` trait הפנימי ע"י העברת כל lifecycle/input/render ל-host; קודק binary מלא ל-paint ops (`encode_op` / `decode_frame`, 7 opcodes); `hal_app_*` externs מוגדרים ב-[kernel/src/host.rs](kernel/src/host.rs) + `KernelWasmHost` adapter ב-[kernel/src/lib.rs](kernel/src/lib.rs); stubs ב-[runtime/web/worker.js](runtime/web/worker.js) שמחזירים 0/no-op. החוזה ל-apps מתועד ב-[docs/abi.md §2](docs/abi.md#2-app-abi-v0--raw-exports-phase-2b).
- 🟢 **Phase 2b step 3** הושלם: host-side wasm instantiation + framework wire-up. [runtime/web/app_registry.js](runtime/web/app_registry.js) מחזיק `handle → slot` + `packageId → process` — טוען `packages/*/app.wasm` מ-OPFS, מקמפל דרך `new WebAssembly.Module` (סינכרוני ב-worker), ומנתב את כל ה-`hal_app_*` ל-`akiva_app_*` של ה-instance הנכון; `akiva_host_log` חשוף כ-import עם prefix per-package. `KernelWasmHost` ב-[kernel/src/lib.rs](kernel/src/lib.rs) הופעל ומועבר כ-`Rc<dyn WasmHost>` ל-[framework/src/lib.rs](framework/src/lib.rs) — שם `boot` רושם `WasmActivity` factory לכל entry activity של package מותקן. ההפצה של [docs/abi.md §2.3](docs/abi.md#23-imports-host--app) עוברת לציין את ה-imports הקיימים בפועל.
- 🟢 **Phase 2b step 4** הושלם: סיבוב סגור מ-user app ל-canvas. [sdk-rust](sdk-rust) מחזיק עכשיו `Activity` trait מלא (lifecycle + input + render), `Canvas` encoder שכותב ישירות לפורמט הwire של v0, `akiva_host_log` wrapper עם מאקרו `log_info!` etc, ו-מאקרו `akiva_app!(MyType)` שמייצר את כל 14 ה-`#[no_mangle]` extern wrappers. אפליקציית [apps/hello](apps/hello) הראשונה מודגמת — `HelloMain` מצייר title, frame counter, ו-pointer reticle. [apps/hello/build.sh](apps/hello/build.sh) מקמפל ומעתיק ל-[runtime/web/dev-apps/hello/](runtime/web/dev-apps/hello/). worker.js מריץ `installDevApps()` לפני `akiva_boot` כדי להעביר manifest+wasm לכל חבילה חדשה אל OPFS. ב-`framework::boot` נוסף auto-launch זמני של ה-user package הראשון מעל ה-Shell (יוחלף ב-Phase 2b.5 ב-UI של Shell).
- 🟢 **Phase 2b step 5** הושלם: Shell launcher + activity back-stack. Shell מרנדר רשימת כפתורים לכל חבילה מותקנת ([framework/src/shell.rs](framework/src/shell.rs)); לחיצה דוחפת את האפליקציה על גבי stack חדש ומעבירה את Shell ל-`Paused`; Escape מטיל top → destroy ומחזיר את הקודם ל-`Resumed`. [framework/src/activity_manager.rs](framework/src/activity_manager.rs) קיבל `pause()` driver + תיקון ל-`resume()` שיעבוד גם מ-Paused→Resumed. `ActivityContext` קיבל out-param `launch_requested` (באותו דפוס כמו `finish_requested`); הFramework מרכז את כל ה-dispatch דרך `with_ctx` + `apply_post` שמנקזים את הבקשות אחרי ה-callback. ה-auto-launch מ-Phase 2b.4 הוסר — Shell בתחתית תמיד, אפליקציות משתמש מעליו.

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

> **החלטה ארכיטקטונית (אפריל 2026):** ה-widget toolkit, layout engine, ו-AKML inflater חיים ב-**SDK** (`sdk-rust/src/ui` + `sdk-rust/src/akml`), לא ב-framework. ה-ABI נשאר `PaintOp` בלבד. ראה [ARCHITECTURE.md §2.1](ARCHITECTURE.md#21-abi-boundary--למה-widgets-ו-akml-חיים-ב-sdk-ולא-ב-framework) לרציונל המלא. ה-"§3.3 Widget types" ו-"§3.4 LayoutEngine" למטה מתארים מה קרה — רק שהמיקום הפיזי שונה ממה שמופיע ב-"רכיבים שנבנים".

**סטטוס (אפריל 2026):**
- 🟢 **Phase 3a** הושלם: widget toolkit app-side ב-SDK. [sdk-rust/src/ui/](sdk-rust/src/ui/) מחזיק `UiState` (app-owned, שומר hit rects + pending clicks בין frames), `Ui` builder עם `.text()/.rect()/.spacer()/.button()/.v_stack()/.h_stack()` וצ'יינינג `.size()/.color()/.gap()/.padding()/.build()`, שני-pass layout (measure bottom-up + place top-down), paint emitter שמפיק canvas ops בסדר document, וclick detection דרך `was_clicked(id)`. **Text metrics** מגיעים מ-host דרך import חדש `akiva_host_text_width` (OffscreenCanvas `measureText` ב-worker), עם fallback ל-`chars × 0.55 × size` ב-cargo test. **C1 — יישור צירים**: `MainAxis::{Start, Center, End, SpaceBetween}` ו-`CrossAxis::{Start, Center, End, Stretch}` דרך `.main()/.cross()` על `StackBuilder`; ברירת מחדל `Start/Start` זהה ביט-לביט לפני-C1. **C2 — Grow לכל ילד**: `.grow(factor)` על כל builder + `Ui::grow(id, factor)` post-hoc לכפתורים; חלוקת leftover בסגנון flexbox (`base + leftover × grow/total_grow`), grow על stack מקונן מתפשט כ-stretch לילד. העיצוב: widgets הם SDK concern — ה-ABI נשאר paint-ops, הframework לא יודע כלום. [apps/hello](apps/hello) משתמש ב-widgets (כפתורי `−1 / +1 / Reset` עם counter). **44 unit tests** מכסים measure/place/alignment/grow/click-detection.

- 🟢 **Phase 3b stone 1** הושלם: **AKML parser + inflater** ([sdk-rust/src/akml/parser.rs](sdk-rust/src/akml/parser.rs), [sdk-rust/src/akml/inflate.rs](sdk-rust/src/akml/inflate.rs)). tokenizer XML-ish משלנו (`<VStack>`, `<Button label="…" onClick="…">`, self-closing, comments), `Element` AST עם `Loc { line, col }` להודעות שגיאה, ו-inflater שהופך AST ל-`Ui` node ids. תומך ב-`VStack`, `HStack`, `Frame`, `Text`, `Button`, `Rect`, `Spacer` עם attributes `id`, `gap`, `padding` (`all` / `v h` / `top right bottom left`), `main`, `cross`, `flex`, `size`, `color` (`#RGB`/`#RRGGBB`/`#AARRGGBB`/`0xAARRGGBB`), `text`, `label`, `min_width`, `min_height`, `onClick`.

- 🟢 **Phase 3b stone 2** הושלם: **bindings** (`{name}` substitution). [sdk-rust/src/akml/bindings.rs](sdk-rust/src/akml/bindings.rs) חושף `Bindings::new().text("name", val).color("name", u32)`, וה-inflater מבצע substitution בזמן attribute parse. בעיית UTF-8 ב-attribute parser נתפסה ותוקנה (commit `bb9a626`).

- 🟢 **Phase 3b stone 3** הושלם: [apps/calculator](apps/calculator) עבר כולו ל-AKML ([apps/calculator/res/layout/main.akml](apps/calculator/res/layout/main.akml)). ה-Rust מחזיק רק את ה-state machine + dispatch; ה-layout הוא קובץ נפרד embedded ב-`include_str!`.

- 🟢 **Phase 3b stone 4 (Frame widget)** הושלם: `<Frame>` — z-stack widget שמוריש את ה-inner rect שלו לכל הילדים (stretch בשני הצירים). [sdk-rust/src/ui/node.rs](sdk-rust/src/ui/node.rs), layout + paint ב-[sdk-rust/src/ui/layout.rs](sdk-rust/src/ui/layout.rs) + [sdk-rust/src/ui/paint.rs](sdk-rust/src/ui/paint.rs). 5 layout tests + 2 inflater tests. שימוש ב-[apps/calculator](apps/calculator): display panel עם `<Rect>` ברקע + `<HStack>` של טקסט מעליו.

- 🟢 **Phase 3b stone 5 (onClick / was_triggered)** הושלם: decoupling של widget ids מ-semantic actions. `<Button onClick="equals">` + `ui.was_triggered("equals")` — פרוטוקול pull-based, לא closure registry. שלוש סיבות: (1) closures מעל `&mut self` מעלים borrow checker errors; (2) drain-after-present טריוויאלית בטוחה ב-Rust; (3) מאפשר renaming של id ל-"eq"/"equal_btn" בלי לגעת ב-Rust. 4 unit tests + 2 inflater tests.

- 🟢 **תיקון Platform** הושלם (commit `24d320e`): שני באגים שחשפו פערים מתחת ל-Phase 3b. (1) [framework/src/wasm_activity.rs](framework/src/wasm_activity.rs) היה מעביר `Create` lifecycle לפני שה-surface metrics התעדכנו — `on_create` של ה-app ראה `Metrics::default()` (0x0), שהתייצבה רק עם window resize. תוקן: `WasmActivity::on_create` יורה resize מיד אחרי Create. (2) [runtime/web/worker.js](runtime/web/worker.js) `postEvent` היה מקבל events בזמן async `bootKernel` לפני שה-wasm instance קיים → `TypeError`. תוקן: guard `if (!booted || !instance) return;`.

**מה עדיין דחוי מ-Phase 3** (לא טופל ב-stones 1–5):
- Grid widget, Image widget, Input widget
- Attributes: `margin`, `background`, `visible`, `textAlign`, `fontWeight`, `tag`
- `dp`/`sp`/`%` suffixes, `match_parent`/`wrap_content`
- Resource references (`@string/`, `@color/`, `@dimen/`, `@style/`, `@drawable/`) — דורש ResourceManager wired
- RTL direction (מוחרג מפורש מ-MVP)

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

## שלב 4 — ABI + SDK + First Real App

**סטטוס (אפריל 2026):**
- 🟢 **App ABI v0 (raw exports)** הושלם במקום WIT — ראה [docs/abi.md §2](docs/abi.md). ה-app חושפת 14 `#[no_mangle] extern "C"` exports (`akiva_app_create`/`_resize`/`_pointer`/`_key`/`_render`/`_start`/`_resume`/`_pause`/`_stop`/`_destroy`/`_alloc`/`_free`), ה-host מייבא דרך `akiva_host_log` + `akiva_host_text_width`. [framework/src/wasm_activity.rs](framework/src/wasm_activity.rs) מציג `WasmHost` trait ו-`WasmActivity` proxy; [kernel/src/host.rs](kernel/src/host.rs) + [runtime/web/app_registry.js](runtime/web/app_registry.js) מחברים את זה ל-JS. ה-SDK מקבל [sdk-rust/src/activity.rs](sdk-rust/src/activity.rs) עם `Activity` trait מלא, `Canvas` encoder, log macros, ומאקרו `akiva_app!(Type)` שמייצר את כל ה-14 wrappers. אפליקציית [apps/hello](apps/hello) רצה end-to-end.

> **WIT / Component Model — דחוי ל-post-MVP:** התכנית המקורית הייתה לייצר `wit/akiva.wit` ולהשתמש ב-`wit-bindgen` ל-host + SDK. בפועל בחרנו raw exports כי (1) תמיכת Component Model ב-browser עדיין לא בשלה ב-2026 (jco / wasm-tools component polyfills נדרשים, מסובך ב-build), (2) MVP הוא Rust-only ממילא — WIT הייתה קונה אותנו polyglot SDKs שאינם יעד של MVP, (3) raw exports פשוטים יותר לצפייה ו-debug. מעבר ל-WIT יהיה breaking ABI change שיבוצע כש-Kotlin/Go/Python SDKs ייכנסו לתמונה (ראה "מפת דרכים ל-SDKs" למטה).

**מטרה (post-MVP):** אפליקציה אמיתית (Rust → wasm component) נטענת ע"י ה-runtime, מציגה layout, מגיבה לקליק.

**רכיבים שנבנים (post-MVP):**

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

**סטטוס (אפריל 2026):**
- 🟡 **Shell כ-built-in של framework** (לא `apps/shell/`) — [framework/src/shell.rs](framework/src/shell.rs) מממש `ShellActivity` שנרשם ב-`framework::boot` עם `package_id = "sys.shell"`. ה-UI מרונדר ידנית על `PaintTarget` (לא דרך AKML) ומציג header + רשימת כפתורי launcher לכל package. לחיצה → `ctx.launch_requested = Some(LaunchRequest { package_id, class })` → framework דוחף את ה-app על ה-stack ו-pause את Shell. Escape בראש stack של יותר מ-1 → pop + resume Shell.

> **הימרה של MVP — לא apps/shell:** ROADMAP מייעד את Shell כ-app רגיל עם `intent_filter: category=HOME`. בחרנו לממש אותו built-in כי (1) זה פותר bootstrap (אין דילמה של "מי מפעיל את ה-shell לפני שה-PackageManager מזהה את ה-shell"), (2) אין עדיין IntentRouter wired (intent resolution לא נקרא ב-launch path — ראה Phase 2 deferred), (3) AKML לא היה קיים כשה-Shell נכתב. הוצאה ל-`apps/shell/` כ-wasm package דוחה ל-post-MVP — אחרי ש-IntentRouter מחובר בפועל וה-Shell יכולה להשתמש ב-AKML כמו כל app אחרת.

**מטרה (post-MVP):** אפליקציית בית שמציגה אפליקציות מותקנות ומאפשרת לפתוח אותן.

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

**סטטוס (אפריל 2026):**
- 🟢 **Calculator preview** הושלם מעל Phase 3a: [apps/calculator](apps/calculator) עם state machine נקי (display, accumulator, pending op, just_computed, error) ו-UI שבנוי כולו על ה-widget toolkit (`MainAxis::End` + `CrossAxis::Center` לתצוגה ימנית-מיושרת, `CrossAxis::Stretch` + `grow=1` לתאים שווים בשבתי 4×5). 19 unit tests מכסים את כל מכונת-המצבים (ספרות, נקודה, backspace, ±, %, שרשור אופרטורים, חלוקה באפס, אורך-קלט מקסימלי, פורמוט f64). רשום ב-DEV_APPS ומתקמפל ל-104KB wasm. **מה חסר כדי להגיע ל-Phase 6 המלא:** AKML (Phase 3b) במקום tree של widgets ב-Rust, `res/` עם strings/colors/qualifier-aware layouts, `on_save_state`/restore, ומחוות landscape. כל אלה מחכים ל-Phase 3b ול-state-save.

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

# v0.2 — מ-POC למערכת הפעלה

v0.1 סגר את ה-MVP: kernel רץ, שתי אפליקציות אמיתיות, CLI מלא, SavedState. v0.2 הופך את השלד הזה למערכת הפעלה אמיתית — חוזה ברור בין המערכת לאפליקציות, בלי hacks דמויי-פיגום.

**היקף ומטרה:** ראה [`docs/v0.2-design.md`](docs/v0.2-design.md) למפרט המלא, כולל gap map מפורט לכל עמוד, definition of done, ורציונל הסדר.

**שלוש phases, סדר חובה (1 → 2 → 3):**

---

## שלב 8 — Package Manager אמיתי

**מטרה:** פורמט הפצה אמיתי, install/uninstall דרך CLI, מחיקת ה-`DEV_APPS` hack.

**רכיבים שנבנים:**

### 8.1 פורמט `.akv` archive
- תיעוד ב-`docs/archive-format.md` (חדש)
- ZIP עם layout קבוע: `manifest.toml` + `app.wasm` + `res/` + `signature.sig` (עתידי, placeholder בלבד)
- בחירת crate: `zip` (stdlib-friendly, ללא dependency-hell) או מימוש פנימי אם ה-crate דורש תלות כבדה
- CLI utility פנימי: `Archive::create(app_dir) -> bytes`, `Archive::extract(bytes) -> {manifest, wasm, res}`

### 8.2 `akiva package <app>`
- ב-[`cli/src/package.rs`](cli/src/package.rs) (חדש)
- שלבים: ודא build קיים (או `akiva build <app>` ראשון), קרא wasm מ-`runtime/web/dev-apps/<app>/` או ישירות מ-`apps/<app>/target/...`, קרא manifest, קרא `res/`, ארוז → `<app>.akv`
- Flag: `--out <path>` (ברירת מחדל: `<app>.akv` ב-cwd)

### 8.3 `akiva install <archive>`
- ב-[`cli/src/install.rs`](cli/src/install.rs) (חדש)
- זה CLI-side install לא browser-side — כדי לעבוד בלי browser פתוח, ה-CLI יעתיק את ה-archive ל-`runtime/web/dev-apps/<id>/` (או לשים אותו במקום שה-worker יודע להרים ב-boot הבא)
- v0.2 stone 1: ה-CLI עדיין מפעיל את הזרימה הישנה (פרישה ל-`dev-apps/`), אבל *דרך* קריאה ל-`PackageManager::install` של ה-framework ב-unit שרץ build-time — לאמת שה-API עובד
- v0.2 stone 2: worker.js מוחק את `installDevApps` לטובת סריקת `runtime/web/installed/` או פקודה ב-zerver שמדחיפה `PackageManager::install` בקרנל

### 8.4 Shell discovery מהאינדקס
- [`runtime/web/worker.js`](runtime/web/worker.js): מחיקת `const DEV_APPS = [...]` ו-`installDevApps`
- Shell קוראת `PackageManager::list()` ומציגה כל מה שמותקן (ללא תיקונים נוספים)
- המבחן: התקנת archive חדש מה-CLI → refresh דפדפן → האפליקציה מופיעה ב-Shell בלי לגעת ב-JS

### 8.5 Upgrade + uninstall
- `PackageManager::install` מזהה `version_code` חדש, מחליף in-place (כולל resources ו-saved_state של activities — או שמור `saved_state` אם אותה major version, ראה `manifest.md §6`)
- `akiva uninstall <package_id>` — CLI command חדש; קורא ל-`PackageManager::uninstall` + מוחק גם grants (כשעמוד 3 ינחת)
- `version_code` זהה או נמוך יותר בהתקנה → dismissal עם הודעה ברורה

**Exit criteria:**
- [ ] `akiva package calculator && akiva install calculator.akv` עובד end-to-end
- [ ] `runtime/web/worker.js` לא מכיל שום שם של אפליקציה ספציפית
- [ ] התקנת archive חדש, ה-Shell מציגה את האפליקציה בלי שינוי קוד
- [ ] upgrade + uninstall עובדים (unit tests ב-`framework/src/package_manager.rs`)

**הערכת זמן:** 2-3 שבועות.

---

## שלב 9 — Process / Activity model מלא

**מטרה:** מחזור חיים מלא, כל launch modes, back navigation, cross-activity intents עם תוצאות.

**תלוי ב:** שלב 8 (אפשר להתחיל stones 9.1 ו-9.2 במקביל לסוף של 8, אבל 9.3+ דורשים install אמיתי כי בלעדיו אי אפשר לבדוק cross-package intents באמת).

**רכיבים שנבנים:**

### 9.1 מחזור חיים מלא
- לוודא ש-`WasmActivity` משדרת **כל** ה-5 callbacks (onCreate/onStart/onResume/onPause/onStop/onDestroy) דרך wasm exports
- `akiva_app_start` / `_pause` / `_stop` / `_destroy` — אם לא קיימים ב-ABI, מוסיפים
- Unit tests ב-`framework/src/activity_manager.rs` על מעבר כל edge; calculator מוסיפה log לכל callback וזה מאומת ידנית
- עדכון [`docs/activity-model.md §9`](docs/activity-model.md) עם טבלת "Implementation" כמו ב-`manifest.md`

### 9.2 Launch modes: SingleTask + SingleInstance
- `ActivityManager::launch` מסיר את ה-`Unsupported` ומיישם:
  - **SingleTask:** אם instance כבר קיים ב-task (לאו דווקא top) — pop כל מה שמעליו → resume אותו. אם לא ב-task — יוצר instance חדש.
  - **SingleInstance:** יש task נפרד משלו; `launch` לפני שהוא קיים יוצר task חדש, בתחום אחר activities; פעולות נוספות באותו package פותחות ב-task הקודם שלהן.
- Unit tests לכל תרחיש ב-[`activity-model.md §4`](docs/activity-model.md)

### 9.3 Back stack + `system.back()`
- Import חדש `system.back()` ב-ABI: אפליקציה יכולה להפעיל בעצמה, אבל במיוחד — הפלטפורמה תשתמש בזה על mapping של keyboard/gesture (v0.2: קיצור מקלדת fixed, אין gesture engine)
- ב-framework: `ActivityManager::back()` → pop top → שולח destroy → מביא את הקודם ל-Resumed
- ה-Shell משתמשת ב-`system.back()` כ-mechanism העיקרי למעבר בין tasks
- **SDK `Context::finish()` plumbing:** חוטים ל-`akiva_host_finish` import; ב-`WasmActivity` dispatch ה-host יקבע `ActivityContext.finish_requested = true` לפני שה-callback מחזיר. זה סוגר את ה-gap שזוהה ב-[`research-activity-boundary.md`](docs/research-activity-boundary.md) — היום אין לאפליקציית wasm דרך לסיים את עצמה. רשום גם `Context::back()` לצד `finish()` לאותו frame של העבודה.
- **Created→Destroyed shortcut:** אם activity קוראת `finish()` מתוך `on_create`, skip על `on_start`/`on_resume`/`on_pause`/`on_stop` וקפוץ ישר ל-`on_destroy`. מוסיף edge `(Created, Destroyed)` ל-`legal_transition` + בדיקה ב-`advance_through_create` אם `finish_requested` הופעל. parity עם Android — ראה [`research-android-lifecycle.md §1`](docs/research-android-lifecycle.md)

### 9.4 `start_activity` מתוך app + `on_new_intent`
- Import חדש `activity.start_activity(intent_blob: bytes)` (grant: `LAUNCH_ANY_APP` ל-Shell, או target-package-matches-self ל-self-launch)
- IntentRouter מפסירס את ה-blob, פותר activity, מפעיל `ActivityManager::launch` עם stack נכון
- SingleTop/SingleTask שמוצאים instance קיים מפעילים export חדש `akiva_app_new_intent(intent_blob)` — הוסף ל-ABI
- unit test: A פותח B, B פותח C, back → back → back → Shell
- **Doc cleanup (🗑️):** בסיום ה-stone למחוק `docs/v0.2-review.md`, `docs/research-android-lifecycle.md`, `docs/research-activity-boundary.md`. ל-fold כל תוכן שעדיין רלוונטי ל-`activity-model.md` / `abi.md`. לעדכן את 6 ה-cross-links שמפנים אליהם. `research-component-model.md` ו-`test-coverage.md` נשארים.

### 9.5 `start_activity_for_result` + `on_activity_result`
- שני imports חדשים: `activity.start_activity_for_result(intent_blob, request_code)` ו-`activity.set_result(result_blob)`
- ה-framework מתעד ב-stack את ה-`request_code` של ה-caller, וכש-B קוראת `set_result` + מסיימת, callback `akiva_app_activity_result(request_code, result_blob)` נקרא ל-A
- Demo: hello פותחת calculator "ל-pick" מספר, calculator מחזירה value

### 9.6 Multiple tasks לפי affinity
- `ActivityManager` כבר מחזיק `tasks: HashMap<TaskId, Task>` ו-`Task.affinity` — חסרה ההחלטה של foreground
- הוסף `bring_task_to_front(task_id)`; Shell יכולה לעבור בין tasks כשמשתמש בוחר app שכבר רץ
- Unit test: A ב-task 1, B ב-task 2 (affinity שונה), swap ביניהם שומר על ה-stack של כל אחד

**Exit criteria:**
- [ ] כל ארבעת ה-launch modes עוברים acceptance tests שמותווים ב-[`activity-model.md §4`](docs/activity-model.md)
- [ ] 5 ה-lifecycle callbacks כולם נקראים ב-apps (מאומת ידנית + unit tests)
- [ ] `start_activity_for_result` demo (hello ↔ calculator) עובד end-to-end
- [ ] back navigation עובד ב-stack של לפחות 3 activities
- [ ] tasks מרובים עם affinity שונה — יש unit test המדגים swap

**הערכת זמן:** 3-4 שבועות.

---

## שלב 10 — Permissions (Capability-via-Imports)

**מטרה:** אפליקציה רואה רק את ה-imports שהמניפסט שלה הצהיר עליהם, ה-PackageManager שומר grants, הקישור נכשל אם אפליקציה מנסה לקרוא להרשאה שאין לה.

**תלוי ב:** שלב 8 (install flow שקורא permissions), שלב 9 (imports ספציפיים-להרשאה כמו `LAUNCH_ANY_APP` קיימים רק אחרי 9.4-9.5).

**רכיבים שנבנים:**

### 10.1 System permission registry
- ב-framework boot: `PermissionController::declare` לכל אחת מההרשאות בטבלה ב-[`ARCHITECTURE.md §8`](ARCHITECTURE.md):
  `STORAGE_SANDBOX`, `QUERY_PACKAGES`, `LAUNCH_ANY_APP`, `NOTIFICATIONS`, `NETWORK`, `CAMERA`
- כל הצהרה כוללת `ProtectionLevel`; ב-v0.2 כולן `Normal` (auto-granted install-time)

### 10.2 Install-time grant
- `PackageManager::install` קוראת לאחר parse ל-`grant_requested_permissions(manifest)`
- כל הרשאה ב-`permissions.required` → check שהיא מוצהרת במערכת → `PermissionController::grant`
- הרשאה לא מוכרת → `InstallError::UnknownPermission(name)`
- Persistence: grants נכתבים ל-`sys/permissions/{package_id}` (LF-separated); ב-`load_installed` הם נטענים בחזרה

### 10.3 Import map לפי capabilities
- [`framework/src/wasm_activity.rs`](framework/src/wasm_activity.rs) — בקישור הקרנל↔wasm, לפני instantiation:
  1. קבל את רשימת ה-grants של האפליקציה
  2. בנה import map שמכיל רק את ה-imports שהטבלה ב-`ARCHITECTURE.md §8` מתאימה להם
  3. אפליקציה שקוראת לפונקציה שלא ב-map → link error ב-instantiation, לא runtime panic
- unit test: app שמנסה לקרוא `activity_start_activity` בלי `LAUNCH_ANY_APP` — instantiation נכשל עם הודעה ברורה

### 10.4 תיעוד ABI לפי capability
- עדכון [`docs/abi.md`](docs/abi.md) — סעיף חדש "Imports by Capability" שמצטט את ה-mapping
- כל import מסומן עם ה-capability שמקנה אותו (או `auto` אם always-granted)

### 10.5 Uninstall cleanup
- `PackageManager::uninstall` מוחק גם את `sys/permissions/{id}`
- `PermissionController` מעדכן את in-memory state

### 10.6 calculator + tic-tac-toe מצהירות permissions
- calculator: `[permissions] required = ["akiva.permission.STORAGE_SANDBOX"]` (לשמירת saved_state — auto-granted)
- tic-tac-toe: אותו דבר
- Shell: `["akiva.permission.QUERY_ALL_PACKAGES", "akiva.permission.LAUNCH_ANY_APP"]`
- hello: אין required permissions — מוודא ש-import map ריק מ-optionals פועל

**Exit criteria:**
- [ ] אפליקציה שמצהירה `STORAGE_SANDBOX` מקבלת גישה; אפליקציה בלי ההצהרה — הקריאה `storage.put` לא קיימת ב-imports
- [ ] Shell מצהירה ומקבלת `QUERY_ALL_PACKAGES` ו-`LAUNCH_ANY_APP`; calculator שתנסה לקרוא `query_packages` — fails instantiation
- [ ] grants persist בין boots — התקנה → reboot → grants עדיין שם
- [ ] uninstall מוחק גם את ה-grants (unit test)
- [ ] `docs/abi.md` מסמל capability פר import

**הערכת זמן:** 2-3 שבועות.

---

## סיכום v0.2

**סה"כ זמן:** ~7-10 שבועות. בסיום v0.2 יש לנו:

- פורמט `.akv` מתועד ותפעולי, CLI מלא סביב התקנה
- מחזור חיים מלא, כל launch modes, cross-activity intents עם תוצאות
- Permission model עובד עם אכיפה ב-instantiation, grants persist

ה-Shell עדיין מינימלית, Window manager עדיין stub, אין Notifications. אלה v0.3. אבל בסוף v0.2, **אפשר להגיד בכנות שזו מערכת הפעלה, לא POC**. אפליקציה מותקנת כמו app. רצה כמו app. נסגרת כמו app. מבקשת הרשאות כמו app. ה-chain הקיומי הזה סגור.

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

## פריטי MVP פתוחים — maintenance backlog

פריטים מהפאזות הסגורות-ברובן שעדיין לא הושלמו. מכונס כדי שלא נאבד אותם:

**מ-Phase 1b:**
- Follower rendering — tab שהוא follower לא מצייר כלום. דוחה ל-Phase 2 לפי ה-ROADMAP המקורי, מאז לא חזרנו.

**מ-Phase 2 (Framework):**
- ✅ **IntentRouter wired** (אפריל 2026, commit `c8f18b6`) — כל manifest intent_filter נרשם ב-`framework::boot`, כולל filter סינתטי ל-Shell (MAIN+HOME+LAUNCHER); ה-Shell שולח `Intent::new(MAIN).with_category(LAUNCHER).with_target_package(id)` וה-framework מפעיל `IntentRouter::resolve` לפני `ActivityManager::launch`. ה-boot הראשוני של ה-Shell נפתר כעת דרך `resolve(MAIN+HOME)` במקום hard-code של class. 6 unit tests חדשים כיסוי explicit, implicit, target_package filtering, ambiguity.
- ✅ **ResourceManager wired** (אפריל 2026, 4 stones, commits `a5648d5` → `9c3ecd6`) — MVP scope: strings + colors, default `values/` בלבד (ללא qualifiers). [framework/src/resources.rs](framework/src/resources.rs) מחזיק `ResourceTable` per package; PackageManager קורא את `res/values/{strings,colors}.toml` מ-OPFS ב-boot; 4 host imports חדשים (`akiva_host_resource_color`/`_string`/`_has_color`/`_has_string`) מתועדים ב-[docs/abi.md §2.3](docs/abi.md); ה-SDK חושף `res::color(name) -> u32` ו-`res::string(name) -> String` עם magenta/`⟨missing:NAME⟩` fallbacks; AKML inflater פותר `@color/name` ו-`@string/name` אוטומטית. Calculator עברה להשתמש — כל הצבעים חיים ב-[apps/calculator/res/values/colors.toml](apps/calculator/res/values/colors.toml). 10 unit tests חדשים (7 framework, 4 SDK) + הכל עובד end-to-end בדפדפן.
- **PermissionController is a stub** — [framework/src/permissions.rs](framework/src/permissions.rs) לא אוכף שום דבר. → **יטופל ב-Phase 10 (v0.2)**.
- **WindowManager is a stub** — [framework/src/window_manager.rs](framework/src/window_manager.rs) לא מנתב בפועל. → **דחוי ל-v0.3+**.

**מ-Phase 3 (AKML):**
- ✅ **Grid widget** (אפריל 2026, 3 stones, commits `0fc6d86` → `51d88c9` + calculator migration) — fixed cols×rows row-major container (Android GridLayout style, no auto-wrap/span). Cells split slot evenly minus gaps+padding, `<Grid cols="N" rows="M" gap="G" flex="F">` ב-AKML, ה-calculator עבר מ-VStack של 5×HStack ל-`<Grid cols="4" rows="5">` אחד. 8 layout tests + 6 inflater tests, total 143 SDK tests (was 129).
- Image widget, Input widget
- Attributes: `margin`, `background`, `visible`, `textAlign`, `fontWeight`, `tag`
- יחידות `dp`/`sp`/`%`; `match_parent`/`wrap_content`
- ✅ **Resource references** — `@color/` + `@string/` פועלים; `@dimen/` / `@drawable/` / `@style/` פוסט-MVP.

**מ-Phase 4 (ABI):**
- WIT / Component Model migration — ראה דחייה מתועדת למעלה.

**מ-Phase 5 (Shell):**
- הוצאה ל-`apps/shell/` כ-wasm package — תלוי ב-IntentRouter.

**מ-Phase 6 (Calculator):**
- ✅ `res/values/colors.toml` — כל ה-palette הועבר (commit `9c3ecd6`)
- `res/values/strings.toml` — נדחה; אין כרגע טקסט סטטי UI משמעותי במחשבון (הלייבלים של הכפתורים הם glyphs של תו אחד)
- `res/layout-land/main.akml` — תלוי ב-qualifier matching (post-MVP)
- ✅ `on_save_state` / restore (אפריל 2026, 5 stones, commits `53e9e0d` → `3973c26`) — SavedState wire codec ב-SDK (TLV, max 64 KiB), שני exports חדשים `akiva_app_save_state` / `akiva_app_restore_state`, שני imports kernel↔host חדשים (`hal_app_save_state`, `hal_app_restore_state`), `WasmHost` trait בframework קיבל save/restore defaults ו-WasmActivity מעביר אותם דרך ה-host. Framework מחזיק `Box<dyn Storage>` post-boot ומתמיד ל-`packages/{id}/activities/{class}/saved_state.bin` בקצה Paused→Stopped, ומשחזר בין Create ל-Start. Calculator משמר display/acc/pending/just_computed/error ומדפיס את מצב ה-restore ב-log. +10 unit tests (8 SDK codec, 2 framework mock host).
- `values-he` — **מוחרג מ-MVP** (user preference: MVP-first, skip i18n)

**מ-Phase 7 (CLI):**
- ✅ **`akiva devserver`** (אפריל 2026, commit `9a132c2`) — HTTP server אמיתי stdlib-only עם COOP/COEP/CORP, thread-per-request, path-traversal rejection, mime table, הגדלות flags `--addr` / `--port`.
- ✅ **`akiva build [names...]`** (commit `13fd1fb`) — wasm build pipeline המחליף את ה-build.sh הפזורים. אוטו-גילוי `apps/*/Cargo.toml`, staging ל-`runtime/web/dev-apps/`, עדכון `res/values/` נקי.
- ✅ **`akiva run`** (commit `eb3eff4`) — kernel build + copy + כל האפליקציות + devserver בפקודה אחת. `--no-build` ל-skip, `--debug` ל-dev profile.
- ✅ **`akiva new <name>`** (commit `8e61a5a`) — scaffolding עם Cargo.toml + manifest.toml + src/lib.rs + .gitignore. שם נורמליזציה ל-kebab-case, Activity class ו-package_id נגזרים אוטומטית. `--force` לoverwrite.
- `akiva package` / `akiva install` — **יטופלו ב-Phase 8 (v0.2)**. פורמט `.akv` יתועד ב-`docs/archive-format.md`.

ה-backlog הזה **לא** בלוקר ל-MVP; כל פריט הוא תוספת שמתלבשת על המבנה הקיים בלי rewrite. אם נרצה לסגור את Phase 2 פורמלית, הפריטים של Phase 2 הם מה שצריך להשלים.

---

## מה *לא* ב-MVP (רשימה מכוונת)

רזה **בעומק**, לא בשלמות:

- ❌ Animations (רק transitions מיידיים)
- ❌ Multi-window / split screen (activity אחד display אחד)
- ❌ Intent chooser UI (אם יש matches מרובים — קחי את הראשון)
- ❌ App store / distribution — ב-v0.2 מגיע פורמט `.akv` + CLI install, אבל לא store מרוכז (Phase 8)
- ❌ Network APIs (לא נחשף ל-apps עדיין)
- ❌ Accessibility (ה-ארכיטקטורה מוכנה; מימוש דחוי ל-v0.3+)
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

### מה נכלל ב-v0.2 (ולא ב-v0.1/MVP)

v0.2 ממקד ב-**חוזה המערכת עם אפליקציות**, לא ב-UI. ראה [`docs/v0.2-design.md`](docs/v0.2-design.md) למלאי.

- ✅ פורמט הפצה `.akv` + `akiva package`/`install`/`uninstall` (Phase 8)
- ✅ מחזור חיים מלא של Activity + SingleTask/SingleInstance + back navigation (Phase 9)
- ✅ `start_activity` / `start_activity_for_result` / `on_new_intent` / `on_activity_result` (Phase 9)
- ✅ Permission model (Capability-via-Imports, install-time grants, persistence) (Phase 10)

**מחוץ ל-v0.2, עדיין דחוי ל-v0.3+:**

- ❌ Window manager אמיתי (multi-surface, z-order, focus)
- ❌ System services (notifications, clipboard, location)
- ❌ Runtime permission prompts (dangerous perms — camera/microphone)
- ❌ PendingIntent + Broadcast receivers
- ❌ Image/Input/ScrollView widgets
- ❌ Animations, themes, sophisticated layouts
- ❌ Network API, keyboard input, IME
- ❌ Code signing, hot reload, debugger/inspector
- ❌ SDKs ל-non-Rust, multi-threading, inter-app storage sharing
- ❌ עברית / RTL / BiDi

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
