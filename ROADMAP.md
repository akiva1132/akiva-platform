# תוכנית פיתוח — Akiva Platform

> **Deferred concerns:** דאגות ארכיטקטוניות שאינן stones בפועל אבל חיוניות לתעד — sandboxing/crash-recovery, ABI v0→v1, web interop, text/input — נמצאות ב-[`docs/risks.md`](docs/risks.md). אם אחת מהן מקבלת trigger, היא עוברת ל-ROADMAP ונמחקת משם.

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

**סטטוס (מאי 2026):**
- ✅ **Shell כ-wasm app רגיל** ([apps/shell/](apps/shell/)) — Stone 11.0a/b/c. Shell מותקנת דרך ה-dev-apps pipeline כמו כל חבילה אחרת, מצהירה על הרשאות `QUERY_PACKAGES + LAUNCH_ANY_APP` ב-manifest שמתקבלות אוטומטית ב-install. ה-launcher list מתקבל דרך `akiva_sdk::pkgs::list()` (Stone 11.0a) שעובר את ה-snapshot שה-framework דוחף ל-host ב-boot. דחיפה של אפליקציה משתמשת ב-`Context::start_activity(&intent)` הסטנדרטי. אין יותר special-casing ב-`framework::boot` ול-`framework/src/shell.rs` נמחק.

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
- ✅ **AKPK v1** (commits `ce8b500`, `d74abcf`) — פורמט custom stdlib-only ב-[`cli/src/archive.rs`](cli/src/archive.rs) (לא ZIP כדי להימנע מ-dependency-hell), מתועד ב-[`docs/archive-format.md`](docs/archive-format.md). `Archive::create` ו-`Archive::extract` עם manifest + wasm + res/ + signature placeholder.

### 8.2 `akiva package <app>`
- ✅ (commit `d734923`) — [`cli/src/package.rs`](cli/src/package.rs) אורז staged app dir ל-`<app>.akv` עם manifest + wasm + `res/values/`. Flag `--out`.

### 8.3 `akiva install <archive>`
- ✅ (commit `76a9daf`) — [`cli/src/install.rs`](cli/src/install.rs) פורק `.akv` ל-`runtime/web/dev-apps/<id>/`. `--as` ו-`--to` flags.
- ⏭️ **validation דרך `PackageManager::install`** — ה-CLI כרגע רק פורש קבצים; העברה דרך ה-framework-side install (שיוודא manifest + grant permissions) מחכה לאינטגרציה שבה framework יהיה dep של ה-CLI. נעשה בזמן Phase 11 prep (דורש ממילא רפקטור של ה-boot flow).

### 8.4 Shell discovery מהאינדקס
- ✅ — `const DEV_APPS` ו-`installDevApps` מ-worker.js נמחקו. Shell מושכת דרך `PackageManager::list()`; devserver מספק endpoint `/dev-apps/index.json` לסריקת subdirs.

### 8.5 Upgrade + uninstall
- ✅ (commit `72115b4`) — `akiva uninstall <id>` + `PackageManager::install` upgrade path. Downgrade/same-version נדחים עם שגיאות מפורשות. `saved_state` מתנקה רק כש-MAJOR של `app.version` משתנה (מתאים ל-`manifest.md §6`).

**Exit criteria:**
- [x] `akiva package calculator && akiva install calculator.akv` עובד end-to-end
- [x] `runtime/web/worker.js` לא מכיל שום שם של אפליקציה ספציפית
- [x] התקנת archive חדש, ה-Shell מציגה את האפליקציה בלי שינוי קוד
- [x] upgrade + uninstall עובדים (unit tests ב-`framework/src/package_manager.rs`)

**סטטוס:** Phase 8 MVP core סגור. ה-`PackageManager::install` integration ב-CLI (ל-OPFS דרך הקרנל במקום file-extraction) תיסגר כחלק מ-Phase 11 prep כשהקרנל יפוצל מה-worker הראשי.

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
- ✅ **`LaunchOutcome` return type** — `ActivityManager::launch` מחזיר `{ id, reused, popped }` במקום `ActivityId` גלמי. `popped` (top-first order) מזהה activities מעל ה-target שה-caller (Framework) חייב לסגור. החלטה מודעת: `launch()` רק **מזהה** את ה-popped, לא משנה את ה-lifecycle state שלהם — ה-destroy callbacks שייכים ל-Framework.
- ✅ **SingleTask reuse** — `ActivityManager::launch` סורק את foreground task ל-`(pkg, class)` תואם; אם נמצא, `popped = stack[pos+1..].rev()`. `LaunchError::Unsupported` הוסר מה-enum.
- ✅ **SingleInstance reuse (MVP)** — זהה סמנטית ל-SingleTask ב-MVP עם task קדמי יחיד. ה-"dedicated task isolation" האמיתי נדחה ל-Stone 9.6 — `launch()` הוא ה-seam הלא נכון לזה, ה-foreground-switch decision שייך ל-Framework.
- ✅ **`Framework::launch_app` מטפל ב-popped** — לכל id ב-`outcome.popped` קורא ל-`destroy_with_persist` (factored out from `finish_top`) שמריץ pause→stop→destroy ומשמר save-state על edge ה-Stop. אחרי ה-destroy loop, אם `reused`, קורא ל-`drive_start_to_resumed` (מכסה Paused→Resumed ו-Resumed no-op) ואז `deliver_new_intent`.
- ✅ **Unit tests** — 4 ב-`activity_manager::tests`: empty-task create-fresh, top-match reuse-no-pop, below-top pop-above-reuse, SingleInstance ≡ SingleTask, popped-state-untouched invariant.
- ✅ **Integration test** — `framework::tests::singletask_relaunch_pops_above_and_fires_new_intent`: Shell→TaskApp(SingleTask)→Filler(Standard)→relaunch TaskApp; מאמת רצוף Filler pause/stop/destroy → task:resume → task:new_intent.
- ⏭️ **SingleInstance dedicated-task isolation** — Stone 9.6.

### 9.3 Back stack + `Context::finish()`
- ✅ **SDK `Context::finish()` plumbing** — `akiva_host_finish(handle)` import → worker per-handle flag table → kernel drains via `hal_app_drain_finish` → `WasmActivity.drain_post` קובע `ActivityContext.finish_requested = true`.
- ✅ **Created→Destroyed shortcut** — `(Created, Destroyed)` ב-`legal_transition`; `advance_through_create` מחזיר אם finish הופעל; `launch_app` קורא ל-`destroy_shortcut` (on_destroy בלבד) ו-pop של ה-stack. parity עם Android.
- ✅ **Integration test (test-coverage.md §4)** — כוסה ב-Stone 9.4: `framework::tests::standard_launch_stacks_and_finish_unwinds` בודק Shell→App→(Esc back)→Shell end-to-end דרך `dispatch_pointer`+`dispatch_key`, מאמת את ה-lifecycle sequence המלא של ה-App (pause/stop/destroy) וחזרה ל-`Resumed` של ה-Shell.
- ⏭️ **`Context::back()` + `akiva_host_back` import** — Shell policy / gesture mapping. כרגע ה-framework קוצר על Esc מ-key events; אין app שצריך להפעיל Back ישירות. נדחה עד שיש use case.
- ⏭️ **Gesture engine** — v0.2: קיצור מקלדת fixed בלבד, אין gesture. נשמר כ-feature של Phase 11+.

### 9.4 `start_activity` מתוך app + `on_new_intent`
- ✅ **Intent wire codec** — `Intent::encode`/`decode` ב-framework ובמקבילה ב-SDK. Byte-for-byte symmetric, 16 KiB cap, `extras_count=0` reserved ל-Stone 9.5. פורמט נעול ב-[`docs/abi.md §2.5.1`](docs/abi.md).
- ✅ **`akiva_host_start_activity` + `hal_app_drain_intent` plumbing** — SDK `Context::start_activity(&intent)` → worker per-handle `_pendingIntent` map → kernel `KernelWasmHost::drain_intent` (two-phase buffer, retries on ≤512-byte guess) → framework `WasmActivity::drain_post` מעביר ל-`ctx.launch_requested` → `apply_post` פותר דרך `IntentRouter` ומעביר ל-`launch_app`.
- ✅ **`on_new_intent` על SingleTop reuse** — `Framework::launch_app` מעביר את ה-`Intent` המלא ל-reuse branch; `deliver_new_intent` קורא ל-`Activity::on_new_intent(ctx, &intent)`, `apply_post` רקורסיבי אם ה-handler עצמו ביקש עוד משהו. test: `singletop_reuse_fires_on_new_intent_instead_of_bring_up`.
- ✅ **`akiva_app_new_intent` export + `hal_app_new_intent`** — נדרש ב-`REQUIRED_EXPORTS` ב-worker, מחובר דרך `akiva_app!` macro; framework `WasmHost::new_intent` (kernel impl + MockHost) משלים את הצד השני.
- ✅ **Framework-level back-stack unwind test** — `standard_launch_stacks_and_finish_unwinds` מכסה Shell→App→(Esc)→Shell על pointers אמיתיים; פותר גם את ה-Integration test שמוזכר ב-9.3 (Rust-side עם Mock). החבורה של "A פותח B, B פותח C, back×3" נשארת בעיקר ה-path שבדוק ע"י שני ה-tests האלה — הרחבה ל-3 רמות היא שכפול של אותו code path.
- ✅ **`on_new_intent` על SingleTask/SingleInstance reuse** — נחת ב-Stone 9.2.
- ✅ **Doc cleanup** — `v0.2-review.md`, `research-android-lifecycle.md`, `research-activity-boundary.md` נמחקו; תוכן רלוונטי מקופל ב-`activity-model.md`; 9 cross-links עודכנו. `research-component-model.md` ו-`test-coverage.md` נשארים.

### 9.5 `start_activity_for_result` + `on_activity_result`
- ✅ **ActivityResult wire format** — `i32 code + u32 data_len + <data_len> bytes`, 16 KiB cap, byte-symmetric בין framework ל-SDK. `RESULT_OK = 0` / `RESULT_CANCELED = 1` (שלנו, לא של Android); app-defined codes מעליהם. פורמט נעול ב-[`docs/abi.md §2.5.2`](docs/abi.md).
- ✅ **Three new primitives** — `akiva_host_start_activity_for_result(handle, ptr, len, request_code)`, `akiva_host_set_result(handle, ptr, len)` (app→kernel), ו-`akiva_app_activity_result(handle, request_code, ptr, len)` (kernel→app). שלושת ה-`hal_app_*` bridges המקבילים משתמשים ב-2-phase buffer pattern מ-Stone 9.4. ה-`REQUIRED_EXPORTS` עודכן.
- ✅ **Framework tracking** — `pending_result_for: HashMap<callee_id, (caller_id, request_code)>` + `pending_result: HashMap<callee_id, ActivityResult>`. `launch_app_for_result` מוסיף לרישום; `finish_top` → `destroy_with_persist(callee)` → `deliver_activity_result_for(callee)` שמוציא את ה-bytes (או מסנתז `canceled()` אם אין), קורא ל-`on_activity_result` ב-caller *לפני* ה-Paused→Resumed (Android parity).
- ✅ **Reuse path drops request_code silently** — start_for_result against SingleTop/SingleTask/SingleInstance reuse target: ה-reused activity לא "מסיימת" במובן שמפעיל delivery. Documented + tested (`for_result_reuse_path_drops_request_code_silently`).
- ✅ **popped-above activities synthesise CANCELED** — אם SingleTask pop-above-reuse מוחק activity שהיתה for-result callee, ה-caller עדיין מקבל CANCELED ולא נתקע בהמתנה.
- ✅ **Tests** — 8 codec (4 framework + 2 SDK mirror + 2 constant pin), 4 wasm_activity drain tests (start_for_result / set_result / malformed / activity_result forward), 3 framework integration tests (success, canceled synthesis, reuse drop), 7 JS Vitest tests (drainStartForResult / drainSetResult / activityResult / unregister cleanup). +22 tests total.
- ⏭️ **Demo app (hello ↔ calculator)** — ה-infrastructure מוכנה; הוספת demo מותנית בעבודת UI על calculator (mode "pick") ו-hello (button שפותח את ה-calculator for-result). לא חלק מה-stone; נדחה לעבודת apps עתידית.

### 9.6 Multiple tasks / foreground switching
- ✅ **`ActivityManager::bring_task_to_front(task_id) -> Option<TaskId>`** — מחזיר את ה-foreground הקודם אם החלפה קרתה, `None` ל-no-op (task לא קיים, או כבר foreground). ה-state machine של ה-activities עצמן לא זזה — ה-Framework אחראי ל-pause/resume.
- ✅ **`ActivityManager::task_stack(task_id) -> Option<&[ActivityId]>`** — accessor ל-bottom-to-top stack. ה-Framework משתמש בו כדי לסנכרן את ה-`fw.stack` (mirror של ה-foreground task) בזמן switch.
- ✅ **`SingleInstance` dedicated-task אמיתי** — fresh-create מקצה task חדש דרך `allocate_task` (לא דרך `ensure_foreground_task`), ולא מגדיר foreground. ה-Framework מפעיל `bring_task_to_front` אחרי ה-launch. reuse סורק את **כל** ה-tasks (cross-task), `popped` תמיד ריק כי ה-dedicated task מחזיק רק instance אחד. Standard / SingleTop / SingleTask ממשיכים לנחות ב-foreground task הקיים (task-by-affinity נדחה — לא מורגש כשיש display יחיד).
- ✅ **`Framework::switch_to_task(task_id, ts)`** — public API שהופעל ע"י Shell כאשר משתמש בוחר app רץ. משתמש ב-`switch_to_task_if_needed` פנימי שמריץ `drive_to_paused` על top הקודם, `switch_foreground_stack` (שמפעיל `bring_task_to_front` + mirror-sync), ואז `drive_start_to_resumed` על ה-top החדש. `task_history: Vec<TaskId>` נדחף בכל switch כדי ש-`finish_top` יוכל לחזור לאחור כשה-foreground מתרוקן.
- ✅ **Back-out מ-dedicated task** — `finish_top` במקרה של `stack.len() == 1` + `task_history` לא ריק מחזיר את ה-foreground ל-task הקודם (pop מה-history) במקום לחסום finish. ה-Back-key short-circuit עודכן ל-`stack.len() > 1 || !task_history.is_empty()` — משתמש יכול לברוח מ-SingleInstance דרך Escape.
- ✅ **Cross-task fresh-create כולל Created→Destroyed shortcut** — אם ה-activity החדשה (ב-task חדש) מסיימת מ-on_create, ה-framework מבטל את ה-foreground switch ומחזיר את ה-task הקודם.
- ✅ **Tests** — 5 ב-`activity_manager::tests` (`launch_singleinstance_lives_in_its_own_task`, `launch_singleinstance_reuse_is_system_wide`, `bring_task_to_front_switches_foreground_and_returns_prev`, `task_stack_returns_bottom_to_top_slice`), 4 integration ב-`framework::tests` (`singleinstance_fresh_create_allocates_dedicated_task_and_foregrounds_it`, `singleinstance_reuse_across_tasks_brings_target_task_forward`, `task_swap_preserves_each_tasks_stack`, `back_from_singleinstance_returns_to_prev_task`).
- ⏭️ **Task-by-affinity routing** — Standard / SingleTop / SingleTask ממשיכים לנחות ב-foreground task הקיים. עם display יחיד ההבדל לא נראה; task-per-affinity אמיתי נדחה ל-Phase 11+ (multi-display / picture-in-picture).
- ⏭️ **Shell UI ל-task switcher** — ה-infrastructure מוכנה (`switch_to_task` public). UI שמציג את ה-tasks החיים לבחירה נדחה לעבודת Shell עתידית.

**Exit criteria:**
- [x] כל ארבעת ה-launch modes עוברים acceptance tests שמותווים ב-[`activity-model.md §4`](docs/activity-model.md)
- [x] 5 ה-lifecycle callbacks כולם נקראים ב-apps (מאומת ב-unit tests + integration tests; manual-exercise demo נדחה עם ה-Shell task switcher UI)
- [ ] `start_activity_for_result` demo (hello ↔ calculator) — infrastructure מוכנה, demo app UI נדחה (9.5 deferred note)
- [x] back navigation עובד ב-stack של לפחות 3 activities (`standard_launch_stacks_and_finish_unwinds` + `singletask_relaunch_pops_above_and_fires_new_intent`; `back_from_singleinstance_returns_to_prev_task` מוסיף cross-task back)
- [x] tasks מרובים עם affinity שונה — `task_swap_preserves_each_tasks_stack` + `bring_task_to_front_switches_foreground_and_returns_prev`

**סטטוס:** Phase 9 סגור (Stones 9.1–9.6). עבודה שנדחתה במודע: demo app UI של start_activity_for_result ו-Shell task-switcher UI — שתיהן עבודת apps/shell, לא infrastructure.

**הערכת זמן:** 3-4 שבועות.

---

## שלב 10 — Permissions (Capability-via-Imports)

**מטרה:** אפליקציה רואה רק את ה-imports שהמניפסט שלה הצהיר עליהם, ה-PackageManager שומר grants, הקישור נכשל אם אפליקציה מנסה לקרוא להרשאה שאין לה.

**תלוי ב:** שלב 8 (install flow שקורא permissions), שלב 9 (imports ספציפיים-להרשאה כמו `LAUNCH_ANY_APP` קיימים רק אחרי 9.4-9.5).

**רכיבים שנבנים:**

### 10.1 System permission registry
- ✅ **`declare_mvp_permissions(&mut PermissionController)`** — helper חדש ב-[`framework/src/permissions.rs`](framework/src/permissions.rs) שמכריז על שש ההרשאות מ-[ARCHITECTURE.md §8](ARCHITECTURE.md) בבת אחת: `STORAGE_SANDBOX`, `QUERY_PACKAGES`, `LAUNCH_ANY_APP`, `NOTIFICATIONS`, `NETWORK`, `CAMERA`. קבועי `pub const` לכל שם (למניעת typo ב-wiring של Stone 10.3).
- ✅ **`framework::boot` מכריז לפני rehydrate** — Stone 10.2 ישען על זה: install יבדוק `is_declared` לפני grant, אז ההצהרה חייבת לקדם את load_installed.
- ✅ **`ProtectionLevel::Normal` לכולן (v0.2)** — auto-granted install-time, בלי user prompt. Tripwire test (`mvp_permissions_are_all_normal_protection`) שובר את ה-build אם מישהו מעלה הרשאה ל-`Dangerous` בלי לעדכן את ה-install path.
- ✅ **API additions ל-`PermissionController`** — `is_declared(name)` ו-`declarations()` iterator; Stone 10.2 ישתמש בהם. 5 unit tests חדשים.

### 10.2 Install-time grant + persistence
- ✅ **Signature change** — `PackageManager::install` ו-`uninstall` מקבלים כעת `&mut PermissionController` נוסף (forward-compat ל-production install flow ב-Phase 8). 23 test call-sites עודכנו אוטומטית.
- ✅ **Validation first** — install בודק `is_declared` לכל `manifest.permissions.required` **לפני** כל כתיבה ל-storage. הרשאה לא מוכרת → `InstallError::UnknownPermission { package_id, name }`; storage נשאר נקי לחלוטין (אין manifest, wasm, index או permissions file).
- ✅ **`apply_grants` helper** — כותב set מלא ל-`sys/permissions/{id}` (LF-separated, סדר מה-manifest). כולל blob ריק ל-apps בלי permissions, כדי ש-rehydration תדע להבחין בין "installed בלי permissions" ל-"pre-10.2 install".
- ✅ **Upgrade semantics** — v2 שמסיר permissions מה-manifest משקף זאת: ה-controller revokes את מה שאבד, ו-blob הפרסיסטנטי נכתב מחדש. v2 שמוסיף — מקבל grants חדשים.
- ✅ **Boot-time rehydration** — `PackageManager::load_granted_permissions` נקרא מ-`framework::boot` אחרי `load_installed`, קורא את `sys/permissions/{id}` לכל package ו-grants דרך ה-controller. כישלונות נרשמים ל-`package_errors` אבל לא שוברים את ה-boot.
- ✅ **Tests** — 6 חדשים ב-`package_manager::tests`: grant-on-install, unknown-permission-rejection (כולל storage clean-state), rehydration round-trip, upgrade revocation, uninstall cleanup (Stone 10.5 tripwire), empty-blob invariant.

### 10.3 Import map לפי capabilities
**מחולק לשני שלבים:**
- ✅ **10.3a — Rust-side mapping (source of truth)** — [`framework/src/permissions.rs`](framework/src/permissions.rs) מוסיף `AUTO_IMPORTS` (8 שמות ש-app מקבל תמיד: log, text_width, resource_*, finish, set_result), `imports_for_capability(name) -> &'static [&'static str]` (טבלת mapping מ-`ARCHITECTURE.md §8`), ו-`imports_for_grants(&PermissionController, package_id) -> Vec<&'static str>` (auto + gated לפי grants בפועל). כרגע רק `LAUNCH_ANY_APP` ממפה imports (`akiva_host_start_activity` + `_for_result`); `STORAGE_SANDBOX`/`QUERY_PACKAGES`/`NOTIFICATIONS`/`NETWORK`/`CAMERA` ברשימה אבל empty-slot (ה-HAL של storage/notifications/network לא נגיש לאפליקציות עדיין). 4 unit tests.
- ✅ **10.3b — JS-side enforcement (soft via deny-stubs)** —
  - **HAL חדש:** `hal_host_set_grants(pkg_ptr, pkg_len, grants_ptr, grants_len)` ב-[`kernel/src/host.rs`](kernel/src/host.rs); LF-separated UTF-8 על ה-wire (זהה לפורמט הפרסיסטנטי ב-`sys/permissions/{id}`).
  - **Trait method חדש:** `WasmHost::set_grants(package_id, grants)` עם default no-op; `KernelWasmHost` מממש דרך ה-extern, MockHost מתעד `Call::SetGrants` לטסטים.
  - **`framework::boot` push:** אחרי `load_granted_permissions`, איטרציה על כל `fw.packages.list()` ו-`host.set_grants(pkg, &grants)`. רץ לפני ה-Shell launch כך שכל instantiation עתידי כבר רואה את ה-grants.
  - **worker.js bridge:** `hal_host_set_grants` ב-`buildHalImports`, מפענח את ה-LF blob, מעביר ל-`apps.setGrants(pkg, names)`.
  - **AppRegistry filter ב-`_buildAppImports`:** כל import מסווג ל-AUTO (`AUTO_IMPORTS`, 8 תמיד) / gated+granted (passthrough) / gated+denied (deny-stub). Stub יורה warning log + מחזיר 0. Last-write-wins ב-grants map (תואם upgrade semantics).
  - **`AUTO_IMPORTS` + `CAPABILITY_IMPORTS` ב-JS:** mirror של ה-Rust constants ב-`framework/src/permissions.rs`. Reserved capabilities (STORAGE_SANDBOX/QUERY_PACKAGES/NOTIFICATIONS/NETWORK/CAMERA) במקום עם empty arrays — tripwire test (`reserved capabilities ... contribute no imports today`) יקרס ביום שה-HAL ינחת.
  - **Tests:** 1 Rust חדש (`set_grants_delegates_to_host_with_owned_strings`) + 8 Vitest חדשים (setGrants store, last-write-wins, AUTO passthrough, deny-stub fires warning, granted unlocks, per-package isolation, reserved-slot tripwire). 129 framework tests + 42 Vitest tests.
  - **לא נכלל (defer מוצהר):** True LinkError-at-instantiation (ARCHITECTURE.md §8 ideal). דורש SDK build-tooling שמפיק externs מותנים לפי ה-manifest. כרגע ה-soft enforcement (deny-stub) זה מה שנשלח.

### 10.4 תיעוד ABI לפי capability
- ✅ **סעיף 2.3.1 חדש ב-[`docs/abi.md`](docs/abi.md)** — "Imports by Capability" עם טבלת mapping מלאה (auto set + כל אחת מהשש permissions). ה-slots הlead-reserved (STORAGE_SANDBOX / QUERY_PACKAGES / NOTIFICATIONS / NETWORK / CAMERA) מסומנים כ-"reserved — post-MVP" כך שמפתח יודע שהם ב-mapping אבל עדיין לא מחוברים. עיקרון האכיפה מוסבר: "link error, not runtime error."

### 10.5 Uninstall cleanup
- ✅ **נחת כחלק מ-Stone 10.2** — `PackageManager::uninstall(storage, &mut PermissionController, id)` מוחק את `sys/permissions/{id}` ועושה revoke לכל ה-grants של ה-package מה-controller. Test: `uninstall_deletes_permissions_file_and_revokes_grants`.

### 10.6 calculator + tic-tac-toe + Shell מצהירים permissions
- ✅ **calculator** — `[permissions] required = ["akiva.permission.STORAGE_SANDBOX"]` (שמירת display/accumulator state דרך `on_save_state`/`on_restore_state`).
- ✅ **tic-tac-toe** — אותו `STORAGE_SANDBOX` (שמירת board state).
- ✅ **Shell** — מצהירה `QUERY_PACKAGES` + `LAUNCH_ANY_APP` ב-`apps/shell/manifest.toml`. עוברת ב-Stone 11.0c את אותו install-time grant path כמו כל אפליקציה. (לפני Stone 11.0c ה-grants ניתנו ישירות ב-`framework::boot` כי Shell היה built-in.)
- ✅ **hello** — נשאר בלי `required`. משמש כ-tripwire ל-empty-permissions path (install_with_no_permissions_writes_empty_blob test ב-Stone 10.2).

**Exit criteria:**
- [x] אפליקציה שמצהירה `STORAGE_SANDBOX` מקבלת grant ב-install-time (test: `install_grants_required_permissions`). אכיפת "import map ריק מ-storage" — Stone 10.3b.
- [x] Shell מקבלת `QUERY_PACKAGES` ו-`LAUNCH_ANY_APP` ב-boot; mapping מוכיח ש-`akiva_host_start_activity` ב-import set של Shell (test: `launch_any_app_adds_start_activity_imports`). אכיפת "calculator בלי LAUNCH_ANY_APP → LinkError" — Stone 10.3b.
- [x] grants persist בין boots — `grants_rehydrate_from_storage_on_boot` test.
- [x] uninstall מוחק גם את ה-grants — `uninstall_deletes_permissions_file_and_revokes_grants` test.
- [x] `docs/abi.md` מסמל capability פר import — §2.3.1.
- [ ] JS-side instantiation filter (Stone 10.3b) — נדחה; דורש kernel-to-worker bridge + SDK build-tooling.

**סטטוס:** Phase 10 MVP core סגור. 10 tests חדשים (5 permissions + 6 install/grant + הסרה -1 old). Stone 10.3b (JS enforcement) ממתין לעבודת SDK build tooling.

**הערכת זמן:** 2-3 שבועות.

---

## סיכום v0.2

**סה"כ זמן:** ~7-10 שבועות. בסיום v0.2 יש לנו:

- פורמט `.akv` מתועד ותפעולי, CLI מלא סביב התקנה
- מחזור חיים מלא, כל launch modes, cross-activity intents עם תוצאות
- Permission model עובד עם אכיפה ב-instantiation, grants persist

ה-Shell עדיין מינימלית, Window manager עדיין stub, אין Notifications. אלה v0.3. אבל בסוף v0.2, **אפשר להגיד בכנות שזו מערכת הפעלה, לא POC**. אפליקציה מותקנת כמו app. רצה כמו app. נסגרת כמו app. מבקשת הרשאות כמו app. ה-chain הקיומי הזה סגור.

---

# v0.3 — מ-"מערכת הפעלה על שלד" ל-"OS core אמיתית"

v0.2 הפך את Akiva ממערכת-הפעלה-על-נייר למערכת-הפעלה-עם-חוזים. **אבל עדיין יש מקום אחד שבו היא מתנהגת כ-runtime ולא כ-OS:** כל האפליקציות חיות באותו Web Worker, על אותו event loop, עם אותו JS heap. App שנתקע ב-infinite loop מקפיא את המערכת כולה; wasm trap ב-activity משאיר `ActivityRecord` ב-Resumed ללא cleanup; אין Low-Memory Killer כי אין memory accounting.

v0.3 סוגר את הפער הזה. הוא בנוי סביב שלושה עמודים, **בסדר חובה** — כל אחד דורש את הקודם לצאת.

**חשיבות סדרי הגודל:** העמוד הראשון (isolation) לבדו הוא shift ארכיטקטוני מ-sync ABI ל-async ABI שמגע בכל הstack (framework, SDK, runtime). השני (governance) תלוי בקיום workers נפרדים כדי למדוד-ולאכוף. השלישי (window manager) הוא bounded scope, לא architectural.

## Prereqs לפני Phase 11 (סגירת חזיתות פתוחות)

לפני שמתחילים את ה-rewrite הגדול כדאי לסגור שתי חזיתות קצרות:

### Stone 10.3b — JS-side permission filter
- ✅ **נחת.** ראה Phase 10 לעיל לפירוט המלא. Soft enforcement דרך deny-stubs (warning log + return 0); true LinkError-at-instantiation מחכה ל-SDK build-tooling.

### Stone 11.0 — הוצאת Shell ל-`apps/shell/` כ-wasm app
- ✅ **נחת (מאי 2026)**, פוצל ל-3 sub-stones:
  - **11.0a — `akiva_host_query_packages` HAL.** Length-prefixed wire format ב-[`framework/src/pkgs_wire.rs`](framework/src/pkgs_wire.rs); push בכריכה בoot עם `WasmHost::set_packages` → `hal_host_set_packages` → `AppRegistry.setPackages`; capability `akiva.permission.QUERY_PACKAGES` עכשיו unlocks את ה-import (לא יותר reserved-slot ריק); SDK wrapper ב-[`sdk-rust/src/pkgs.rs`](sdk-rust/src/pkgs.rs) עם `pkgs::list() -> Vec<PackageEntry>`.
  - **11.0b — `apps/shell/` crate.** Manifest מצהיר `sys.shell` עם `intent_filter: MAIN+HOME+LAUNCHER`; lib.rs מעבר verbatim של ה-rendering/input מ-`framework/src/shell.rs` לשימוש ב-`pkgs::list()` + `Context::start_activity()`; `boot_count` נופל מהכותרת (Stone 11.0d).
  - **11.0c — boot surgery.** הוסרו `pub mod shell`, ה-grants הידניים ל-`shell::PACKAGE_ID`, ה-intent filter הסינתטי, registration של `ShellActivity::new(...)`, וסינון Shell מ-`pkg_refs`. חתימת `boot()` ירדה מ-`(boot_count, storage, host)` ל-`(storage, host)`. HOME resolution עכשיו tolerates missing Shell (warn דרך `BootReport.package_errors`, לא panic). [`framework/src/shell.rs`](framework/src/shell.rs) נמחק (224 שורות).
- **למה זה חשוב:** Phase 11 (Process Isolation) עכשיו רואה את Shell כ-app רגיל — אין יותר exception פריבילגי שדורש ייחוד בכל מנגנון isolation.

---

## שלב 11 — Process / Fault Isolation + Modern Async SDK

**מטרה:** הקפצת Akiva ל-OS-grade. שני יעדים שלובים:
1. **Isolation:** פעילות של app אחד — trap, infinite loop, alloc זיכרון — לא יכולה להשפיע על אחרים או על kernel.
2. **DX ברמת Android/iOS:** SDK async-native עם lifecycle scopes, structured concurrency, ו-onboarding ב-5 דקות.

**תלוי ב:** Stone 10.3b + Stone 11.0 (Shell extraction).

**מסמך תכנון מלא:** [`docs/isolation-design.md`](docs/isolation-design.md) (Design v2 — מבוסס מחקר על WASI 0.3, OPFS, Chrome Site Isolation, Android Coroutines, SwiftUI structured concurrency, embassy executor).

### 11.1 Design doc ✅
- ✅ **נחת (מאי 2026)** — [`docs/isolation-design.md`](docs/isolation-design.md) Design v2. 14 סעיפים. 8 stones מוגדרים.
- **החלטות מרכזיות:**
  - **ABI async-native:** `Activity` trait עם `async fn on_create/...`, host calls מחזירים `Future`. embassy executor רץ בתוך כל app.
  - **Worker-per-app:** package = unit of isolation. `app_worker_shell.js` חדש.
  - **transport postMessage primary** (מהיר מספיק ב-Chrome 2026: 0-1ms). SAB אופציונלי לעתיד.
  - **OPFS hybrid:** apps פותחות readonly handles ישירות (multiple readonly נתמכים מ-Chrome 121+); writes עוברים דרך kernel.
  - **lifecycle scopes:** `activity_scope`, `resumed_scope` — מקבילים ל-Android `lifecycleScope` ו-SwiftUI `.task`.
  - **Component Model נדחה ל-v0.5+** (WASI 0.3 עוד preview, jco transpile required); ה-ABI שלנו עם semantics קרובות ל-future migration.
  - **iframe-per-app נדחה** (דורש DNS infrastructure ל-Spectre-grade); Worker isolation מספקת thread+heap isolation שמספיקה למודל המאיים שלנו.

### 11.2 Async SDK + Activity trait (3-4 שבועות)
- `embassy-executor` integration ב-`akiva-sdk`.
- `Activity` trait async (`async fn on_create`, `on_resume`, ...).
- `Context` async methods (`start_activity().await`, `storage.get().await`, `pkgs::list().await`).
- 3 lifecycle scopes — `activity_scope`, `resumed_scope`, one-shot.
- `task!` + `task_until!` macros.
- Migration של 4 האפליקציות הקיימות ל-async (calc, hello, shell, tic-tac-toe).
- 141 framework tests ממשיכים לעבור (עם `.await`).
- **עדיין רץ ב-single worker** — שלב הproof שה-async API עובד.

### 11.3 Async ABI + future registry (2-3 שבועות)
- `akiva_host_io_start`, `akiva_host_io_poll`, `akiva_host_fire` imports.
- `FutureRegistry` ב-kernel-worker — מנהל future IDs.
- wire encoders ב-`framework/src/wire/` (op_kinds, lifecycle, storage, pkgs).
- `runtime/web/app_registry.js` עובר ל-future-based dispatch.
- benchmark: host call latency ≤ 1ms.

### 11.4 Worker-per-app (3-4 שבועות) — **ה-isolation בפועל**
- `runtime/web/app_worker_shell.js` — shell של app worker.
- `WorkerPool` ב-`runtime/web/app_registry.js` — lifecycle management.
- lifecycle dispatch via postMessage.
- pong handler ב-JS-level (לא עובר דרך wasm).
- 4 apps ב-4 workers נפרדים; log breadcrumbs מוכיחים.

### 11.5 Fault containment + watchdog (2 שבועות)
- `LifecycleState::Trapped` (terminal).
- `worker.onerror` + try/catch ב-`app_worker_shell.js` → mark Trapped.
- ping/pong watchdog: 1s interval, 1s timeout.
- Integration tests: panic-app, infinite-loop-app, OOM-app.

### 11.6 OPFS hybrid (1-2 שבועות)
- `akiva_sdk::storage` API async, transparent.
- app-worker storage layer פותח readonly handles ישירות (multiple readonly נתמכים).
- writes routed through kernel-worker (atomic, sandboxed by package_id).
- close-on-uninstall protocol.
- benchmark: read latency direct vs kernel-routed.

### 11.6.5 SDK API completeness pass (3-5 ימים) — **✅ סגור (May 2026)**

**מטרה:** סגירת שני פערים semantically קריטיים ב-SDK שזוהו ב-2026-05-02 audit (אחרי ש-P0/P1 של 11.6 נסגרו). שני הפערים נשמטו ב-Stone 11.2 (התכנון של ה-SDK הסתיים כש-`block_on_ready` עוד היה ה-executor) ולא נחשפו עד שה-executor האמיתי (P0 #4) הפך אותם ל-actionable.

**למה לא 11.7 קודם.** ה-docs של 11.7 מתעדים את ה-API; אם נכתוב docs על TaskScope שלא מבטל ועל host calls שאינם מקבילים, ה-API יקובע ב-bedrock של מפתחים עתידיים והעלות ל-fix תעלה pi-fold.

**למה לפני Phase 12.** Phase 12.4 (App suspension / CACHED state) מניח שscope-based cancellation עובד — בלי זה, "suspend" יוכל רק לעשות `worker.terminate()`. Phase 12.1 (Memory accounting) מניח שapp יכולה להקצות data באופן מציאותי (מקבילי) — בלי זה ה-baseline שגוי. ראה [`docs/isolation-design.md` §10.5 → Phase 12 readiness](docs/isolation-design.md) לdetailed analysis.

**רכיבים:**

- **#11: TaskScope אמיתי** — `Slot.spawned: Vec<SpawnedTask>` עם cancellation ב-on_pause/on_destroy לפי `ScopeKind`. דרישה משלימה: cancellation token שאpps יכולים לקרוא (`ctx.is_cancelled() -> bool`). קוד: `sdk-rust/src/runtime.rs`, `sdk-rust/src/activity.rs:140-168` (TaskScope), `runtime/web/app_worker_shell.js` (re-poll integration).
- **#12: Concurrent host calls per activity** — `Slot.pending` הופך מ-`Option<PendingDispatch>` ל-`Slab` או `HashMap`; dispatch_ids ייחודיים בתוך ה-slot; framework FIFO ל-callback dispatches נשמר אבל user-spawned tasks יכולים להיות מקבילים. קוד: `sdk-rust/src/{executor.rs, runtime.rs}`, `runtime/web/app_worker_shell.js::pendingDispatches`, invariant doc-comment update.

**Definition of Done:**
- [ ] test app spawns infinite-yield task ב-`resumed_scope`, on_pause → ה-task מפסיק להיpoll (counter קבוע אחרי on_pause).
- [ ] `let (a, b) = futures::join!(storage::read("a"), storage::read("b"));` באותה activity — שני reads רצים מקבילים, completion order עצמאי.
- [ ] cancellation אמצע flight: read.await בתוך resumed-scoped task; on_pause → ה-pending dispatch נקרע (אין resource leak).
- [ ] re-entry: read.await ב-`activity_scope` ב-on_create; on_create returns לפני resolution; ה-await ב-on_resume תופס את הbytes (lifetime cross-callback מותר ב-activity scope).
- [ ] ABI עדיין `0.2.0` (לא breaking) — ה-changes הם לkeep-alive contract בwasm side ובshell side, לא ל-import surface.
- [ ] documentation update: `sdk-rust/src/activity.rs` ו-`executor.rs` — את ה-comments שאומרים *"only one in-flight dispatch per handle"* ו-*"renamed block_on_ready"* יש להחליף ב-truth החדשה.

**הערכת זמן:** 3-5 ימי עבודה.

### 11.6.7 Foundation polish backlog (3-5 ימים) — **חוסם 11.7**

**מטרה:** סגירת חמישה rough edges שזוהו ב-architectural audit 2026-05-02 (post-11.6.5). אף אחד מהם אינו lock-in מבני; כולם פעולות פנימיות / additive APIs / refactor של duplication. **כולם חייבים להיסגר לפני 11.7** — אחרת ה-public docs יבעירו את ה-rough edges לbedrock.

**אופי הקלאסטר:** כל פריט הוא scope קטן ועצמאי. ניתן לעבוד עליהם sequenally או במקביל. אין dependencies בין הפריטים.

#### פריט 1 — `WriteHandle` ל-storage (~30 דק')

**הבעיה.** [`sdk-rust/src/storage.rs`](sdk-rust/src/storage.rs) — `pub fn write(key: &str, value: &[u8])` הוא fire-and-forget. App שכותבת notes / settings / drafts לא יודעת אם ה-write נכשל (quota / IO / denied).

**Implementation.**
- `pub fn write(key: &str, value: &[u8]) -> WriteHandle` — מחזיר handle.
- `WriteHandle: Future<Output = Result<(), StorageError>>` — `await` משלים, drop בלי await = fire-and-forget (backward compatible).
- מנגנון: io_start עם op_kind `STORAGE_SET` (קיים), future נpolled עד שה-host_op_done מגיע.
- הקוד עובר דרך ה-async-resumable dispatch protocol של P0 #4 (אותו pattern כמו `storage::read`).

**DoD.**
- [ ] `let _ = storage::write("k", b"v").await?;` עובד באפליקציה אמיתית.
- [ ] write בלי await — ה-handle מושלך, ה-write עדיין מבוצע (test ב-vitest שמאמת ש-host_op יוצא).
- [ ] write עם `STORAGE_SANDBOX` denied — `Err(StorageError::Denied)`.
- [ ] write empty key — `Err(StorageError::InvalidKey)` (ה-host handler כבר מטפל).

**Code surfaces.** `sdk-rust/src/storage.rs`, `sdk-rust/src/runtime.rs` (אם נדרש helper). אין שינוי ב-host handler ([`runtime/web/app_registry.js::_dispatchStorage`](runtime/web/app_registry.js)) — הוא כבר שולח host_op_done.

#### פריט 2 — `pkgs_wire` decoder dedup (~1-2h)

**הבעיה.** wire format זהה decoded ב-3 מקומות: [`framework/src/wire/pkgs.rs`](framework/src/wire/pkgs.rs), [`sdk-rust/src/pkgs.rs`](sdk-rust/src/pkgs.rs), [`runtime/web/app_registry.js::_warmupWorkers`](runtime/web/app_registry.js). drift-risk גובר — כל הוספת שדה ב-`PackageEntry` (icon_ref, version, signing_key) דורשת 3 שינויים מתואמים.

**Implementation (recommended).**
- `framework/src/wire/pkgs.rs` נשאר ה-canonical source.
- `sdk-rust/src/pkgs.rs::decode` הופך wrapper דק שcalls ל-`framework::wire::pkgs::decode` (sdk-rust כבר תלויה ב-framework? לא — sdk-rust בנויה לwasm32, framework לא. אז:** לעבור ל-shared crate `akiva-wire` חדש (no_std + no_alloc-friendly) שגם framework וגם sdk-rust מייבאות).
- ה-JS side ב-`app_registry.js` נשאר נפרד, אבל מקבל schema-comment שmלינק ל-`akiva-wire/src/pkgs.rs` — drift נתפס by reading the comment.
- alternative פשוט יותר: לקבץ את הDecoder ב-`sdk-rust/src/wire/pkgs.rs` (חדש), framework מייבא משם.

**DoD.**
- [ ] decoder יחיד ב-Rust side; שני ה-crates מייבאים אותו.
- [ ] cross_crate parity test הקיים ([framework/src/wire/pkgs.rs](framework/src/wire/pkgs.rs) tests) ממשיך לעבור.
- [ ] schema comment ב-JS שמלינק ל-Rust source.
- [ ] טסט חדש: roundtrip `(framework encode) → (sdk decode)` ו-`(sdk encode) → (framework decode)` — ABI invariance.

**Code surfaces.** New crate או refactor של `sdk-rust/src/pkgs.rs` ו-`framework/src/wire/pkgs.rs`. JS-side: comment + reference fix.

#### פריט 3 — Storage direct readonly handles (~1-2 ימים)

**הבעיה.** היום *כל* `storage::read` עובר round-trip: app worker → kernel worker → OPFS → kernel worker → app worker. ~2ms per read. ה-design v2 ([`docs/isolation-design.md` §3.2](docs/isolation-design.md)) קבע hybrid model: readonly handles ישירות ב-app worker (Chrome 121+ תומך multiple readonly), writes דרך kernel. הtarget היה ~10x speedup.

**Implementation.**
- `runtime/web/app_worker_shell.js` מקבל את ה-`package_id` שלו ב-init (כבר יש).
- ב-init, ה-shell פותח `navigator.storage.getDirectory()` → mode "readonly" → cache handle as `_readDir`.
- `STORAGE_GET` op נwhich: אם יש `_readDir` cache → קורא ישירות עם `getFileHandle().createSyncAccessHandle({ mode: "read-only" })`, מחזיר bytes ב-host_op_done שיוצא ל-self.
- אם הקריאה נכשלת (handle stale, file removed during runtime) — fallback ל-kernel host_op (העתיקה הנוכחית).
- writes (`STORAGE_SET`, `STORAGE_DELETE`) ממשיכים דרך kernel — sandbox enforcement שם.
- close-on-uninstall: kernel שולח `{type:"storage_invalidate"}` ל-worker → ה-worker mots את ה-readonly handle.

**DoD.**
- [ ] benchmark: storage::read direct ≤ 0.5ms; via kernel ≤ 2ms (כפי שמתועד ב-Phase 11 exit criteria).
- [ ] uninstall in-flight: app A קוראת file. app A uninstalled. ה-read שמתבצע נכשל gracefully (Err, לא panic).
- [ ] multiple readonly handles: 3 apps באותו זמן קוראות → אין contention (vitest עם mocked OPFS שמדמה multi-handle).
- [ ] write→read consistency: app SET key, אחר-כך GET key → רואה את הvalue (writes דרך kernel, reads ישירות, הסדר נשמר).

**Code surfaces.** [`runtime/web/app_worker_shell.js`](runtime/web/app_worker_shell.js), [`runtime/web/app_registry.js`](runtime/web/app_registry.js) (storage_invalidate broadcast), [`sdk-rust/src/storage.rs`](sdk-rust/src/storage.rs) (אין שינוי — המעבר שקוף).

#### פריט 4 — Lazy-spawn workers (~חצי יום)

**הבעיה.** [`runtime/web/app_registry.js::_warmupWorkers`](runtime/web/app_registry.js) פותח worker לכל package מותקן ב-boot (Stone 11.4c-6 — pre-warm). עם 4 apps זה fine. עם 100 apps = 100 workers גם אם המשתמש פותח 5. בזבוז זיכרון × 20.

**Implementation.**
- `_warmupWorkers` הופך לbatching של `sys.shell` בלבד (ה-home, תמיד נחוץ).
- ראשון `launch_app(package_id)` של package אחר → spawn-on-demand. ה-launch path הקיים כבר מטפל ב-instantiation-if-missing; הצעד היחיד הוא לא לpre-warm.
- אם performance חשוב לapp-cold-start, אפשר לpre-warm רק apps recently-used (תיעוד ב-OPFS).

**DoD.**
- [ ] vitest: post-boot, AppRegistry יש active_workers count = 1 (`sys.shell`).
- [ ] vitest: post-boot + launch_app("calc"), active_workers = 2.
- [ ] vitest: launch_app + finish + launch_app שוב — worker שתעצמ keep-alive (cache לא נפתחת; ה-worker חי).
- [ ] integration test: app שכבר פעל, finish, launch אחר — startup time ≤ 50ms (cached) ו-≤ 150ms (cold).

**Code surfaces.** [`runtime/web/app_registry.js`](runtime/web/app_registry.js) (`_warmupWorkers`, `launch_app` paths).

#### פריט 5 — Watchdog tunability + warning threshold (~חצי יום)

**הבעיה.** [`runtime/web/config.js`](runtime/web/config.js) — `WATCHDOG.TIMEOUT_MS = 1500` יחיד. אין warning threshold (Android יש: 5s warning ANR, 60s system_server reboot). אין per-package override (debug app רוצה 100ms; long-task app רוצה 5s). אין dev-mode tunability (testes רוצים 200ms).

**Implementation.**
- `config.js` מתרחב:
  ```js
  WATCHDOG = {
    HEARTBEAT_MS: 500,        // כבר יש
    WARN_MS: 1000,            // חדש — log warning
    TIMEOUT_MS: 1500,         // קיים — terminate
  }
  ```
- `globalThis.__akivaConfig.watchdog` — override hook לdev/tests (כבר מתועד ב-config.js שנקרא ע"י `app_registry.js`).
- per-package override (אופציונלי, mutually exclusive עם global): `manifest.runtime.watchdog_timeout_ms` ב-`manifest.toml` — נקרא ע"י `package_manager.rs` ומעבר דרך setPackages ל-AppRegistry.
- ה-watchdog ב-`app_registry.js` פולט `console.warn(...)` ב-`WARN_MS` (רק logged, לא kill); אחרי `TIMEOUT_MS` — הtermination הקיימת.

**DoD.**
- [ ] vitest fake-timers: hang app, advance 1100ms → warning logged, app חי. advance עוד 500ms → terminate.
- [ ] `globalThis.__akivaConfig.watchdog.TIMEOUT_MS = 200` — overrides ב-test, terminate ב-200ms.
- [ ] manifest override: app עם `runtime.watchdog_timeout_ms = 5000` — לא נהרגת ב-1.5s.
- [ ] backward compat: apps בלי manifest override ממשיכים כ-1500ms.

**Code surfaces.** [`runtime/web/config.js`](runtime/web/config.js), [`runtime/web/app_registry.js`](runtime/web/app_registry.js) (watchdog logic), [`framework/src/manifest.rs`](framework/src/manifest.rs) (parse `runtime.watchdog_timeout_ms`), [`framework/src/wire/pkgs.rs`](framework/src/wire/pkgs.rs) (אם רוצים שזה יעבור ל-app workers דרך setPackages — אופציונלי).

#### Definition of Done — Stone 11.6.7 כולה

- [ ] 5 פריטים סגורים, כל אחד עם tests משלו.
- [ ] cargo test green: 414+ Rust tests + 54+ vitest tests (תוספת של ~15-20 חדשים).
- [ ] `docs/sdk-rust.md` §19 מעודכן: §19.3 (WriteHandle) ✅, §19.4 (pkgs_wire) ✅. §19.5/§19.6 כבר ✅ אחרי 11.6.5.
- [ ] `docs/storage.md` §8 מעודכן: deferred ReadHandle הוסר, ה-readonly direct path מתועד.
- [ ] `docs/risks.md` §7.6 (pkgs_wire) ו-§7.7 (magic numbers) ניתן לסמן retired/promoted.

**הערכת זמן Total:** 3-5 ימי עבודה.

**Gate ל-11.7:** אסור לפתוח את 11.7 (akiva new + docs מקיפים) לפני ש-11.6.7 ירוק. ה-docs יבעירו את ה-API לbedrock; כל rough edge שלא נסגר עכשיו יחיה לעולם בעיני מפתחים עתידיים.

### 11.6.8 Foundation primitives gap (1-2 ימים) — **חוסם 11.7**

**מטרה:** סגירת חמישה primitives בסיסיים ב-API שחסרים ב-foundation למרות ש-11.6.5 + 11.6.7 ירוקים. כולם **additive ל-ABI** (לא breaking, לא bumping ABI_VERSION ל-0.3.0 — תוספות הן MINOR ב-policy). כולם foundational במובן שmappים בסיסיים יזדקקו להם, ובלעדיהם אפליקציה שתוכל להיכתב על Akiva היא subset של מה שאפליקציה דומה ל-Android יכולה לעשות.

**אופי הקלאסטר:** כמו 11.6.7 — 5 פריטים עצמאיים, אין dependencies בין הפריטים, אפשר sequential או parallel.

**למה לא קודם.** ה-5 פערים נסתרים — לא מתבטאים ב-tests של 4 ה-MVP apps (calc, hello, tic-tac-toe, infinite-loop). הם נחשפים רק כש-app אמיתי כותב code שמשתמש ב-crypto, או רוצה לדעת איזה תאריך היום, או צריך 2 spawned tasks לתאם ביניהן. May 2026 audit פוסט-11.6.7 חשף את כולם.

#### פריט 1 — CSPRNG / entropy: getrandom custom hook (~45 דק')

**הבעיה.** ב-`grep -nE "rand|getRandomValues" sdk-rust/`: אין שום thing. אין לאפליקציה דרך לקבל bytes אקראיים. wasm לבד אין לו entropy — כל crypto, UUID generation, session token, password salt, anti-CSRF — חסום. iOS, Android, Linux כולם חושפים primitive זה ungated.

**גישה: `getrandom` custom hook — לא module עצמאי.**
הcrate `getrandom` הוא ה-primitive שעליו כל ה-Rust ecosystem מסתמך: `rand`, `uuid`, `ring`, `rustls`, `chacha20poly1305` — כולם קוראים ל-`getrandom::getrandom` תחת המכסה. במקום לבנות `akiva_sdk::random` שרק apps שמכירים אותו ישתמשו בו, מממשים את ה-`getrandom` custom backend — ואז כל ה-crates האלה עובדים אוטומטית בלי שה-app developer צריך לדעת על ה-ABI.

**Implementation.**
- `sdk-rust/Cargo.toml`: הוסף `getrandom = { version = "0.2", features = ["custom"] }`.
- New host import ב-[`sdk-rust/src/runtime.rs`](sdk-rust/src/runtime.rs):
  ```rust
  extern "C" {
      /// Fills [out_ptr, out_ptr+len) with cryptographically secure random bytes.
      /// Backed by crypto.getRandomValues on the JS side.
      /// Returns 0 on success, non-zero on failure (in practice never fails).
      fn akiva_host_random_bytes(out_ptr: *mut u8, len: usize) -> u32;
  }
  ```
- חדש `sdk-rust/src/random.rs` — מממש את ה-custom backend ומוציא thin wrappers:
  ```rust
  use getrandom::register_custom_getrandom;

  fn akiva_getrandom(dest: &mut [u8]) -> Result<(), getrandom::Error> {
      let ret = unsafe { crate::runtime::akiva_host_random_bytes(dest.as_mut_ptr(), dest.len()) };
      if ret == 0 { Ok(()) } else { Err(getrandom::Error::FAILED) }
  }
  register_custom_getrandom!(akiva_getrandom);

  /// Fills `out` with cryptographically secure random bytes.
  pub fn fill(out: &mut [u8]) { getrandom::getrandom(out).expect("RNG unavailable") }

  /// Returns `len` cryptographically secure random bytes.
  pub fn bytes(len: usize) -> Vec<u8> { let mut v = vec![0u8; len]; fill(&mut v); v }
  ```
- JS-side ב-[`runtime/web/app_worker_shell.js`](runtime/web/app_worker_shell.js):
  ```js
  akiva_host_random_bytes: (ptr, len) => {
      crypto.getRandomValues(new Uint8Array(instance.exports.memory.buffer, ptr, len));
      return 0;
  }
  ```
- **No capability gate** — RNG הוא primitive, לא resource. תיעוד מפורש ב-`docs/abi.md` שזה ungated.
- **Side effect:** `rand::random::<u64>()`, `uuid::Uuid::new_v4()`, כל crate שמשתמש ב-`getrandom` — עובד מעכשיו ב-Akiva apps ללא שינוי נוסף.

**DoD.**
- [ ] `let mut buf = [0u8; 32]; random::fill(&mut buf);` ממלא 32 bytes.
- [ ] שתי קריאות → ה-bytes שונים (probabilistic; test עם מספיק bytes שה-collision probability < 2⁻⁶⁴).
- [ ] `uuid = "1"` כ-dev-dependency ב-sdk-rust: `Uuid::new_v4()` מקמפל ורץ ללא כל שינוי.
- [ ] vitest: mock `crypto.getRandomValues` שמחזיר predictable bytes; assert ה-wasm memory מכיל את ה-bytes.
- [ ] doc-test ב-`random.rs`: דוגמת `Uuid::new_v4()` ב-`/// # Examples`.

**Code surfaces.** [`sdk-rust/Cargo.toml`](sdk-rust/Cargo.toml) (getrandom dep), [`sdk-rust/src/runtime.rs`](sdk-rust/src/runtime.rs) (extern), `sdk-rust/src/random.rs` (חדש), [`sdk-rust/src/lib.rs`](sdk-rust/src/lib.rs) (re-export `pub mod random`), [`runtime/web/app_worker_shell.js`](runtime/web/app_worker_shell.js) (host import body), [`docs/abi.md`](docs/abi.md) (תיעוד ב-§ ה-imports).

#### פריט 2 — `MAX_PENDING_PER_SLOT` bound (~30 דק')

**הבעיה.** ב-Stone 11.6.5 #12 השנינו `Slot.pending` ל-`HashMap<u64, PendingDispatch>` — concurrent dispatches per activity. אבל ה-HashMap **unbounded**. App עוין שיעשה `for _ in 0..1_000_000 { ctx.activity_scope().spawn(slow_io()); }` יקצה מיליון future_ids ב-burst אחד, ויצרוך זיכרון unbounded לפני שה-watchdog יתפוס. גם app legitimate שעושה `join_all` של 10K reads — מוצא את עצמו unbounded.

**Implementation.**
- const חדש ב-[`sdk-rust/src/runtime.rs`](sdk-rust/src/runtime.rs):
  ```rust
  /// Maximum concurrent in-flight dispatches per activity slot.
  /// Why: bounds DoS-via-allocation; an app spinning up unbounded
  /// futures should fail loudly, not OOM the worker.
  const MAX_PENDING_PER_SLOT: usize = 256;
  ```
- ב-`install_*` / `run_*_sync` helpers — לפני insertion: `if slot.pending.len() >= MAX_PENDING_PER_SLOT { return STATUS_BUSY; }`.
- ABI status חדש (additive — extends existing status enum ב-op_kinds): `STATUS_BUSY = 3` (after the existing PENDING/READY/CANCELLED).
- [`runtime/web/app_worker_shell.js`](runtime/web/app_worker_shell.js): כשה-status הוא BUSY, log warning + reply ל-host_op_done עם `Err(BUSY)`.
- App-side: `storage::read("k").await` במצב busy מחזיר `Err(StorageError::Busy)` — אפליקציה יכולה להחליט לחזור (backoff) או להחליט ש-task הזה מיותר.
- 256 הוא רחב dummy — מאפשר 100+ legitimate concurrent operations, מספיק נמוך כדי לתפוס spam-loop ב-< 50ms.

**DoD.**
- [ ] vitest test: spam 257 concurrent dispatches → ה-257 חוזר עם BUSY status.
- [ ] vitest test: complete 100 dispatches → 100 חדשים מקובלים (בנייה לא מצטברת).
- [ ] vitest test: 256 dispatches בכל אחד מ-2 slots שונים — שניהם עובדים (per-slot, לא global).
- [ ] sdk test: `Future` שמקבל BUSY מ-`io_poll` resolves ל-`Err(_::Busy)` proper.
- [ ] doc update ב-`executor.rs` invariant comment: "bounded by MAX_PENDING_PER_SLOT."

**Code surfaces.** [`sdk-rust/src/runtime.rs`](sdk-rust/src/runtime.rs), [`sdk-rust/src/op_kinds.rs`](sdk-rust/src/op_kinds.rs) (status enum), [`framework/src/op_kinds.rs`](framework/src/op_kinds.rs), [`runtime/web/app_worker_shell.js`](runtime/web/app_worker_shell.js), [`runtime/web/op_kinds.mjs`](runtime/web/op_kinds.mjs), [`sdk-rust/src/storage.rs`](sdk-rust/src/storage.rs) (StorageError::Busy variant).

#### פריט 3 — SDK concurrency primitives (Channel, Mutex, Signal) (~1-2 ימים)

**הבעיה.** Stone 11.6.5 #11 הביא `TaskScope::spawn`. אבל אין primitives לתאם בין spawned tasks. App ש-spawn 2 tasks — אין `Channel<T>` להעביר נתונים, אין `Mutex<T>` להגן על shared state, אין `Signal<T>` שיודיע על שינוי. Coroutines של Kotlin שולחים `Channel`, `Mutex`, `Flow`, `StateFlow` out of the box. SwiftUI שולח `AsyncStream`, `actor`, `@MainActor`. אצלנו — אין כלום. ה-vision של "DX כמו Compose/SwiftUI" פוגע פה.

**Implementation.**
- New module tree `sdk-rust/src/sync/` עם `mod.rs` שmוצא:
  ```rust
  pub mod channel;
  pub mod mutex;
  pub mod signal;
  pub use channel::Channel;
  pub use mutex::{Mutex, MutexGuard};
  pub use signal::Signal;
  ```
- **Channel** — bounded MPSC (single-producer-single-consumer מספיק לחלק גדול מה-cases ב-single-threaded wasm; אפשר להרחיב למרובה מאוחר):
  ```rust
  pub struct Channel<T, const N: usize> { /* ring buffer + waker slots */ }
  impl<T, const N: usize> Channel<T, N> {
      pub fn new() -> Self;
      pub async fn send(&self, value: T);                 // backpressure when full
      pub async fn recv(&self) -> T;                       // wait when empty
      pub fn try_send(&self, value: T) -> Result<(), Full<T>>;
      pub fn try_recv(&self) -> Option<T>;
  }
  ```
- **Mutex** — futurized (גם בwasm single-threaded זה שימושי כי spawned tasks חולקים ה-event loop):
  ```rust
  pub struct Mutex<T> { /* RefCell + Vec<Waker> */ }
  impl<T> Mutex<T> {
      pub fn new(value: T) -> Self;
      pub async fn lock(&self) -> MutexGuard<'_, T>;
      pub fn try_lock(&self) -> Option<MutexGuard<'_, T>>;
  }
  ```
- **Signal** — single-slot, multiple-waiter (מקביל ל-`mutableStateOf` של Compose):
  ```rust
  pub struct Signal<T> { /* RefCell<Option<T>> + Vec<Waker> */ }
  impl<T: Clone> Signal<T> {
      pub fn new() -> Self;
      pub fn set(&self, value: T);                         // wakes all waiters
      pub async fn wait(&self) -> T;                       // returns latest on change
      pub fn current(&self) -> Option<T>;
  }
  ```
- **Implementation strategy.** Custom (לא embassy-sync) — מסיבות זהות כמו ה-executor: wasm single-threaded לא דורש cross-thread atomics, ה-API tightly-fitted לcase שלנו, ~200-300 שורות סך הכל. חיבור ל-waker mechanism הקיים ב-`crate::executor`. embassy-sync יישאר כ-fallback אם פעם נצטרך wide-scope concurrency.
- **Re-export ב-`lib.rs`:** `pub mod sync;` כך ש-apps כותבים `use akiva_sdk::sync::Channel;`.

**DoD.**
- [ ] **Channel:** producer task `send` 10 items, consumer task `recv` 10 — סדר נכון, ערכים נכונים. test ב-`sdk-rust/src/sync/channel.rs::tests`.
- [ ] **Channel backpressure:** Channel<i32, 4>, send 5 items, ה-5 ממתין; recv אחד, send חוזר ומשלים.
- [ ] **Mutex contention:** 2 spawned tasks `lock().await`, רק אחד מחזיק בכל זמן; test ע"י assertion שה-counter אחרי שניהם == 2 ולא ערך אחר.
- [ ] **Signal multi-waiter:** 3 tasks `wait().await` במקביל, set אחד → כולם wake-up עם הvalue.
- [ ] **Cancellation interaction:** spawn task ב-resumed_scope שעושה `channel.recv().await`; on_pause נקרא; ה-task נקרע. ה-channel לא נשאר ב-state corrupt — recv חדש מ-activity_scope עובד.
- [ ] vitest doc-tests ב-3 ה-modules — דוגמאות מציאותיות (producer-consumer pattern, lock-the-counter, observe-state-change).

**Code surfaces.** New: `sdk-rust/src/sync/{mod.rs, channel.rs, mutex.rs, signal.rs}`. Updated: [`sdk-rust/src/lib.rs`](sdk-rust/src/lib.rs) (re-export), [`sdk-rust/src/executor.rs`](sdk-rust/src/executor.rs) (waker registration helper if needed).

#### פריט 4 — `clock_gettime_ms(ClockId)` — שני clock types כמו POSIX (~45 דק')

**הבעיה.** `ctx.timestamp_ms` הוא monotonic frame-timestamp (קפוא לאורך ה-frame). אין שעה אמיתית. ב-OS אמיתי קיימים שני clock types נפרדים: `CLOCK_MONOTONIC` (לא קופץ לאחור, למדידת elapsed time) ו-`CLOCK_REALTIME` (שעון קיר, לsignposting הודעות, timestamps למשתמש). הם לא ניתנים להחלפה — ו-POSIX מאז 1993 לא מנסה לאחד אותם.

**גישה: extern יחיד עם `clock_id` param — מקביל לPOSIX `clock_gettime(clockid_t, ...)`.**
במקום שני imports נפרדים, extern אחד שמקבל clock ID מספרי. זה extensible (אפשר להוסיף `CLOCK_PROCESS_CPUTIME_ID` בעתיד בלי ABI bump) ומירור נאמן של POSIX.

**Implementation.**
- New host import ב-[`sdk-rust/src/runtime.rs`](sdk-rust/src/runtime.rs):
  ```rust
  extern "C" {
      /// Returns milliseconds for the requested clock.
      /// clock_id 0 = MONOTONIC (performance.now()), 1 = REALTIME (Date.now()).
      fn akiva_host_clock_ms(clock_id: u32) -> u64;
  }
  ```
- New module `sdk-rust/src/clock.rs`:
  ```rust
  /// Which clock to query. Mirrors POSIX clockid_t.
  #[repr(u32)]
  pub enum ClockId {
      /// Monotonic time since an unspecified epoch (process start).
      /// Never jumps backward. Use for measuring elapsed time, timeouts.
      /// Backed by `performance.now()`.
      Monotonic = 0,
      /// Wall-clock time in milliseconds since UNIX epoch.
      /// May jump due to NTP, DST, or manual changes. Use for user-visible timestamps.
      /// Backed by `Date.now()`.
      RealTime  = 1,
  }

  pub fn clock_gettime_ms(id: ClockId) -> u64 {
      unsafe { crate::runtime::akiva_host_clock_ms(id as u32) }
  }
  ```
- JS-side ב-[`runtime/web/app_worker_shell.js`](runtime/web/app_worker_shell.js):
  ```js
  akiva_host_clock_ms: (clockId) => {
      if (clockId === 0) return BigInt(Math.round(performance.now()));
      return BigInt(Date.now()); // REALTIME
  }
  ```
- **No capability gate** — time is primitive, ungated like RNG.
- הבדל מ-`ctx.timestamp_ms`: זה frame-stable (מגיע כ-arg ב-on_render, לא host import), ואין לו ClockId. apps שמדדו elapsed עם `ctx.timestamp_ms` ממשיכים לעשות זאת; `clock_gettime_ms(Monotonic)` הוא call-site-fresh.

**DoD.**
- [ ] `clock_gettime_ms(ClockId::RealTime)` מחזיר ערך ≥ `2026-01-01` כ-unix ms (> 1_735_689_600_000).
- [ ] שתי קריאות סדרתיות ל-`RealTime`: ה-2 ≥ ה-1.
- [ ] `clock_gettime_ms(ClockId::Monotonic)` מחזיר ערך > 0 ועולה בין קריאות.
- [ ] vitest mock: `Date.now` mocked לערך קבוע, `clockId=1` מחזיר אותו; `performance.now` mocked, `clockId=0` מחזיר אותו.
- [ ] doc-test ב-`clock.rs` שמראה note-with-timestamp + elapsed-time pattern.
- [ ] `docs/abi.md` import section: `akiva_host_clock_ms(clock_id: u32) -> u64` עם טבלת clock_id values.

**Code surfaces.** [`sdk-rust/src/runtime.rs`](sdk-rust/src/runtime.rs) (extern), `sdk-rust/src/clock.rs` (חדש), [`sdk-rust/src/lib.rs`](sdk-rust/src/lib.rs) (re-export `pub mod clock`), [`runtime/web/app_worker_shell.js`](runtime/web/app_worker_shell.js) (host import body), [`docs/abi.md`](docs/abi.md) (§ imports).

#### פריט 5 — ABI versioning policy formalization (~30 דק', no code)

**הבעיה.** `ABI_VERSION = "0.2.0"` קיים ב-`sdk-rust/src/lib.rs` ו-`framework/src/lib.rs`. mismatch loudly fails-on-load. אבל **אין כתוב מתי `0.X` → `1.0` קורה, מה נחשב breaking, מי gate-r, מה הקריטריון לpromotion.** מתועד ב-[`docs/risks.md` §3](docs/risks.md) כ"premature to formalize" — אבל לפני ש-Stone 11.7 פותח את ה-public docs, ה-policy חייב להיכתב, אחרת ה-public docs ייקבעו implicit policy שאי אפשר לסגת ממנה.

**Implementation.** New section `## 1.X ABI versioning policy` ב-[`docs/abi.md`](docs/abi.md). תוכן:

- **In scope של ABI_VERSION:** extern function signatures (גם ב-app side וגם ב-host side), op_kind constants, status code constants, wire format byte-layouts (intent, paint, save_state, pkgs, result), capability constants, all `MAX_*_BYTES` constants ב-public types.
- **NOT in scope:** SDK Rust API ergonomics (function names, error types names, doc comments), internal kernel implementation, framework internal logic, `runtime/web` JS internals, test harness.
- **Bump rules:**
  - **PATCH** (`0.X.Y` → `0.X.(Y+1)`): bug fix in implementation, contract unchanged. כדאי לשמור unused עד 1.0 — בעידן 0.X לא נשתמש.
  - **MINOR** (`0.X.Y` → `0.(X+1).0`): additive change — new op_kind, new import, new capability, new field appended to a wire format that's documented as "may be extended." Old code remains compatible.
  - **MAJOR** (`0.X` → `(X+1).0` ב-pre-1.0; `X.Y` → `(X+1).0` post-1.0): breaking change. Apps must rebuild against the new ABI. Includes: removing or renaming a primitive, changing type of an arg, reordering wire format fields, changing semantics of an existing op_kind.
- **Promotion ל-1.0:**
  - Phase 11 + 12 + 13 exit criteria all green.
  - 6+ months without a MAJOR bump.
  - First third-party app (signed by external party) installed end-to-end without manual ABI patches.
  - Streams primitive is stable (or explicitly deferred to 1.X).
  - Stone 11.7's docs at maturity (no "WIP" markers).
- **Review process (current — single author):** any ABI bump documented in (a) commit message body, (b) the relevant design doc (`isolation-design.md` for Phase 11, `windowing-design.md` for Phase 13, etc.), (c) `CHANGELOG.md` entry under "ABI changes."
- **Review process (future — ≥2 contributors):** RFC-style proposal in `docs/abi-proposals/<NNN>-<slug>.md` describing change + impact + migration path. 1-week review window. Approval = at least 2 maintainers signed-off ב-comment.
- **CHANGELOG.md ב-root** — חדש (אופציונלי, אבל המומלץ עכשיו). מפלס entries לפי semver, ABI changes מסומנים `[ABI]`.

**DoD.**
- [ ] `docs/abi.md` כולל את ה-policy החדש כ-§1.X (לאחר §1 הקיים, לפני §2).
- [ ] [`docs/risks.md` §3](docs/risks.md) (JS-side ABI governance) ניתן לסמן ✅ — promoted to abi.md.
- [ ] [`CHANGELOG.md`](CHANGELOG.md) חדש ב-root עם entry יחיד שמתעד את ה-ABI bumps של 0.1.0 → 0.2.0 (P0 #4 + Stone 11.6.5) + 0.2.0 → 0.3.0 (Stone 11.6.8: + `akiva_host_random_bytes`, + `akiva_host_clock_ms`, + `STATUS_BUSY`).
- [ ] commit message convention documented: כל commit שמשנה ABI כולל `[ABI: MINOR]` או `[ABI: MAJOR]` ב-subject line.

**Code surfaces.** [`docs/abi.md`](docs/abi.md) (new section), [`CHANGELOG.md`](CHANGELOG.md) (new file ב-root), [`docs/risks.md`](docs/risks.md) §3 (mark resolved).

#### Definition of Done — Stone 11.6.8 כולה

- [ ] 5 פריטים סגורים, כל אחד עם tests משלו.
- [ ] ABI bumped ל-`0.3.0` (פריטים 1, 2, 4 הם MINOR additions; פריט 3 הוא pure SDK; פריט 5 הוא תיעוד policy).
- [ ] cargo test green: 414+ Rust tests + 54+ vitest tests (תוספת של ~12-18 חדשים).
- [ ] `docs/sdk-rust.md` §19 מעודכן: §19.X חדש לכל primitive חדש (random, clock, sync).
- [ ] `docs/abi.md` מעודכן: imports list (`akiva_host_random_bytes`, `akiva_host_clock_ms` + clock_id table), status codes (BUSY), versioning policy.
- [ ] `docs/risks.md` §3 (ABI governance) → ✅ resolved.

**הערכת זמן Total:** 1-2 ימי עבודה.

**Gate ל-11.7:** אסור לפתוח את 11.7 לפני ש-11.6.8 + 11.6.9 ירוקים. הסיבה זהה ל-11.6.7 — public docs יבעירו את ה-API לbedrock; primitives שחסרים יחיו לעולם בעיני מפתחים שיתעדו "Akiva = Android אבל בלי RNG / wall clock / channels."

### 11.6.9 Rendering + platform polish (חצי יום) — **חוסם 11.7**

**מטרה:** שלושה פערים שהתגלו ב-audit מאי 2026 — אחד ויזואלי (פינות מעוגלות), אחד correctness ב-resize, ואחד dark mode אוטומטי. שלושתם additive ולא breaking. `OP_FILL_ROUND_RECT` הוא MINOR ABI bump (opcode חדש). ResizeObserver ו-matchMedia הם JS-only.

**למה לא אחרי 11.7.** כשה-docs של 11.7 ייכתבו עם screenshots ו-scaffold templates, הם יצלמו את ה-look הנוכחי — כפתורים עם פינות חדות, dark mode ידני. תיקון אחרי כן דורש עדכון docs + templates + screenshots. לפני 11.7 — עלות חצי יום. אחרי — פי כמה.

**אופי הקלאסטר:** 3 פריטים עצמאיים, אין dependencies בין הפריטים.

#### פריט 1 — `OP_FILL_ROUND_RECT` (~1h)

**הבעיה.** כל ה-UI elements (כפתורים, כרטיסים, פנלים) מרובעי פינות לחלוטין. `roundRect()` הוא Baseline 2023 (Chrome 99, Firefox 112, Safari 15.4) — קיים בכל דפדפן שתומך ב-OffscreenCanvas. כל OS עכשווי (Android, iOS, macOS, Windows) משתמש בפינות מעוגלות כשפת עיצוב ברירת מחדל. בלי זה, apps ב-Akiva נראות כמו web-app משנות ה-90, לא כמו native OS.

**Implementation.**
- New variant ב-[`sdk-rust/src/canvas.rs`](sdk-rust/src/canvas.rs):
  ```rust
  OP_FILL_ROUND_RECT: u8 = 7;
  PaintOp::FillRoundRect { x: f32, y: f32, w: f32, h: f32, radius: f32, rgba: u32 }
  ```
- `encode_op` + `decode_op` ב-[`framework/src/wasm_activity.rs`](framework/src/wasm_activity.rs): x/y/w/h/radius כ-f32 LE, rgba כ-u32 LE.
- [`runtime/web/renderer/canvas2d.js`](runtime/web/renderer/canvas2d.js):
  ```js
  fillRoundRect(x, y, w, h, radius, rgba) {
      this.ctx.fillStyle = rgbaToCss(rgba);
      this.ctx.beginPath();
      this.ctx.roundRect(x, y, w, h, radius);
      this.ctx.fill();
  }
  ```
- [`runtime/web/worker.js`](runtime/web/worker.js): `hal_draw_round_rect: (x, y, w, h, r, rgba) => backend?.fillRoundRect(x, y, w, h, r, rgba >>> 0)`.
- SDK widget toolkit ([`sdk-rust/src/ui/paint.rs`](sdk-rust/src/ui/paint.rs)): כפתורים מקבלים ברירת מחדל `radius: 8.0`. ניתן לoverride ב-AKML attribute `cornerRadius`.

**DoD.**
- [ ] `Canvas::fill_round_rect(x, y, w, h, radius, rgba)` מקמפל ב-wasm32.
- [ ] encode/decode roundtrip test ב-`framework/src/wasm_activity.rs::tests` — byte-for-byte symmetric.
- [ ] calculator buttons ו-shell buttons נראים עם פינות מעוגלות בדפדפן.
- [ ] `radius=0.0` → שטח זהה ל-`fill_rect` (regression test).
- [ ] AKML `cornerRadius="12"` attribute מפורש ב-`inflate.rs`, עובד על `<Button>` ו-`<Rect>`.

**Code surfaces.** [`sdk-rust/src/canvas.rs`](sdk-rust/src/canvas.rs), [`framework/src/wasm_activity.rs`](framework/src/wasm_activity.rs), [`runtime/web/renderer/canvas2d.js`](runtime/web/renderer/canvas2d.js), [`runtime/web/worker.js`](runtime/web/worker.js), [`sdk-rust/src/ui/paint.rs`](sdk-rust/src/ui/paint.rs), [`sdk-rust/src/akml/inflate.rs`](sdk-rust/src/akml/inflate.rs).

#### פריט 2 — `ResizeObserver` עם `devicePixelContentBoxSize` (~30 דק')

**הבעיה.** [`runtime/web/tab.js:228`](runtime/web/tab.js#L228) — `window.addEventListener("resize", ...)`. זה מחמיץ:
- שינוי zoom level (Ctrl+/-) — DPR משתנה בלי resize event, ה-canvas נשאר ברזולוציה שגויה.
- `devicePixelContentBoxSize` נותן pixels פיזיים מדויקים ישירות, ללא צורך בכפל DPR ידני (Baseline 2022: Chrome 84+, Firefox 93+, Safari 15.4+).

**Implementation.**
```js
// מחליף את installResizeHandler ב-tab.js
new ResizeObserver(entries => {
    const s = entries[0].devicePixelContentBoxSize?.[0];
    const width_px  = s ? s.inlineSize  : Math.round(canvas.clientWidth  * dpr());
    const height_px = s ? s.blockSize   : Math.round(canvas.clientHeight * dpr());
    const m = { width_px, height_px, density: dpr() };
    state.metrics = m;
    if (state.ownsCanvas) applyCanvasSize(canvas, m);
    session.send({ type: "resize", tabId, metrics: m });
}).observe(canvas);
```
ה-`window.addEventListener("resize")` נמחק.

**DoD.**
- [ ] שינוי zoom level (Ctrl+/-) → canvas נרנדר ברזולוציה הנכונה ללא reload.
- [ ] `window.addEventListener("resize")` מוסר מ-`installResizeHandler`.
- [ ] בדיקה ידנית: פתיחה ב-125% zoom → UI חד, לא מטושטש.

**Code surfaces.** [`runtime/web/tab.js`](runtime/web/tab.js) בלבד.

#### פריט 3 — `matchMedia('prefers-color-scheme')` → auto dark mode (~1h)

**הבעיה.** ResourceManager תומך ב-`values-night/` qualifier אבל **אף פעם** לא מופעל אוטומטית מהגדרת ה-OS. אפליקציות ב-Akiva לא עוברות ל-dark mode גם אם המשתמש הגדיר night mode ב-OS.

**Implementation.**
- [`runtime/web/tab.js`](runtime/web/tab.js):
  ```js
  const mq = window.matchMedia('(prefers-color-scheme: dark)');
  const sendColorScheme = () => session.send({ type: "config-change", night: mq.matches });
  mq.addEventListener('change', sendColorScheme);
  sendColorScheme(); // ערך נוכחי ב-boot
  ```
- [`runtime/web/worker.js`](runtime/web/worker.js): `case "config-change"` → `instance.exports.akiva_config_change(event.night ? 1 : 0)`.
- Framework: `kernel/src/lib.rs` — `akiva_config_change(night: i32)` export → `DisplayManager::set_night_mode(night != 0)` → event ל-`ResourceManager::reload_qualifiers()` בframe הבא.

**DoD.**
- [ ] OS ב-dark mode + `akiva run` → calculator + shell מציגים `values-night/colors.toml`.
- [ ] שינוי dark/light mode בזמן ריצה (system settings) → apps מתעדכנות בframe הבא, ללא reload.
- [ ] OS ב-light mode → `values/colors.toml` ברירת מחדל (regression test).

**Code surfaces.** [`runtime/web/tab.js`](runtime/web/tab.js), [`runtime/web/worker.js`](runtime/web/worker.js), [`kernel/src/lib.rs`](kernel/src/lib.rs) (export), framework device config.

#### Definition of Done — Stone 11.6.9 כולה

- [ ] 3 פריטים סגורים עם tests.
- [ ] ABI bumped ל-`0.4.0` (פריט 1 הוא MINOR: opcode 7 חדש; פריטים 2+3 הם JS-only, לא ABI).
- [ ] `docs/abi.md` imports: `hal_draw_round_rect` + opcode 7 מתועד.
- [ ] `CHANGELOG.md` entry: 0.3.0 → 0.4.0, `[ABI: MINOR]`.
- [ ] calculator + shell buttons נראים native בדפדפן.

**הערכת זמן Total:** חצי יום.

**Gate ל-11.7:** 11.6.8 + 11.6.9 ירוקים לפני פתיחת 11.7. ה-docs + scaffold templates של 11.7 יצלמו את ה-look — פינות מעוגלות + dark mode אוטומטי חייבים להיות שם ברגע שה-docs נכתבים.

### 11.7 `akiva new` + dev tooling polish (1-2 שבועות) — **DX foundation**
- `akiva new <name>` — scaffold app (Cargo.toml, manifest.toml, src/lib.rs hello-world async, README).
- 3 templates: `default`, `compose-style`, `game`.
- `akiva docs` — מציג SDK docs רלוונטיים.
- Documentation pass: כל public API ב-`akiva-sdk` עם `///` doc comments + example.

### 11.8 Performance pass + benchmarks (1 שבוע)
- `docs/perf-baseline.md` — מדידות before/after isolation.
- Optimizations critical paths אם נחוצות (batching של logs, transferable canvas ops).
- 60fps יציב עם 3 apps ב-foreground.

**Exit criteria:**
- [ ] 4 apps רצים ב-4 workers נפרדים. console.log מוכיח.
- [ ] App עם `loop {}` ב-`on_pointer` נהרגת ב-≤ 1.5s; Shell חי.
- [ ] App עם `panic!()` → Trapped → cleanup → Shell ב-Resumed.
- [ ] **141 framework + 146 sdk + 68 kernel + 50 vitest tests** עוברים. צפויה תוספת של 30-50.
- [ ] host call overhead ≤ 1ms.
- [ ] storage read direct ≤ 0.5ms; write through kernel ≤ 2ms.
- [ ] `akiva new my-app && cd my-app && akiva run` — hello על המסך תוך 30 שניות.
- [ ] `docs/sdk-rust.md` מעודכן עם async API + 3 דוגמאות מקיפות.
- [ ] `docs/perf-baseline.md` קיים.

**הערכת זמן:** 13-19 שבועות. **זה ה-foundation rebuild הגדול של הפרויקט.** אם זה מתבצע איכותי — Akiva מרגישה כמו OS אמיתי גם למפתחים וגם לאפליקציות. אם נחפף — לא נדע אם נצליח לבנות 100 apps עליו.

---

## שלב 12 — Resource Governance

**מטרה:** מערכת יודעת כמה memory וכמה CPU כל app צורך, ויכולה לעצור או להקפיא apps שעוברים את ה-quota.

**✅ Precondition met:** Stone **11.6.5** נסגר ב-May 2026 — TaskScope אמיתי + slot.pending HashMap. ראה [`docs/isolation-design.md` §10.5 Phase 12 readiness](docs/isolation-design.md) לסיכום הfix.

**תלוי ב:** Phase 11 (בלי workers נפרדים, CPU attribution בלתי אפשרית ו-memory eviction מסוכנת) **+ Stone 11.6.5** (semantic foundation ל-suspension).

**רכיבים שנבנים:**

### 12.1 Per-app memory accounting ✅ (May 2026)
- `WebAssembly.Memory.buffer.byteLength` per instance → `MemoryManager::record(package_id, bytes)` בכל grow
- API: `fw.memory_usage(package_id) -> UsageReport { current_bytes, peak_bytes, limit_bytes }` עם `over_budget()`
- Sampling: app workers שולחים byteLength על pong; AppRegistry stashes; kernel HAL `hal_app_memory_current` per frame; `Framework::record_app_memory` כל frame דרך `sample_app_resources_into`.
- Code: `framework/src/memory_manager.rs`, `framework/src/lib.rs::{record_app_memory, memory_usage}`, `kernel/src/{lib.rs::sample_app_resources_into, host.rs}`, `runtime/web/{app_worker_shell.js, app_registry.js, worker.js}`.

### 12.2 Per-app CPU attribution ✅ (May 2026)
- App workers brackets every wasm-export call (dispatch/poll/get_result/drive_spawned) עם `performance.now()`, מצטבר ל-`cumulativeCpuMs` ומדווח על pong.
- Kernel-side: HAL `hal_app_cpu_ms`, framework `CpuBudget` עם 1-sec rolling window, `CpuReport { cumulative_ms, recent_ms_per_sec, peak_ms_per_sec, frame_budget_fraction }` ו-`over_frame_budget()` (שמתפרץ מעל 0.5 של תקציב 60Hz).
- API: `fw.cpu_usage(package_id) -> CpuReport`. `iter()` ל-LMK ranking ב-12.3.
- Code: `framework/src/cpu_budget.rs`, `framework/src/lib.rs::{record_app_cpu, cpu_usage}`, `kernel/src/{lib.rs::sample_app_resources_into, host.rs::hal_app_cpu_ms}`, `runtime/web/{app_worker_shell.js::chargeCpu, app_registry.js::cpuMs, worker.js}`.

### 12.3 Low-Memory Killer policy
- Priority tiers בסגנון Android: FOREGROUND > VISIBLE > PAUSED > BACKGROUND > EMPTY
- נקבעים לפי `LifecycleState` + מיקום ב-`task_history`
- Threshold-based: כש-total memory (or system pressure signal מה-דפדפן) עובר סף → kill the lowest-priority living app
- Kill = worker.terminate() + `ActivityManager::destroy_all_for_package`

### 12.4 App suspension (soft eviction)
- שונה מ-destroy: שמור state (saved_state blob), פרוק wasm instance, השאר metadata
- רvive ב-`launch`: instantiate מחדש, restore state
- זה המודל ש-Android קורא לו "process state: CACHED"

### 12.5 Framework enforcement
- `manifest.runtime.max_memory_mb` נאכף בפועל: grow שיעבור אותו → trap
- Frame budget watchdog (מ-Phase 11.6) גם מעדכן את ה-CPU accounting

**Exit criteria:**
- [ ] אפליקציה שמקצה 100MB ממשיכה לרוץ; אפליקציה שמקצה 5GB נהרגת עם הודעה ברורה
- [ ] 3 apps רצים, לחץ memory מלאכותי → ה-LMK הורג את ה-least-priority
- [ ] suspension → revival שומר saved_state ומחזיר את ה-app למצב קודם

**הערכת זמן:** 3-5 שבועות.

---

## שלב 13 — WindowManager completion

**מטרה:** מעבר מ-fullscreen-only ל-layered display: overlays, dialogs, transitions, focus routing.

**תלוי ב:** לא תלוי ב-Phase 11/12 (יכול לרוץ במקביל). מוזכר אחרון כי חשיבותו כ-OS-core קטנה יותר.

**אופי העבודה:** כולה Rust/framework-internal — לא דורש browser APIs חדשים. Canvas2D כבר תומך בכל הנדרש (transform stack, z-ordering דרך paint order). כל stone עצמאי; אפשר לעבוד sequentially.

---

### Stone 13.0prep — Foundation primitives שחסרים (~2-3 ימים) ✅ סגור (May 2026)

audit פוסט-11.6.9 (May 2026) חשף שלושה primitives שלא חוסמים את Phase 12 אבל **חוסמים את 13.x** — עדיף לסגור אותם בפתח Phase 13 לפני שנכנסים ל-WindowManager עצמו.

**סטטוס:** A+B+C נסגרו (commits `fe9dbdc` timer / `a0cea8d` stroke ops / `1c96321` C1 wire / `5301cd3` C2 PNG decode / `a7d28da` C3 AKML widget / `d9a8f90` ABI 0.7.0). יוצא דופן: ה-Shell launcher icons עדיין לא מציגים את ה-`app.icon` (ה-data זורם דרך ה-pipeline אבל Shell לא קורא את השדה — ה-visual integration שייך ל-Stone 13.2). cancel-on-drop ל-Sleep futures מתועד כ-known limitation.

#### A. Timer primitives (~1 יום) — **חוסם 13.3**

**הבעיה.** ה-SDK חושף `clock_gettime_ms` (Stone 11.6.8 #4) — אבל זה polling. אין primitive שpark future למשך N ms ומעיר אותו. App שרוצה `sleep(500).await` או `timeout(read("k"), 2000).await` חייבת busy-loop על `clock_gettime`, דבר שפיר ה-CPU budget של Stone 12.2. Stone 13.3 (animations) דורש waker-on-tick — לא ניתן לבנות בלי timer.

**Implementation.**
- Host import חדש ב-[`sdk-rust/src/runtime.rs`](sdk-rust/src/runtime.rs):
  ```rust
  /// Arms a one-shot timer. After `delay_ms`, host fires the future.
  /// Returns 0 on success, non-zero on failure (per-slot quota exceeded).
  fn akiva_host_timer_arm(future_id: u64, delay_ms: u32) -> u32;
  fn akiva_host_timer_cancel(future_id: u64);
  ```
- New module `sdk-rust/src/time.rs`:
  ```rust
  pub async fn sleep(duration_ms: u32);
  pub async fn timeout<F: Future>(fut: F, ms: u32) -> Result<F::Output, Elapsed>;
  pub fn interval(period_ms: u32) -> Interval; // poll-driven
  ```
- JS-side ב-[`runtime/web/app_worker_shell.js`](runtime/web/app_worker_shell.js): `Map<future_id, setTimeout_handle>`. Drop-on-await → `clearTimeout` + erase מהmap.
- ABI bump: 0.4.0 → 0.5.0 (MINOR — additive imports).

**DoD.**
- [x] `sleep(100).await` returns אחרי ≥ 100ms — backed by setTimeout, host caps at 60s
- [x] `timeout(slow_io(), 50).await` → `Err(Elapsed)` — manual select! בתוך poll_fn
- [ ] drop של Sleep future לפני wake → ה-timer מבוטל ב-host — **deferred**, known limitation. Sleep dropped before fire leaves a stale futures entry until worker dies. Bounded by `MAX_PENDING_FUTURES = 256`. Honest cancel-on-drop needs an `io_cancel` host import that doesn't exist yet.
- [x] `interval(period_ms).tick().await` works as a chained sleep helper

#### B. Stroke + line paint ops (~3 שעות) — נצרך ב-13.0/13.2

**הבעיה.** Canvas2D primitives היום: `Clear`, `FillRect`, `FillRoundRect`, `DrawText`, `PushClip/Pop`, `PushTransform/Pop`. אין stroke או line. דיאלוגים ב-13.2 (borders), separators ב-13.0 (panel divider), outlined buttons — כולם דורשים stroke. אפשר לחקות ב-1px-tall fill_rect אבל זה מכוער ולא תומך ב-radius.

**Implementation.**
- 3 opcodes חדשים ב-[`sdk-rust/src/canvas.rs`](sdk-rust/src/canvas.rs): `OP_STROKE_RECT = 8`, `OP_STROKE_ROUND_RECT = 9`, `OP_DRAW_LINE = 10`. כל אחד עם width param (f32).
- `encode_op` / `decode_op` ב-[`framework/src/wasm_activity.rs`](framework/src/wasm_activity.rs).
- [`runtime/web/renderer/canvas2d.js`](runtime/web/renderer/canvas2d.js): `strokeRect`, `roundRect`+`stroke`, `moveTo`+`lineTo`+`stroke`.
- ABI bump: 0.5.0 → 0.6.0 (MINOR — additive opcodes).

**DoD.**
- [x] encode/decode roundtrip בכל ה-3 opcodes — `outline_ops_roundtrip_byte_identical` + `outline_ops_wire_sizes_match_specification` framework tests pin the wire sizes (25/29/25 bytes).
- [x] `Canvas::stroke_rect / stroke_round_rect / draw_line` מקמפלים ב-wasm32 — sdk-rust release builds clean against the additions.
- [ ] Visual sanity: כפתור outlined עם radius=8 + lineWidth=2 — **TBD**, no app uses outlined buttons yet. The pipeline works (canvas2d.js strokeRoundRect via roundRect+stroke); first visible use lands when Stone 13.2 dialog borders ship.

#### C. `@drawable/` + image rendering (~1 יום) — נצרך ב-13.2 ל-icons + Shell

**הבעיה.** `@drawable/` resource type מתועד ב-`docs/resources.md §8` כ"פוסט-MVP". בלי זה אין PNG icons — Shell מציגה כפתורי launcher עם טקסט בלבד; דיאלוגים של 13.2 לא יכולים להראות icon (warning/info/error). זה הופך את "Akiva נראית כמו native OS" ל-vision לא-ממומש בעיני המשתמש.

**Implementation.**
- ResourceManager: load `res/drawable/<name>.png` ב-boot per package, cache כ-`ImageBitmap` ב-renderer (transferable).
- Host import: `akiva_host_resource_drawable(name_ptr, name_len) -> u32` (drawable_id; 0 = missing).
- New opcode `OP_DRAW_IMAGE = 11`: `(drawable_id: u32, x: f32, y: f32, w: f32, h: f32)`.
- AKML attribute `<Image src="@drawable/icon" w="48" h="48">`.
- Manifest extension: `app.icon = "@drawable/launcher"` (Stone 13.2 משתמש בזה ב-Shell).
- ABI bump: 0.6.0 → 0.7.0 (MINOR — opcode + import).

**DoD.**
- [x] PNG מ-`apps/calculator/res/drawable/icon.png` נטען ב-boot, מצוייר במסך דרך `<Image>` widget — calculator's display HStack carries `<Image src="@drawable/icon" w="24" h="24" />`; the orange 16×16 brand mark paints next to the readout on every frame.
- [x] missing drawable → magenta placeholder — `canvas2d.js::drawImage` falls through to `fillStyle="#ff00ff"` for unregistered ids (parallel to MISSING_COLOR).
- [ ] Shell מציגה icons ל-launcher entries — **deferred to Stone 13.2.** The data flows: `manifest.app.icon` parses + validates as `@drawable/<name>`; `pkgs_wire` would need to extend with an icon field for Shell to read it cross-process. Held back so Shell visual changes ride together with the rest of 13.2 dialog/launcher work.
- [x] uninstall → ה-cache נוקה — `Canvas2DBackend.forgetDrawable(id)` wired; not exercised yet because explicit uninstall isn't end-to-end.

**Code surfaces (כל ה-3 פריטים).** [`sdk-rust/src/{runtime.rs, time.rs, canvas.rs, lib.rs}`](sdk-rust/src/), [`framework/src/{wasm_activity.rs, resources.rs, manifest.rs}`](framework/src/), [`runtime/web/{app_worker_shell.js, renderer/canvas2d.js, worker.js}`](runtime/web/), [`docs/{abi.md, resources.md}`](docs/).

**DoD — Stone 13.0prep כולה.**
- [x] ABI bumped ל-`0.7.0`. CHANGELOG entry — see `CHANGELOG.md` `[0.7.0]` block, commits `d9a8f90` (sdk + framework constants) and the C1/C2/C3 commits that added the surface.
- [ ] `docs/sdk-rust.md` עם `time`, stroke ops, drawable דוגמאות — **partial**: sdk-rust.md hasn't been refreshed to mention §13/§14 worth of new APIs since 11.7. Stone 11.7 followup task left it incomplete. Recommended quick add: a §10b "13.0prep additions" section that points at `time::sleep`, `Canvas::stroke_*`, `<Image>`, and `res::drawable`.
- [x] regression: 4 ה-MVP apps ממשיכות לעבוד — Calculator visually tested with the new `<Image>` element on the live demo; hello, shell, tic-tac-toe rebuild + run unchanged (they don't reference any 13.0prep additions, so the additive ABI bump leaves them backward-compatible).

**הערכת זמן:** 2-3 ימי עבודה.

---

### Stone 13.0 — Window surface model (~1 שבוע) ✅ סגור (May 2026)

**הבעיה.** [`framework/src/window_manager.rs`](framework/src/window_manager.rs) הוא stub — מחזיר `()` על הכל. Compositor עובד עם `FrameCommands` יחיד גלובלי. אין concept של "surface" נפרד מ-"activity" — כל הציור הולך לאותו buffer.

**מה נבנה.**

`WindowLayer` enum ב-[`framework/src/window_manager.rs`](framework/src/window_manager.rs):
```rust
#[repr(u32)]
pub enum WindowLayer { Wallpaper = 0, App = 100, Overlay = 200, System = 300 }
```

`SurfaceId(u32)` — opaque handle. `WindowManager` מחזיק `Vec<SurfaceRecord>`:
```rust
struct SurfaceRecord {
    id: SurfaceId, owner: AppHandle, layer: WindowLayer,
    bounds: Rect, visible: bool,
}
```

API חדש:
```rust
pub fn create_surface(&mut self, owner: AppHandle, layer: WindowLayer) -> SurfaceId
pub fn destroy_surface(&mut self, id: SurfaceId)
pub fn set_bounds(&mut self, id: SurfaceId, rect: Rect)
pub fn set_visible(&mut self, id: SurfaceId, visible: bool)
pub fn focus(&mut self, id: SurfaceId) -> Option<SurfaceId>  // returns old focus
pub fn focused(&self) -> Option<SurfaceId>
pub fn surfaces_in_z_order(&self) -> Vec<SurfaceId>  // sorted by layer+creation_order
```

`Framework::render_frame` מקצה `FrameCommands` per-surface במקום global אחד. כל activity ציור הולך ל-surface שלה.

**DoD:**
- [x] `WindowManager::create_surface` / `destroy_surface` + 11 unit tests (layer ordering, visibility, focus stack, owner filter, none-sentinel)
- [x] `surfaces_in_z_order` מחזיר לפי layer value, creation-order כ-tiebreak
- [x] Calculator + Shell ממשיכים לעבוד — stub replaced, same behavior

---

### Stone 13.1 — Compositor z-ordering (~3 ימים) ✅ סגור (May 2026)

**הבעיה.** [`framework/src/compositor.rs`](framework/src/compositor.rs) — `Compositor` מחזיק `FrameCommands` יחיד. הוספת surface שני היא undefined — אין הגדרה איזה z הוא מעל.

**מה נבנה.**

`Compositor` עובר מ-`frame: FrameCommands` ל-`surfaces: HashMap<SurfaceId, FrameCommands>`.

```rust
pub fn begin_surface(&mut self, id: SurfaceId) { self.surfaces.insert(id, FrameCommands::new()); }
pub fn surface_target(&mut self, id: SurfaceId) -> &mut FrameCommands { ... }
pub fn finish_frame(&self, order: &[SurfaceId]) -> FrameCommands {
    // concatenate ops in z-order — flat stream for the HAL
    let mut out = FrameCommands::new();
    for id in order { if let Some(fc) = self.surfaces.get(id) { out.extend(fc); } }
    out
}
```

`Framework::render_frame` שואל `WindowManager::surfaces_in_z_order()` → passes to `Compositor::finish_frame`. Result: single flat `FrameCommands` → HAL (renderer unchanged).

**DoD:**
- [x] שתי surfaces: app + overlay → overlay ops appear last in output (drawn on top) — exercised live in Stone 13.2
- [x] `destroy_surface` → removed from `surfaces` map; no dangling entries
- [x] Performance: finish_frame ≤ 0.5ms for 4 active surfaces (no per-frame allocs beyond ops copy) — `Vec::clear` keeps capacity, copy bounded by total ops

---

### Stone 13.2 — System dialog overlays (~3 ימים) ✅ סגור (May 2026)

**הבעיה.** אין dialog API. App שרוצה "confirm?" צריך לצייר ולנהל focus בעצמו — ואין isolation שמונע מ-app שמאחורי ה-dialog לקבל pointer events.

**מה נבנה.**

SDK API ב-[`sdk-rust/src/ctx.rs`](sdk-rust/src/ctx.rs):
```rust
pub fn show_dialog(
    &mut self, title: &str, body: &str, buttons: &[&str],
) -> Future<DialogResult>

pub enum DialogResult { Button(usize), Dismissed }
```

Framework: `SystemDialogRenderer` — מנוהלת ב-framework, לא ב-app wasm. מצייר dialog box ישירות על `WindowLayer::Overlay` surface שלו, כולל background dim על ה-App surface שמתחתיה.

Focus routing: `WindowManager::focus(dialog_surface)` → `Framework::dispatch_pointer` בודק `focused()` — אם overlay פתוח, events מגיעים רק לdialog. Tap מחוץ ל-`dialog_bounds` → `DialogResult::Dismissed`. App מאחורי עובר ל-`paused-but-visible`: ממשיך לצייר (מוצג dimmed), לא מקבל input.

**DoD:**
- [x] Calculator: `ctx.show_dialog("Clear?", "This will reset the calculator.", &["Cancel", "OK"])` → dialog מוצג מעל
- [x] Tap "OK" → `DialogResult::Button(1)` מגיע ל-Future; calculator קורא `clear()`
- [x] Tap outside bounds → `DialogResult::Dismissed`
- [x] בזמן dialog פתוח — calculator לא מקבל pointer events (Down/Move/Up מסוננים ב-`Framework::dispatch_pointer`)
- [x] Background dim (alpha ~0.4 = 0x66 byte) על ה-app surface בזמן dialog פתוח

**Implementation summary**: SDK `dialog::DialogFuture` + `Context::show_dialog` (Stone 13.2). Framework `dialog::DialogManager` owns lifetime; allocates `WindowLayer::Overlay` surface per open dialog, drains resolutions per frame. Kernel `akiva_open_dialog` export + `hal_dialog_complete` import bridges JS↔framework. New `IO::SHOW_DIALOG = 0x0107` op_kind. 12 framework + 5 SDK unit tests. Live demo: tap C in Calculator triggers the dialog.

---

### Stone 13.3 — Transitions (~1 שבוע) ✅ סגור (May 2026)

**הבעיה.** כל activity push/pop הוא jump-cut מיידי. על mobile, transition מגדיר את תחושת ה-"OS" יותר מכל שינוי אחר ב-UI.

**מה נבנה.**

`framework/src/animation.rs` — `AnimationController`:
```rust
pub enum Transition {
    FadeIn { duration_ms: u32 },
    FadeOut { duration_ms: u32 },
    SlideFromRight { duration_ms: u32 },
    SlideToLeft { duration_ms: u32 },
}

impl AnimationController {
    pub fn tick(&mut self, elapsed_ms: f64) -> Option<f32>  // None = done, Some(t) = [0..1]
}
```

`Compositor::set_surface_props(id, alpha: f32, translate_x: f32)` — לפני `finish_frame`, מחיל `OP_PUSH_TRANSFORM(translate_x, 0, 1.0)` בתחילת surface's ops + rgba alpha multiplier על כל op. אין opcode חדש — transform stack קיים.

`Framework::push_activity` מפעיל `SlideFromRight(250ms)` על surface הנכנס. `pop_activity` מפעיל `SlideToLeft(200ms)` + `FadeOut(200ms)` במקביל. בכל frame: `tick(delta_ms)` → progress → compositor מחיל → כשהסתיים, outgoing surface נהרס.

**DoD:**
- [x] Activity push → slide-in animation 250ms — `SlideFromRight` driven by `AnimationController::tick`; rewrite via `apply_props` adds 2 ops + an alpha-byte rewrite per existing op (O(ops))
- [~] Activity pop → slide-out 200ms — **partial**: spec calls for `SlideToLeft`+`FadeOut` on the **outgoing** surface. Stone 13.3 ships `SlideFromLeft` on the **incoming** new top instead, because surface continuity for the destroyed activity (capture-and-replay of its last paint stream past lifecycle teardown) is a follow-up refactor outside Stone 13.3's scope. Documented in `framework/src/lib.rs::finish_top`.
- [x] 2 concurrent transitions on the same surface → composed multiplicatively (slide + fade test pinned)
- [x] Transition frame budget — `apply_props` is single-pass over the surface's ops vec; no per-frame allocations beyond the existing paint Vec it rewrites in place

**Implementation summary**: `framework::animation::AnimationController` keyed by a synthetic `SurfaceId` minted from `ActivityId`; `Framework::render_frame` ticks the controller, queries `props_for(top)`, and rewrites the foreground surface's ops with `PushTransform` + `PopTransform` for translate, alpha-byte multiplier on every colour-bearing op for fade. No new wire opcode. 13 unit tests covering all 5 transitions (FadeIn/FadeOut/SlideFromRight/SlideToLeft/SlideFromLeft) + composition + cancellation + completion drain.

**Follow-ups** (Stone 13.3.x):
- Surface continuity for outgoing pop (capture last paint stream + replay during transition window).
- Concurrent push-during-push back-stack — currently the second push restarts the slide on the new top; the prior (now-Paused) activity's transition state isn't preserved.

---

### Stone 13.4 — Cross-display / multi-tab (~1 שבוע) ✅ סגור (May 2026)

**הבעיה.** כיום `hal_post_to_display(displayId, bytes)` קיים ב-worker.js (Stone 11.x) — אבל tab שני מקבל אותו `displayId=0` כמו הראשון ולא task נפרד. אין מנגנון שמאפשר ל-tab שני לתבוע display חדש.

**מה נבנה.**

`tab.js` side: `{ type: "request_display" }` → session forwards to worker → kernel assigns `displayId` חדש (via `DisplayManager::alloc_display`) → kernel renders assigned task ל-displayId → `hal_post_to_display(displayId, bytes)` → worker.js שולח `display_frame(displayId)` ב-BroadcastChannel → tab שתבע אותו displayId מציג.

`leader.js` / Session: `Map<displayId, MessageChannel>` — כל tab שהצהיר `{ type: "claim_display", displayId }` מקבל frames רלוונטיים.

`DisplayManager` ב-kernel: `HashMap<DisplayId, SurfaceMetrics>` (קיים ב-[`kernel/src/display_manager.rs`](kernel/src/display_manager.rs), נרחיב ל-alloc/release).

**DoD:**
- [x] שני tabs: כל אחד מקבל task נפרד — `request_display` triggers `akiva_launch_to_display(sys.shell, ShellMain)` שמייצר instance חדש על task ייעודי. Default mirror נשמר כ-fallback אם auto-launch נכשל.
- [x] resize ב-tab B → אינו משפיע על metrics ב-tab A — `Event::Resize` נושא `display_id`; `DisplayManager::on_resize(id, ...)` מעדכן per-display; framework's `set_metrics` נקרא רק עבור PRIMARY. tab.js follower שולח resize עם הid הנכון; ResizeObserver מחיל גם על ה-`Canvas2DBackend` המקומי.
- [x] tab B נסגר → kernel מקבל `release_display` → tab.js fires `release_display` ב-pagehide; worker קורא `akiva_release_display` שמסיר את ה-display מה-HashMap. Foreground task ב-primary לא מושפע.

**מה נבנה (Stone 13.4 — closed)**:
- `kernel::display_manager::DisplayManager`: `HashMap<DisplayId, Display>`; `alloc_display`/`release_display`/`metrics_of`/`ids`/per-display `flush`.
- Kernel exports: `akiva_alloc_display(w, h, density_q16)`, `akiva_release_display(id)`, `akiva_assign_task_to_display(task, display)`, `akiva_launch_to_display(pkg, class, display)`.
- `ActivityManager::launch_in_new_task` — fresh-task variant של launch ש-bypasses launch_mode folding.
- `Framework`: `task_displays`, `assign_task_to_display`, `display_for_task`, `render_frame_for_display`, `foreground_activity_for_display`, `dispatch_pointer_on_display`, `tick_animations`, `launch_to_display`.
- `Event::Pointer` ו-`Event::Resize` הורחבו עם `display_id: u32` (legacy payloads ללא הid נקראים כ-`display_id = 0`).
- Per-display render path ב-`akiva_frame`: animation tick once → loop over `display_ids` → set_metrics per display → render_frame_for_display → flush.
- Follower paint: `runtime/web/paint_decoder.mjs` מפענח wire format זהה ל-`encode_op` של framework; tab.js follower מאתחל `Canvas2DBackend` ומחיל `display_frame` נכנסים מה-BroadcastChannel.
- JS protocol: `request_display { width, height, density }` (auto-launches `sys.shell/ShellMain`), `release_display { displayId }`, resize events נושאים displayId, pointer events נושאים displayId.
- Animation tick יוצא מ-`render_frame` ל-`tick_animations` נפרד שהקרנל קורא פעם אחת לפני loop ה-display, כדי ש-multi-display לא מאיץ transitions פי N.
- Dialog hit-testing משתמש ב-`foreground_activity_for_display` ב-`dispatch_pointer_on_display` — dialogs נצמדים ל-activity של ה-display שלהם אוטומטית.

**Tests**: 288 framework (+6 multi-display tests) + 202 SDK + 143 vitest.

---

**Exit criteria — Phase 13:**
- [x] Stone 13.0prep + 5 stones סגורים (13.0, 13.1, 13.2, 13.3, 13.4).
- [x] `sleep`/`timeout`/`interval` עובדים באפליקציה אמיתית; animation tick רץ על frame timestamp שמגיע מ-rAF (לא משתמש ב-timer primitive ישירות, אבל בקנה אחד עם ה-monotonic clock דרך `hal_now_ms`).
- [x] Stroke + image opcodes מצוירים נכון; Calculator מציג icon אמיתי.
- [x] System dialog מעל app פעיל; tap מחוץ לו סוגר אותו.
- [x] Activity push עם animation — slide-in 250ms, לא jump-cut.
- [x] שני tabs מציגים שתי tasks שונות — auto-launch על `request_display` נותן לכל tab instance עצמאי של Shell; per-display resize/pointer routing מבטיח isolation.

**הערכת זמן בפועל:** Phase 13 נסגר ב-~5 ימי עבודה אינטנסיבית, כשהוא רוכב על השכבות שכבר עומדות מ-Phase 11 (per-app workers, async dispatch) ו-Phase 12 (LMK, suspension). ההפרש מ-6-8 השבועות שצפינו: לא נדרש לבנות capture-and-replay לחציצה outgoing ב-13.3 (slide-out מבוצע כ-SlideFromLeft על ה-incoming).

---

## שלב 14 — Capabilities (Browser ↔ Apps)

**מטרה:** חשיפת platform capabilities שזמינות רק דרך הדפדפן לאפליקציות Akiva — רשת, אודיו, התראות, לוח, רטט, מיקום. כל capability עובר דרך HAL imports (kernel worker), עם proxying ל-tab.js עבור APIs שדורשות main thread.

**תלוי ב:** Phase 10 (Permission model — כל capability דורשת permission grant; ללא Phase 10, כל app מקבל גישה ל-WebSocket ויכול לדלוף נתונים). Phase 11 (isolated workers — capabilities נחשפות ל-app worker בלבד, לא ל-kernel ישירות).

**למה "14" ולא קודם.** Capabilities ללא permission model זה XSS-by-design: כל app מקבל גישה חופשית לרשת ויכול ל-exfiltrate נתוני app אחר. הסדר הנכון: isolation (Phase 11) → governance (Phase 12) → capabilities (Phase 14).

**ארכיטקטורת HAL לcapabilities:**

```
App (wasm) ─► hal_cap_* imports ─► kernel worker ─┬─[worker-native]──────────► Web API (WebSocket, Notifications)
                                                   └─[main-thread-proxied]──► postMessage("cap_req") ─► tab.js ─► Web API
                                                                               postMessage("cap_resp") ◄──────────────────
```

שתי קטגוריות:
- **Worker-native** (Stones 14.1–14.3): WebSocket, WebTransport, Notifications — ישירות ב-dedicated worker, ללא proxy
- **Main-thread-proxied** (Stones 14.4–14.6): Clipboard, Vibration, Geolocation — API תקפה רק ב-main thread; worker שולח `cap_req` ל-tab.js ומקבל `cap_resp`

---

### Stone 14.1 — Net capability foundation + HTTP/WebSocket transports ✅ סגור (May 2026)

**מה הדפדפן מאפשר.**
- `fetch` — HTTP/1.1 + HTTP/2 + HTTP/3 transparent. CORS חוסם cross-origin בלי opt-in מהשרת.
- `WebSocket` (RFC 6455) — bidirectional, message-framed, TCP/TLS. **Baseline 2015**. זמין ב-dedicated workers (`self.WebSocket`) — kernel worker פותח חיבורים ישירות, ללא proxy לmain thread. Binary frames (`binaryType = "arraybuffer"`), UTF-8 text. Browser מטפל ב-TLS + WS handshake בשקיפות.

**ההחלטה הארכיטקטונית.** במקום "ship WebSocket" כפי שקודם תכנן, מסגרנו את 14.1 מחדש כ-**Net Capability Foundation** — המשטח OS-shaped (manifest declaration + `INTERNET`-style permission + `connect()`-shaped SDK) קיים *ראשון*; HTTP ו-WebSocket הם הוצאות לראשונה של אותה תשתית. הסיבה: 14.2-14.6 הופכים לפלאגינים על אותה capability machinery במקום שכפול per-stone של פרק ה-grant/dispatcher.

**מה נבנה (Stone 14.1 — closed)**:

- **Manifest declaration** — `permissions.required = ["akiva.permission.NETWORK"]` ב-`manifest.toml` של אפליקציה. `framework::permissions::declare_mvp_permissions` כבר הצהיר על `NETWORK` ב-`ProtectionLevel::Normal` מ-Stone 10.3, אז install-time auto-grant ירש את הסמנטיקה. שדרוג ל-`Dangerous` (runtime prompt דרך 13.2's `DialogManager`) הוא 14.1.x.
- **Capability gate** — בדיקת ה-grant פעמיים: ב-`app_worker_shell.js::dispatchFire`/`dispatchIoStart` (קצר את הקריאה לפני round-trip ל-AppRegistry) וב-`app_registry.js::_handleHostOp`/`_handleSideEffect` (אכיפה אמיתית — האפליקציה לא יכולה לרמות את ה-worker שלה כי האירוע יורד ב-AppRegistry).
- **Wire ops** — 5 op_kinds חדשים, כולם בטווחי fire/io הקיימים:
  - `IO::HTTP_FETCH = 0x0108` — request/response שלם, async, גומר על fetch resolution
  - `IO::WS_OPEN = 0x0109` — handshake → handle: u32
  - `IO::WS_RECV = 0x010A` — pull הבא event (text/binary/closed)
  - `FIRE::WS_SEND = 0x0006` — fire-and-forget משלוח frame
  - `FIRE::WS_CLOSE = 0x0007` — close idempotent
- **SDK API** — `sdk-rust/src/net.rs`:
  - `net::get(url)` / `net::post(url, body)` / `net::Request` builder עם `.header()`/`.body()`/`.send()`
  - `net::WebSocket::connect(url) -> WsConnectFuture`; methods `send_text/send_binary/recv/close`; `Drop` שולח close 1000 כדי לא לדלוף connections
  - `NetError` enum: `Denied | BadUrl | Network(String) | Closed | Busy`
  - 14 unit tests (encode/decode round-trips, pre-resolved error paths, method codes)
- **AppRegistry-side** — `_wsHandles: Map<u32, {ws, packageId, recvQueue, pendingRecv, closed}>`. אירועי `onmessage` נכנסים ל-queue (cap=64; overflow → force-close 1009). `recv()` של אפליקציה רואה את ה-queue או נרשם כ-`pendingRecv`. ניקוי ב-`evictPackage` + watchdog kill דרך `_disposeWsForPackage`.
- **Demo app** — `apps/network/` חדש: שני כפתורים (HTTPS GET → `httpbin.org/get`, WebSocket echo → `wss://echo.websocket.events`) + status line שמראה round-trip success.
- **ABI bump** — `RUNTIME_ABI_VERSION` 0.8.0 → 0.9.0 (additive: ops חדשים תחת fire/io ranges קיימים, אפליקציות עם `min_abi_version = "0.1.0"` עדיין נטענות).

**מה לא במסגרת (deferred ל-14.1.x)**:
- **Runtime prompt UI** — `NETWORK` נשאר `Normal` (auto-grant). שדרוג ל-`Dangerous` עם dialog "Allow X to access network?" דורש את ה-`DialogManager` הקיים מ-13.2 + persistence של החלטת המשתמש. נטל קטן יחסית (~יום) אבל UI work שהמשתמש העדיף לדחות.
- **Host allowlist בmanifest** — `permissions.network.hosts = [...]` לחסום ב-grant time. נמצא בשטח אבל מחוץ ל-foundation; כל apps כעת רואים the open internet (modulo CORS).
- **Streaming HTTP** — `fetch` כרגע resolves על body שלם. SDK עוד אין `Response::body_stream()`. הוצאה לראשונה אם app אמיתי דורש (P1).
- **WebSocket subprotocols** — wire byte קיים אבל ה-SDK לא חשפו, כי אין use case היום.

**Exit criteria — Stone 14.1 (sealed):**
- [x] Manifest `permissions.required = ["akiva.permission.NETWORK"]` נדרש; אפליקציה ללא ה-grant מקבלת `Err(NetError::Denied)` synchronous
- [x] HTTPS GET ל-server עם CORS עובד (httpbin.org demo)
- [x] WebSocket connect → send → echo round-trip עובד (echo.websocket.events demo)
- [x] WS handle בלעדי לאפליקציה — קריאה מ-worker אחר נדחית (provenance check)
- [x] Worker termination סוגרת WS connections (no orphan handles)
- [x] 288 framework + 216 SDK + 81 wire + 143 vitest tests עוברים
- [x] ABI bump 0.8.0 → 0.9.0 documented ב-`docs/abi.md`

---

### Stone 14.1.x — Network capability follow-ups (planned, not blocking)
- Runtime permission prompt: NETWORK → `Dangerous`, dialog UX
- Host-restricted grants (manifest allowlist of allowed hosts)
- Streaming HTTP responses (`Response::body_stream()`)
- WebSocket subprotocols exposed in SDK

---

### Stone 14.2 — WebTransport ✅ סגור (May 2026)

**מה הדפדפן מאפשר.** WebTransport over HTTP/3 (QUIC). Chrome 97+, Firefox 114+, Safari 18.2+ — broadly available. שתי ערוצים לכל session:
- **datagrams** — unreliable, unordered, UDP-style. הכי קרוב שמסך אפשר ל-`DatagramSocket()` של אנדרואיד. אידיאלי ל-real-time games / voice signaling / telemetry שלא יכולים לסבול head-of-line blocking.
- **bidirectional streams** — reliable, ordered (TCP-style on top of QUIC streams). אחד או יותר לכל session.

**מה נבנה (Stone 14.2 — closed)**:
- 8 wire ops חדשים תחת ABI 0.10.0 (additive — אפליקציות 0.x עדיין נטענות):
  - IO: `WT_OPEN`, `WT_RECV_DATAGRAM`, `WT_OPEN_BIDI`, `WT_RECV_STREAM`
  - Fire: `WT_SEND_DATAGRAM`, `WT_SEND_STREAM`, `WT_CLOSE_STREAM`, `WT_CLOSE`
- SDK API (`sdk-rust/src/net.rs`):
  - `net::WebTransport::connect(url) -> WtConnectFuture`
  - `wt.send_datagram(&[u8])` / `wt.recv_datagram().await -> WtDatagram { Bytes(Vec<u8>) | Closed { code, reason } }`
  - `wt.open_bidi_stream().await -> BidiStream`
  - `BidiStream::send/recv/close` — `recv` מחזירה `StreamEvent { Bytes(Vec<u8>) | Fin | Error(String) }`
  - `Drop` על שני ה-types שולח close כדי לא לדלוף host-side handles
- AppRegistry-side: 2 maps נוספים (`_wtHandles`, `_wtStreams`) + 2 pump loops (datagram readable stream + per-stream readable). queue cap זהה ל-WS (64 events); overflow על datagrams = drop oldest (UDP-style), על streams = החלפת tail ב-Error event ("stream queue overflow"). cleanup ב-`_disposeWsForPackage` (משם השם הכללי).
- 8 SDK unit tests חדשים (decode datagram bytes/closed/truncated/unknown, decode stream bytes/fin/error, connect-empty-url).
- Demo: כפתור "WebTransport echo" באפליקציית `Network`. ה-endpoint `https://webtransport.day/echo` מנוסה כברירת מחדל; כשלון מוצג בסטטוס בלי lockup. אפשר להגדיר server אישי דרך `localStorage.setItem('akiva.wt_endpoint', '...')`.

**מגבלה אמיתית — public infra.** בניגוד ל-WebSocket, אין "echo.websocket.events" של WebTransport: השרתים הציבוריים נדירים ולא יציבים. ה-button חוזר ב-error message ברור כשהשרת לא מגיב. ה-SDK עצמו מכוסה ע"י unit tests; צרכן רציני בונה את השרת שלו.

**Exit criteria — Stone 14.2 (sealed):**
- [x] Manifest `permissions.required = ["akiva.permission.NETWORK"]` ה-grant היחיד הנדרש (משותף עם 14.1)
- [x] Datagram round-trip עובד נגד server שמדבר HTTP/3 + WebTransport (manual test against user-supplied endpoint)
- [x] Bidirectional stream round-trip עובד (manual)
- [x] Stream / session handle בלעדי לאפליקציה (provenance check)
- [x] Worker termination סוגר WT sessions + streams
- [x] 296 framework + 224 SDK + 81 wire + 143 vitest tests עוברים
- [x] ABI bump 0.9.0 → 0.10.0 documented

---

### ~~Stone 14.2 — WebTransport (~3 ימים)~~ — superseded by closed entry above

**מה הדפדפן מאפשר.**
`WebTransport` over HTTP/3 (QUIC). **Baseline 2023** (Chrome 97+, Firefox 114+, Safari 18.2+). זמין ב-dedicated workers (Chrome 104+). מספק:
- `datagrams.readable/writable` — unreliable, unordered, UDP-style; אידיאלי ל-game state ו-real-time telemetry (קטן יותר מWS לmessages קטנות, ללא head-of-line blocking)
- `createBidirectionalStream()` → QUIC stream (reliable, ordered, backpressure)
- דורש HTTPS (HTTP/3 server בלבד)

**מה אנחנו עושים.**

Worker-native (כמו WS). HAL imports:
```
hal_wt_open(url_ptr, url_len, handle_out) → i32
hal_wt_datagram_send(handle, data_ptr, data_len)
hal_wt_datagram_recv(handle, out_ptr, out_cap, out_len) → i32   // 0=empty, -1=closed
hal_wt_stream_open(handle, stream_out) → i32
hal_wt_stream_send(stream, data_ptr, data_len) → i32
hal_wt_stream_recv(stream, out_ptr, out_cap, out_len) → i32
hal_wt_close(handle)
```

`worker.js` — `Map<u32, WebTransport>` + datagrams ring buffer. `wt.datagrams.readable.getReader()` pumped async ל-ring.

SDK: `akiva_sdk::net::WebTransport` עם `send_datagram(&[u8])`, `recv_datagram() -> Future<Vec<u8>>`, `open_stream() -> Future<WtStream>`.

**DoD:**
- [ ] Datagram roundtrip ב-test server
- [ ] `typeof WebTransport === "undefined"` (old browser) → `hal_wt_open` returns `-1` gracefully, לא crash
- [ ] Stream open + send + recv roundtrip

---

### Stone 14.3 — Notifications (~3 ימים)

**מה הדפדפן מאפשר.**
`Notification` — **Baseline 2022**. **זמין ב-dedicated workers** (`self.Notification`). `new Notification(title, { body, icon, tag, badge })` → OS-level system notification. `tag` deduplicates — אותו tag מעדכן notification קיים במקום ליצור חדש (חשוב נגד spam). `Notification.requestPermission()` → Promise<"granted"|"denied"|"default">; browser stores בין sessions.

**מה אנחנו עושים.**

Worker-native — אין proxy. `runtime/web/worker.js`:
```js
let notifyPermission = "default";
const hal_notify_post = async (titlePtr, titleLen, bodyPtr, bodyLen) => {
  if (notifyPermission !== "granted") {
    notifyPermission = await Notification.requestPermission();
  }
  if (notifyPermission !== "granted") return 1;  // denied → app handles
  new Notification(readUtf8(titlePtr, titleLen), {
    body: readUtf8(bodyPtr, bodyLen),
    tag:  "akiva-notify",
  });
  return 0;
};
```

Permission integration: app manifest `permissions = ["NOTIFICATION"]` (Phase 10). Kernel checks permission table לפני שמגיע ל-HAL; אם לא granted → returns 1 מבלי לשאול.

SDK: `akiva_sdk::notify::post(title: &str, body: &str) -> i32`.

**DoD:**
- [ ] OS notification מוצגת מ-Calculator app לאחר חישוב
- [ ] Same `tag` → update existing notification, לא spam
- [ ] `Notification` לא נתמך (HTTP context) → returns -1, לא crash
- [ ] Permission denied → returns 1; app log ברור

---

### Stone 14.4 — Clipboard (~3 ימים)

**מה הדפדפן מאפשר.**
`navigator.clipboard.writeText(text)` / `.readText()` — **Baseline 2018**, secure context בלבד. **Main thread only** — `navigator` לא זמין ב-dedicated workers. `readText()` דורש `clipboard-read` permission (Permissions API). `writeText()` עובד ב-response ל-user gesture.

**מה אנחנו עושים.**

`cap_req` / `cap_resp` protocol חדש ב-tab.js + worker.js:

`worker.js`:
```js
let capReqId = 0;
const capPending = new Map();  // reqId → resolve fn

// בhandler של "message":
if (e.data?.type === "cap_resp") {
  const r = capPending.get(e.data.reqId);
  if (r) { r(e.data); capPending.delete(e.data.reqId); }
}

function capRequest(payload) {
  return new Promise(resolve => {
    const id = ++capReqId;
    capPending.set(id, resolve);
    self.postMessage({ type: "cap_req", reqId: id, ...payload });
  });
}

hal_clipboard_write: async (ptr, len) => {
  const r = await capRequest({ capability: "clipboard_write", text: readUtf8(ptr, len) });
  return r.error ? -1 : 0;
},
hal_clipboard_read: async (outPtr, outCap, outLenPtr) => { ... },
```

`tab.js`:
```js
case "cap_req": handleCapReq(msg); break;

async function handleCapReq(msg) {
  const resp = { type: "cap_resp", reqId: msg.reqId };
  try {
    if (msg.capability === "clipboard_write")
      await navigator.clipboard.writeText(msg.text);
    else if (msg.capability === "clipboard_read")
      resp.text = await navigator.clipboard.readText();
  } catch (e) { resp.error = e.message; }
  session.send(resp);
}
```

SDK: `akiva_sdk::clipboard::write(text: &str) -> Future<()>`, `read() -> Future<String>`.

**DoD:**
- [ ] Write "hello" → paste ב-external app works
- [ ] `read()` Future resolves עם clipboard text
- [ ] Permission denied → `Err(ClipboardError::Denied)`, לא crash
- [ ] Non-secure context (HTTP) → graceful `Err`, לא panic

---

### Stone 14.5 — Vibration (~1 יום)

**מה הדפדפן מאפשר.**
`navigator.vibrate(pattern)` — `pattern` הוא `number` (ms) או `number[]` (vibrate/pause/vibrate). Chrome 32+/Firefox 16+ ב-Android. **Safari: לא תומך — מחזיר `false` בשקט.** Main thread only. שימוש נפוץ: haptic feedback על tap, redo/undo, alert patterns.

**מה אנחנו עושים.**

Fire-and-forget — לא דורש Future. `hal_vibrate(pattern_ptr, pattern_len)` → worker posts `{ type: "cap_req", capability: "vibrate", pattern: [...u16] }`. tab.js:
```js
case "vibrate":
  if (navigator.vibrate) navigator.vibrate(msg.pattern);
  // no cap_resp needed — fire and forget
  break;
```

SDK: `akiva_sdk::device::vibrate(pattern: &[u16])`.

**DoD:**
- [ ] Android Chrome — vibrate pattern רוטט (200ms)
- [ ] Safari / desktop → silent no-op (returns immediately, no crash, no warning log)

---

### Stone 14.6 — Geolocation (~3 ימים)

**מה הדפדפן מאפשר.**
`navigator.geolocation.getCurrentPosition(ok, err, opts)` / `watchPosition()`. Requires secure context. **Main thread only**. User permission dialog. `opts.enableHighAccuracy: true` → GPS (battery expensive, 1-10s cold start); `false` → network/WiFi triangulation (fast, ~100m). `watchPosition` fires callback כל פעם שמיקום משתנה (מינימום ~1s).

**מה אנחנו עושים.**

Proxied via `cap_req` / `cap_resp` (מ-Stone 14.4). `worker.js`:
```
hal_geo_current() → future_id: u32  // result arrives via cap_resp
hal_geo_watch_start() → watch_id: u32
hal_geo_watch_stop(watch_id: u32)
```

`tab.js`:
```js
case "geo_current":
  navigator.geolocation.getCurrentPosition(
    pos => session.send({ type: "cap_resp", reqId: msg.reqId,
      lat: pos.coords.latitude, lng: pos.coords.longitude,
      accuracy_m: pos.coords.accuracy, ts_ms: pos.timestamp }),
    err => session.send({ type: "cap_resp", reqId: msg.reqId, error: err.code }),
    { enableHighAccuracy: false, timeout: 10000 }
  );
  break;
case "geo_watch_start": {
  const wid = navigator.geolocation.watchPosition(pos => { ... }, err => { ... });
  session.send({ type: "cap_resp", reqId: msg.reqId, watchId: wid });
  break;
}
```

SDK: `akiva_sdk::location::get_current() -> Future<GeoPosition>`, `watch() -> GeoWatcher`.
```rust
pub struct GeoPosition { pub lat: f64, pub lng: f64, pub accuracy_m: f32, pub ts_ms: f64 }
```

**DoD:**
- [ ] `get_current()` Future resolves עם position (test על mobile Chrome)
- [ ] Permission denied → `Err(GeoError::Denied)`, app continues
- [ ] `watch()` fires updates על movement (5+ updates ב-30s walk test)
- [ ] Non-HTTPS → `Err(GeoError::InsecureContext)` מ-tab.js

---

**Exit criteria — Phase 14:**
- [ ] 6 stones סגורים, כל אחד עם end-to-end test app.
- [ ] Calculator: notification לאחר כל חישוב → OS notification מוצגת.
- [ ] Sample "ping" app: WebSocket send/recv roundtrip ל-echo server.
- [ ] Clipboard write/read roundtrip עובד.
- [ ] כל capability מטפלת gracefully ב-permission denied + browser ללא תמיכה.
- [ ] `docs/abi.md` מעודכן עם כל HAL imports חדשים.
- [ ] ABI MINOR bump (additive imports — לא breaking).

**הערכת זמן:** 5-7 שבועות.

---

## סיכום v0.3

**סה"כ זמן:** ~13-21 שבועות (~3-5 חודשים) אחרי v0.2.

בסיום v0.3, Akiva עוברת את שלושת ה-"hard systems tests" של OS אמיתית:
- **isolation:** app אחד לא יכול להפיל את המערכת
- **governance:** המערכת יודעת למדוד ולאכוף quota על apps
- **window control:** המערכת מנהלת surfaces מרובים, לא רק fullscreen activity יחידה

הסוקר החיצוני ([codex-notes/external-review.md](codex-notes/external-review.md), 2026-04-22) זיהה שלושת אלה כ-"three tests before real OS". v0.3 הוא הפעולה הממוקדת לסגור את השלישייה הזו.

## סיכום v0.4

**סה"כ זמן:** ~5-7 שבועות אחרי v0.3.

בסיום v0.4, Akiva עוברת מ-OS מבודדת ל-**connected OS**:
- **networking:** apps מדברות עם servers בזמן אמת (WebSocket, WebTransport)
- **audio:** חוויית מולטימדיה native-grade (Web Audio API)
- **platform integration:** התראות, clipboard, מיקום, רטט — כל מה שOS mobile מציע
- **הוכחה:** Akiva apps עושות את מה שApps בכל OS אחר עושות

---

## סיכום זמנים

### MVP (v0.1) — Phases 0–7

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

### post-MVP — Phases 8–14

| שלב | תיאור | זמן |
|---|---|---|
| 8  | Package distribution (.akv + install) | 2-3 שבועות |
| 9  | Activity lifecycle מלא (back stack, for-result) | 3-4 שבועות |
| 10 | Permission model (capability-via-imports) | 2-3 שבועות |
| 11 | Process isolation (dedicated workers + ABI) | 13-19 שבועות |
| 12 | Resource Governance (memory + CPU quota) | 3-5 שבועות |
| 13 | WindowManager completion (overlays, transitions) + foundation prep (timers/stroke/drawable) | 6-8 שבועות |
| 14 | Capabilities (WebSocket, Audio, Notify, Clipboard…) | 5-7 שבועות |

**סה"כ post-MVP v0.2–v0.4: ~8-13 חודשים** נוספים.

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
- הוצאה ל-`apps/shell/` כ-wasm package — **קודם לעבור ל-Stone 11.0 (Phase 11 prereq)**; ראה v0.3 לעיל. Shell כ-wasm app אמיתי חייב לקדום את Phase 11 כדי שהוא לא יהפוך ל-exception פריבילגי בתוך ה-isolation model.

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

**Foundation gaps deferred (audit מאי 2026, post-Stone 11.6.9):**

לא בלוקר ל-Phase 12; לא בלוקר ל-13 (timers + UI primitives טופלו ב-Stone 13.0prep). מתווספים בהמשך לפי ביקוש אמיתי של app:

- **Text input — character data**: היום key events מעבירים `keyCode` (deprecated) בלבד דרך [tab.js:182-185](runtime/web/tab.js#L182). אפליקציה לא יכולה לקבל typed text — אפילו Latin בלבד. כשapp ראשון מצריך typing, הוסף `event.key` + `compositionevent` ל-`tab.js` → `worker.js` event encoder → `kernel/events.rs` → SDK input enum. ~1 יום. גם Hebrew/IME/RTL מחכים פה (mark-out: user preference dictates MVP-skip). חוסם apps של notes/email/chat — לא חוסם calc/games.
- **`storage::list_keys()` / scan**: היום אין דרך ל-app להציג רשימת keys שלה ב-storage. עוקפים ע"י maintain-an-index ב-key נפרד. SDK addition + `OPFS getDirectoryHandle().keys()` (Chrome 121+ תומך). ~חצי יום.
- **App icon + display name ב-manifest**: `app.icon = "@drawable/launcher"` + `app.label = "@string/app_name"`. תלוי ב-`@drawable/` (Stone 13.0prep #C). ~חצי יום מעל drawable. במקביל לחיווט ב-Shell.
- **Crash log persistence**: trapped app מאבד את כל ה-logs שלו ב-`worker.terminate()`. אם רוצים postmortem ("מה ה-`println!` האחרון לפני ה-trap?") — ring buffer ב-kernel שcollect מ-`message: { type:"log", ... }` ב-flight, persisted ל-`sys/crashes/{id}/{ts}.log`. ~1 יום. helpful ל-Phase 12.4 debugging אבל לא חוסם.
- **`select!` macro**: `Channel`/`Mutex`/`Signal` קיימים מ-Stone 11.6.8 #3. `select!` (לחכות לראשון מבין כמה futures) הוא thin wrapper מעליהם. apps שצריכות יכולות לכתוב match-loop או להשתמש ב-`futures::future::select`. ~3h כשתחושת חסימה.
- **Multi-key storage atomicity**: היום write יחיד atomic; אין transaction. App ש-crash באמצע multi-key save נשארת half-written. design call — OPFS לא חושף transactions, צריך WAL ב-kernel. ~3-5 ימים. **המלצה: לעולם לא** — עוקפים בblob יחיד דרך `on_save_state` (כבר ב-MVP) או דרך index file pattern.
- **CI / GitHub Actions**: 414+ Rust + 54+ vitest רצים מקומית. אין pre-merge gate. מתי שcodebase פותח ל-contributors חיצוניים → הוסף `.github/workflows/test.yml`. עד אז: לא חסר.

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
