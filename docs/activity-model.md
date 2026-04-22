# Activity Model — מודל תהליכים ו-Activities

> **סטטוס מימוש (אפריל 2026):** lifecycle state machine מלא (6 המעברים) + launch modes `Standard`/`SingleTop` פעילים ב-[framework/src/activity_manager.rs](../framework/src/activity_manager.rs), עם test coverage end-to-end (Stone 9.1). `SingleTask`/`SingleInstance`, back navigation, `start_activity_for_result`, ו-`on_new_intent` — stones 9.2–9.6. ראה [v0.2-design.md §4 (עמוד 2)](v0.2-design.md) ל-gap map מלא ו-[ROADMAP.md שלב 9](../ROADMAP.md) לסטונים.

## 1. מושגים יסודיים

**App (אפליקציה):** יחידת הפצה. חבילה עם `manifest.toml`, קובץ `wasm`, ו-`res/`. מזוהה ב-`package_id` ייחודי (למשל `com.akiva.calculator`).

**Process:** instance רץ של wasm של אפליקציה. memory מבודד. ל-App יכול להיות יותר מ-Process אחד בו-זמנית (לפי launch mode).

**Activity:** יחידת UI — מסך עם lifecycle משלו. Activity שייכת ל-App ורצה בתוך Process שלה. אפליקציה יכולה להגדיר כמה Activities (למשל `MainActivity`, `SettingsActivity`).

**Task:** רצף של Activities שהמשתמש עבר בהן, מונהג כ-stack. Task יכול לכלול Activities מאפליקציות שונות.

## 2. Activity Lifecycle

```
        ┌─────────────┐
        │ Not Existing│
        └──────┬──────┘
               │ launch(intent)
               ▼
        ┌─────────────┐
        │   Created   │ ← onCreate(savedState?)
        └──────┬──────┘
               │ onStart()
               ▼
        ┌─────────────┐
        │   Started   │ (visible, not in focus)
        └──────┬──────┘
               │ onResume()
               ▼
        ┌─────────────┐
        │   Resumed   │ (visible, in focus — "running")
        └──────┬──────┘
               │ onPause()  ← אחרת activity קיבלה focus
               ▼
        ┌─────────────┐
        │   Paused    │
        ├─────────────┤
        │ ← onResume() │
        └──────┬──────┘
               │ onStop()  ← אם כבר לא visible
               ▼
        ┌─────────────┐
        │   Stopped   │
        ├─────────────┤
        │ ← onRestart()→ onStart()  │
        └──────┬──────┘
               │ onDestroy()
               ▼
        ┌─────────────┐
        │  Destroyed  │
        └─────────────┘
```

### ערבויות

- `onCreate` נקרא פעם אחת בלבד.
- `onDestroy` נקרא פעם אחת בלבד, **מובטח** להיקרא גם אם ה-activity נהרגת (אלא אם ה-process קרס).
- `onPause` **תמיד** נקרא לפני `onStop`.
- `onStop` **תמיד** נקרא לפני `onDestroy`.
- `onResume` נקרא **פעם אחת לפחות** לפני כל `onPause`.

### זמני ריצה מקסימליים

- `onCreate`: עד 100ms (משתמש מחכה)
- `onPause`: עד 50ms (המעבר חייב להיות מהיר)
- `onStop`, `onDestroy`: עד 200ms (לא נראים למשתמש)

אם חורגים — ה-runtime מתריע ב-log ובעתיד יכול להרוג את ה-process.

### Implementation (Phase 2b + Phase 9.1)

| שכבה | מצב | איפה |
|---|---|---|
| `LifecycleState` enum — Initial/Created/Started/Resumed/Paused/Stopped/Destroyed | ✅ | [`framework/src/activity_manager.rs`](../framework/src/activity_manager.rs) |
| Legal-transition matrix (כולל `Stopped → Started` עבור onRestart) | ✅ | `legal_transition` + 2 unit tests |
| Drivers: `resume`/`pause`/`destroy`/`advance` עם בדיקות per-edge | ✅ Phase 9.1 | 13 unit tests על mock `RecordingActivity` — טופס כל המעבר, idempotency, וקצוות החוזרים (Stopped→destroy, Paused→destroy) |
| Framework bring-up/tear-down (`drive_to_resumed`, `launch_app`, `finish_top`) | ✅ | [`framework/src/lib.rs`](../framework/src/lib.rs) |
| Kernel ABI: `hal_app_lifecycle(handle, transition)` | ✅ | [`kernel/src/host.rs`](../kernel/src/host.rs), `KernelWasmHost` |
| Worker dispatch: `LIFECYCLE_EXPORTS[0..5]` → `akiva_app_{create,start,resume,pause,stop,destroy}` | ✅ | [`runtime/web/app_registry.js`](../runtime/web/app_registry.js) |
| SDK exports (all 6 `akiva_app_*` extern "C" functions) | ✅ | [`sdk-rust/src/lib.rs`](../sdk-rust/src/lib.rs) `akiva_activity!` macro |
| SDK trait — `on_create/start/resume/pause/stop/destroy` with no-op defaults | ✅ | [`sdk-rust/src/activity.rs`](../sdk-rust/src/activity.rs) |
| `save_state_bytes` / `restore_state_bytes` wired through Create→restore→Start | ✅ | framework `launch_app`, SDK runtime |
| `onRestart` as a distinct callback | ❌ by design | Stopped→Started re-fires `on_start` — apps that need "first start vs restart" distinction gate on internal flags |
| `finish()` inside `on_create` short-circuit (Created→Destroyed) | ✅ Stone 9.3 | `legal_transition` gains the edge; `advance_through_create` returns whether finish was raised; `launch_app` calls `destroy_shortcut` (fires only `on_destroy`) + pops the stack entry instead of continuing to Start/Resume. Android parity — see [research-android-lifecycle.md §1](research-android-lifecycle.md) |
| Wasm apps can call `ctx.finish()` | ✅ Stone 9.3 | SDK `Context::finish()` → `akiva_host_finish(handle)` import → worker sets per-handle flag → kernel drains via `hal_app_drain_finish` at the end of every ctx-bearing callback → framework flips `ActivityContext.finish_requested`. Matches in-kernel activities (Shell) that set the flag directly |
| `on_new_intent` on SingleTop reuse | ✅ Stone 9.4 | `Framework::launch_app` detects `new_id == current top` → `deliver_new_intent` runs the callback, no bring-up chain, no stack churn. Framework-level test: `singletop_reuse_fires_on_new_intent_instead_of_bring_up` |
| Wasm apps can call `ctx.start_activity(&intent)` | ✅ Stone 9.4 | SDK `Context::start_activity` → `akiva_host_start_activity(handle, ptr, len)` import with [`Intent::encode`](../sdk-rust/src/intent.rs) → worker stashes on `_pendingIntent` → kernel drains via `hal_app_drain_intent` after each ctx-bearing callback → `ActivityContext.launch_requested` → `IntentRouter::resolve` → `launch_app`. Symmetric wire codec pinned in [docs/abi.md §2.5.1](abi.md) |
| SDK `Activity::on_new_intent(ctx, &intent)` + `akiva_app_new_intent` export | ✅ Stone 9.4 | Default no-op; dispatched from `runtime::on_new_intent(handle, ptr, len)`. Framework `WasmActivity::on_new_intent` re-encodes the decoded `Intent` and forwards via `WasmHost::new_intent` → `hal_app_new_intent` |
| `on_new_intent` on SingleTask / SingleInstance reuse | ⏳ Stone 9.2 | The reuse-path plumbing is in place; what's missing is `ActivityManager::launch` actually implementing those modes (currently returns `Unsupported`). When 9.2 lands, the existing `deliver_new_intent` picks up the new-intent delivery automatically |

Bug caught by the Phase 9.1 test sweep: `destroy()` used to `break` on the first illegal edge, so destroying a Paused or Stopped activity silently skipped `on_stop`/`on_destroy`. Fixed to ignore-and-continue, matching `resume`'s shape.

Bug caught by the Task 1 state-machine audit ([v0.2-review.md](v0.2-review.md)): `destroy()` called from `Initial` silently dropped the instance and rewrote the stack even though no lifecycle callback fired. Guard added so cleanup only runs once the record actually reached `Destroyed`.

External research backing the above table lives in [research-android-lifecycle.md](research-android-lifecycle.md) — five AOSP edges audited, decisions documented.

## 3. Saved State

### onSaveState / onRestoreState

Activity יכולה לשמור state קטן שיישמר גם אם ה-process נהרג:

```rust
impl App for MyApp {
    fn on_save_state(&self, state: &mut SavedState) {
        state.put_i32("counter", self.counter);
        state.put_str("input", &self.input);
    }

    fn on_restore_state(&mut self, state: &SavedState) {
        self.counter = state.get_i32("counter").unwrap_or(0);
        self.input = state.get_str("input").unwrap_or_default();
    }
}
```

**מתי נקרא:**
- `onSaveState` — לפני `onStop` (לא לפני `onPause`, כי pause עדיין באותו process)
- `onRestoreState` — אחרי `onCreate`, אם יש saved state שמור

**מגבלות:**
- Maximum 64KB לכל activity
- Serializable types בלבד
- **לא** למטרות persistence ארוכת-טווח — זה לצורך recovery בלבד

### Persistent state

ל-data קבוע — להשתמש ב-StorageManager API.

### דפוס State — שלוש שכבות

`on_save_state` עם limit של 64KB הוא קטן בכוונה, כמו `onSaveInstanceState` באנדרואיד (הגבלת Bundle היא 1MB שם, אבל בפועל <50KB). בקוד אמיתי יש שלוש שכבות של state, כל אחת עם מטרה נפרדת:

| שכבה | מטרה | מתי נשמר | איפה | גודל |
|---|---|---|---|---|
| **UI state** | מה שמוצג כרגע על המסך (scroll position, selected tab, form input לא-שמור) | `on_save_state` | Kernel in-memory + OPFS ברגע הריגה | ≤ 64 KB |
| **ViewModel state** | state מחושב של ה-activity שלא רלוונטי ל-persistence (cached queries, list of items currently displayed) | לא נשמר חוצה-process | in-memory ב-app wasm instance | bound by memory quota |
| **Persistent data** | data של האפליקציה (user files, settings, DB) | explicit ב-storage API | StorageManager (OPFS) | bound by app quota |

**הכלל:** אם זה לא יכול להיכנס ב-64KB serialized — זה לא שייך ל-`on_save_state`. כל דבר גדול מזה שאפשר לאבד → ViewModel (re-compute). כל דבר שאסור לאבד → storage explicit.

**דוגמה — אפליקציית רשימת tasks:**

```rust
struct TasksApp {
    // Persistent: תמיד נשמר ל-storage, source of truth
    // Kept in storage via explicit writes
    storage_key: &'static str,

    // ViewModel: נגזר מ-storage, re-hydrated ב-on_create
    tasks: Vec<Task>,
    filter: Filter,

    // UI state: נשמר ב-on_save_state
    scroll_position: f32,
    edit_in_progress: Option<String>,
}

impl App for TasksApp {
    fn on_create(&mut self, ctx: &ActivityContext) {
        // ViewModel hydration from persistent
        self.tasks = storage::get_json("tasks").unwrap_or_default();

        // UI state restoration from saved state if present
        if let Some(saved) = ctx.saved_state() {
            self.scroll_position = saved.get_f32("scroll").unwrap_or(0.0);
            self.edit_in_progress = saved.get_str("edit").map(String::from);
        }
    }

    fn on_save_state(&self, state: &mut SavedState) {
        // Only UI state — tasks come from storage on re-create.
        state.put_f32("scroll", self.scroll_position);
        if let Some(ref edit) = self.edit_in_progress {
            state.put_str("edit", edit);
        }
    }
}
```

**למה לא לשמור `tasks` ב-saved_state:** אם יש 5000 tasks, זה עובר את ה-64KB; ועוד, ה-source of truth שלהם הוא storage — הם נטענים משם תמיד. saved_state זה רק ל-state שזניח לאחסן כל פעם.

## 4. Launch Modes

זהה ל-Android, עם 4 מצבים:

### standard (ברירת מחדל)

כל `startActivity` יוצר instance חדש. אפשרי לראות 5 instances של אותה activity ב-stack.

### singleTop

אם ה-activity כבר ב-top של ה-stack, לא יוצר חדש — מעביר intent ל-existing דרך `onNewIntent`. אחרת — יוצר חדש.

שימושי ל-Search activity: אם המשתמש מחפש מתוך search screen, עדיף להציג את אותה search screen עם ה-intent החדש.

### singleTask

יש **instance אחד בלבד** ב-task נתון. אם כבר קיים, clear של כל ה-activities שמעליו ו-resume שלו.

שימושי ל-main activities של אפליקציות — Browser, Calculator.

### singleInstance

יש **instance אחד בלבד במערכת כולה**, ב-task נפרד לחלוטין. אף activity אחרת לא יכולה להיכנס ל-task שלו.

שימושי ל-dialer, phone call screens.

### הצהרה ב-manifest

```toml
[[activities]]
id = "main"
launch_mode = "singleTask"
```

## 5. Task Stack

### מבנה

```rust
pub struct Task {
    pub id: TaskId,
    pub affinity: String,        // default = package_id
    pub activities: Vec<ActivityInstance>,  // bottom → top
}
```

### Task Affinity

באותו affinity — activities יישבו ב-task אחד. שונה affinity — task נפרד.

ברירת מחדל: affinity = package_id. שתי apps שונות → tasks נפרדים. כל ה-activities של אותה app → אותו task.

### סדר back

לחיצה על Back:
1. אם יש activity אחת מעל ה-bottom → pop. הקודמת נהיית resumed.
2. אם זו ה-activity האחרונה ב-task → task מסתיים, משתמש חוזר לתצוגה הקודמת.
3. אם זה ה-task היחיד → משתמש חוזר ל-Shell.

### הדגמה

```
Stack (top → bottom):
  Calculator.SettingsActivity  ← Resumed
  Calculator.MainActivity      ← Stopped
  Shell.HomeActivity           ← Stopped

משתמש לוחץ Back:
  Calculator.SettingsActivity  → onPause → onStop → onDestroy
  Calculator.MainActivity      → onRestart → onStart → onResume

Stack after:
  Calculator.MainActivity      ← Resumed
  Shell.HomeActivity           ← Stopped
```

## 6. Process States (מבחינת ה-kernel)

בנפרד מ-Activity states, ל-process יש state משלו:

- **FOREGROUND** — יש לו activity ב-Resumed state. לא ייהרג אלא במצב חירום.
- **VISIBLE** — יש activity ב-Started (paused, not in focus). לא ייהרג אלא אם חייב.
- **BACKGROUND** — כל ה-activities ב-Stopped. מועמד להריגה.
- **CACHED** — אין activities, אבל process נשמר ב-memory למען מהירות restart.
- **EMPTY** — process ריק, מועמד מיידי להריגה.

### Low-Memory Killer

כש-MemoryManager מגלה מחסור:
1. נהרג CACHED processes.
2. אם עדיין מחסור → EMPTY, אז BACKGROUND.
3. VISIBLE רק במקרה חירום.
4. FOREGROUND אף פעם לא מופסק אוטומטית.

לפני הריגה: `onSaveState` נקרא, state נשמר ב-StorageManager.

## 7. Recovery

### App crash

אם wasm instance זורק panic / traps:
1. ActivityManager מציין activity = CRASHED
2. WindowManager מציג toast "Calculator crashed"
3. משתמש יכול:
   - לחץ Back → חזרה לקודמת
   - Tap Restart → instance חדש נוצר עם saved state

### Process killed ע"י OS

1. Activity state = KILLED
2. אם משתמש חוזר אליה ב-back stack → relaunch עם saved state
3. משתמש לא אמור להבחין בהבדל (אלא אם state לא נשמר כהלכה)

## 8. Multi-Display Behavior

ActivityInstance מכילה `display_id`. WindowManager מחליט איזה display מציג מה.

תרחישים:
- Single display (tab אחד פתוח): כל activities באותו display.
- Multi display (מספר tabs): Shell רץ ב-display 1, אפליקציה שהמשתמש פתח ב-tab 2 → display 2.
- תנועת activity בין displays (עתיד): `WindowManager.move_activity(id, target_display)`.

## 9. הצהרה במניפסט

```toml
[[activities]]
id = "main"                              # ID בתוך ה-app
class = "CalculatorMainActivity"         # שם סימבולי, ישמש ב-logs
launch_mode = "standard"                 # standard | singleTop | singleTask | singleInstance
task_affinity = "com.akiva.calculator"   # ברירת מחדל = package_id
exported = true                          # האם apps אחרות יכולות להריץ?
entry = true                             # זו ה-activity שנפתחת כברירת מחדל?

[[activities.intent_filters]]
actions = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]
```

## 10. דוגמת lifecycle מלאה

### Scenario: משתמש פותח Calculator, עובר ל-Shell, חוזר למחשבון, הורג את ה-process ע"י OS, חוזר שוב.

```
T=0   Shell.Home in Resumed state
T=1   User taps Calculator icon
T=1   Shell.Home → onPause
T=1   Calc.Main → onCreate(savedState=null)
T=1   Calc.Main → onStart → onResume
T=1   Shell.Home → onStop
T=10  User presses system back to Shell
T=10  Calc.Main → onPause → onStop (but NOT onDestroy)
T=10  Shell.Home → onRestart → onStart → onResume
T=10  Calc.Main onSaveState called → state saved to storage

(later, low memory)
T=60  OS kills Calculator process. Calc.Main state = KILLED.

T=100 User taps Calculator icon again
T=100 Shell.Home → onPause
T=100 ActivityManager sees saved state exists
T=100 New Calc process created
T=100 Calc.Main → onCreate(savedState=<restored>)
T=100 Calc.Main → onRestoreState
T=100 Calc.Main → onStart → onResume
T=100 Shell.Home → onStop

(User sees same state as when left — display value, etc.)
```

זה הסטנדרט של אנדרואיד. אנחנו מיישמים אותו ב-Rust עם wasm.
