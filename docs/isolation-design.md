# Phase 11 — Process / Fault Isolation

מסמך התכנון של עמוד 1 ב-v0.3. **זה המסמך המנחה לכל ששת השבועות הקרובים.** כל החלטה שלא תיסגר כאן — תילקח בחיפזון בזמן הקוד, וקרוב לוודאי תיגרר חזרה לתכנון אחרי שבוע-שבועיים.

המסמך לא מתאר את המצב הסופי באופן מלא. הוא מתאר את **המודל**, את ה-**trade-offs שנבחרו**, ואת ה-**מסלול** — לא כל פרט קוד. פרטי קוד נכתבים ב-stones.

---

## 1. למה Phase 11 קיים

### 1.1 הבעיה במשפט אחד

כל אפליקציה ב-Akiva רצה ב-**אותו Web Worker**, על אותו event loop. אפליקציה אחת שתקועה, מתפוצצת, או מקצה זיכרון בלי גבול — מפילה את כל המערכת.

### 1.2 שלושת התרחישים שבהם המערכת קורסת היום

```rust
// תרחיש 1: infinite loop
fn on_pointer(&mut self, _ctx: &mut Context, _ev: PointerEvent) {
    loop {}
}
// תוצאה: ה-Worker חסום. Shell, kernel, frame loop — כולם מוקפאים.
//        אין דרך לסגור את האפליקציה. רק סגירת ה-tab עוזרת.
```

```rust
// תרחיש 2: panic / wasm trap
fn on_render(&mut self, _ctx: &mut Context, _c: &mut Canvas) {
    panic!("nope");
}
// תוצאה: ה-trap נבלע ב-host call (worker.js לוכד). אבל ה-ActivityRecord
//        נשאר ב-Resumed; ה-state machine של framework מבולבל; ניסיון נוסף
//        לקרוא ל-on_pointer שוב traps. אין mechanism לחזור ל-Shell.
```

```rust
// תרחיש 3: memory leak
fn on_create(&mut self, _ctx: &mut Context) {
    let mut v: Vec<u8> = Vec::new();
    loop { v.extend(std::iter::repeat(0).take(1_000_000)); }
}
// תוצאה: ה-Worker מקצה memory בלי גבול עד שהדפדפן הורג את ה-tab כולו.
//        אין למערכת attribution לפי app — היא לא יודעת מי האשם.
//        אין policy ל-"הרוג את האפליקציה הכי פחות חשובה".
```

### 1.3 איך OS אמיתי פותר את זה

ב-Android, Linux, macOS — לכל app יש **process משלו**. ה-OS יכול:
- להרוג process בודד בלי לפגוע באחרים
- למדוד CPU/memory לפי PID
- להמתין ל-process עם timeout, ואם לא מגיב — `kill -9`

הדפדפן לא נותן לנו processes, אבל הוא **כן** נותן Workers. כל Worker הוא thread נפרד, עם heap נפרד, שאפשר להרוג עם `worker.terminate()`. זה ה-equivalent הקרוב ביותר ל-process בסביבת הדפדפן.

### 1.4 המטרה של Phase 11 בשורה אחת

**כל אפליקציה רצה ב-Worker משלה. ה-kernel יושב ב-Worker הראשי. כשל באפליקציה לא מפיל את המערכת.**

---

## 2. המצב הנוכחי בפירוט

לפני שנדבר על הפתרון, צריך להבין מדויק מה קיים.

### 2.1 המבנה הנוכחי

```
Browser tab
└── Web Worker (יחיד) — runtime/web/worker.js
    ├── kernel wasm                          (akiva-kernel.wasm)
    │   └── framework בתוך ה-kernel
    │       ├── ActivityManager
    │       ├── PackageManager
    │       ├── IntentRouter
    │       └── PermissionController
    └── App registry — runtime/web/app_registry.js
        └── pool של WebAssembly.Instance per package:
            ├── sys.shell                    instantiated, exports.akiva_app_*
            ├── com.akiva.calculator         instantiated, exports.akiva_app_*
            ├── com.akiva.hello              instantiated, exports.akiva_app_*
            └── com.akiva.tictactoe          instantiated, exports.akiva_app_*
```

### 2.2 איך call זורם היום

לחיצת עכבר על השעון ב-Shell:

```
Canvas pointerdown                   (main thread, tab.js)
  │
  └─► postMessage → worker.js
       │
       └─► kernel.akiva_on_event(...)            (sync call into wasm)
            │
            └─► EventQueue.push                  (rust)
                 │
                 └─► next frame:
                      Kernel.handle_event
                       └─► ActivityManager.dispatch_pointer
                            └─► AppRegistry.onPointer(handle, x, y, ...)   (rust → js callback)
                                 │
                                 └─► proc.instance.exports.akiva_app_pointer(handle, x, y, ...)
                                      │
                                      └─► Shell on_pointer (rust, wasm)
                                           │
                                           └─► ctx.start_activity(&intent)
                                                │
                                                └─► akiva_host_start_activity   (host import)
                                                     │
                                                     └─► AppRegistry resolves intent
                                                          └─► framework launches calculator
                                                               └─► calculator.akiva_app_create(handle, ...)
                                                                    │ (sync: returns when done)
                                                                    └─► ...
```

ה-flow הזה הוא **sync לחלוטין מקצה לקצה**. שורה אחרונה לא חוזרת עד שכל ה-tree של calls מסתיים. זאת הסיבה שאם calculator תקועה ב-loop, ה-pointerdown של Shell לא חוזר — וה-event loop של ה-Worker חסום.

### 2.3 שלושת ה-coupling points הקריטיים

הסיבה שקשה לפצל היום:

1. **`ctx.start_activity(&intent)` הוא sync.** ה-app מצפה ש-call יחזור (אם כי בלי ערך). היום זה עובד כי ה-host call נכנס מיידית ל-framework code שרץ באותו thread.

2. **`pkgs::list()` מחזיר Vec.** ה-app קורא, מקבל buffer, decoder, vector. זה sync round-trip.

3. **lifecycle callbacks הם sync.** framework קורא `on_create`, ה-call לא חוזר עד ש-on_create גמרה. אז framework קורא ל-`on_start`. אם ה-app רץ ב-Worker אחר, postMessage היא inherently async — הקריאה הבאה צריכה להמתין ל-ack.

---

## 3. ההחלטה הקריטית — sync facade vs async API

זאת **ההחלטה המכרעת** של Phase 11. כל החלטה אחרת נגזרת ממנה.

### 3.1 שלוש האופציות

#### אופציה A — Pure async API

כל host call ב-SDK מחזיר Future. App code:

```rust
// היום
fn on_pointer(&mut self, ctx: &mut Context, ev: PointerEvent) {
    ctx.start_activity(&intent);
}

// אופציה A
async fn on_pointer(&mut self, ctx: &mut Context, ev: PointerEvent) {
    ctx.start_activity(&intent).await;
}
```

**בעד:** clean. נכון מלמעלה למטה. אין hidden blocking.

**נגד:** כל ה-SDK משתנה. כל אפליקציה צריכה rewrite. ה-`Activity` trait הופך ל-async trait — שזה לא טריוויאלי ב-no_std wasm. התרגום `Future` → polling ב-rust no_std ידרוש executor פנימי. אנחנו מוסיפים ~3-5kb למשקל wasm לכל אפליקציה רק כדי שיכלו לקרוא ל-`.await`.

#### אופציה B — Sync facade עם `Atomics.wait` (SharedArrayBuffer)

ה-API ל-app נשאר sync. מתחת, host call כותב request ל-SharedArrayBuffer, שולח notification ל-main worker, וקורא ל-`Atomics.wait` עד שהתשובה כתובה.

```rust
// API נשאר זהה לגמרי
fn on_pointer(&mut self, ctx: &mut Context, ev: PointerEvent) {
    ctx.start_activity(&intent);
}

// מתחת:
// host import akiva_host_start_activity:
//   1. write intent bytes to SAB at offset
//   2. set "request type" = START_ACTIVITY
//   3. Atomics.notify(sab, control_offset)
//   4. Atomics.wait(sab, ack_offset, 0)        ← worker חוסם פה
//   5. main worker מתחזק את ה-call, כותב תשובה, Atomics.notify
//   6. host import חוזר
```

**בעד:** API ל-apps לא משתנה. 4 האפליקציות הקיימות + SDK ממשיכים לעבוד עם binary compatibility מלא לאחר re-build. ה-mental model של מפתחי app נשאר identicall.

**נגד:** דורש cross-origin isolation (COOP `same-origin` + COEP `require-corp`) כדי ש-SharedArrayBuffer יהיה זמין. זה משמעותי — צריך headers בdevserver וב-production. גם: blocking ב-Worker עצמו טבעי, אבל יש מי שיטען ש-"`Atomics.wait` ב-app worker זה anti-pattern".

#### אופציה C — Hybrid (sync facade עם split)

Calls "fire-and-forget" (אין ערך החזרה) הם async תחת המכסה — ה-app שולח message, חוזר מיידית, ה-call יבוצע מאוחר יותר. Calls "result-bearing" (`pkgs::list`, `read_file`) משתמשים ב-`Atomics.wait` כמו אופציה B.

**בעד:** 90% מה-calls הם fire-and-forget (logging, start_activity, set_result). על השאר אנחנו משלמים את העלות של SAB. ההפרדה מבהירה לעצמה את ה-cost model.

**נגד:** שתי roads ל-call. אם app מצפה לסדר temporal, fire-and-forget יכולים להגיע out-of-order. צריך per-app FIFO ordering בכל מקרה — זה מבטל חלק מה-saving.

### 3.2 ההחלטה: **אופציה B** — sync facade עם `Atomics.wait`

**הסיבות:**

1. **שמירה על ה-API לאפליקציות.** יש לנו SDK עם 4 אפליקציות עובדות. כל אחת מהן 100-300 שורות. שבירת ה-API דורשת לעדכן 4 crates + sdk-rust + כל ה-tests. עלות גבוהה ללא תועלת ארכיטקטונית — באמת הרי האפליקציה רוצה שהcall יחזור (היא רוצה לדעת שה-activity התחילה, או שה-pkgs רשימה הגיעה).

2. **`Atomics.wait` הוא בדיוק מה ש-OS עושה.** ב-Linux, syscall זה user→kernel context switch שחוסם את ה-userspace thread עד שה-kernel חוזר. אין הבדל מהותי בין `Atomics.wait` ב-app worker לבין `int 0x80` ב-Linux process. ה-app worker הוא user thread, main worker הוא kernel.

3. **COOP/COEP הם ממילא דרישה לעתיד.** אם נרצה אי-פעם `precise high-resolution timer` (`performance.now()` עם microsecond precision), `Worker.terminate` שעובד מהר, או high-precision shared memory — נצטרך אותם. עדיף לשלם עכשיו.

4. **C כאופטימיזציה אחרי שמודדים.** אם נמצא שbursts של `log_info!` מציפים את ה-Atomics path, נוכל להוסיף fast-path async-fire-and-forget רק עבור log calls. אבל זה אופטימיזציה, לא ארכיטקטורה.

### 3.3 מה זה אומר טכנית

עבור כל app worker יש:

- **SAB אחד** של גודל קבוע (נניח 64KB). Layout:
  - bytes 0-3:   request type (u32, 0 = idle)
  - bytes 4-7:   ack flag (u32, 0 = pending, 1 = done)
  - bytes 8-11:  response length (u32)
  - bytes 12-15: response status (u32, 0 = ok, !=0 = error)
  - bytes 16-32K: request payload buffer (caller writes here)
  - bytes 32K-64K: response payload buffer (callee writes here)

- **MessagePort** בין main worker ל-app worker:
  - notifications של "request ready" אם רוצים פלטפורמה גם בלי SAB (fallback path, אופציונלי)
  - lifecycle commands (create/start/resume/pause/stop/destroy) שאינם host calls

- **בproject conventions:**
  - `akiva_host_*` imports ב-app — נשארים בשמותיהם, אבל המימוש ב-app worker הופך ל-write-to-SAB + wait
  - `hal_host_*` externs ב-kernel — נשארים sync (kernel ב-main worker, אין barrier)

---

## 4. המודל: worker-per-app

### 4.1 ההחלטה: package = unit of isolation

יש שלוש granularities אפשריות:

- **worker-per-activity** — כל activity ב-Worker משלו. פתרון אקדמי לטוב ולרע: container אבטחתי מקסימלי, overhead מקסימלי.
- **worker-per-task** — task הוא back-stack. activities מאותו task באותו worker. הבעיה: tasks יכולים לכלול activities מ-packages שונים (cross-app intent). שיתוף heap בין packages חוזר להיות פגיע.
- **worker-per-app** — package הוא unit. כל ה-activities של calculator באותו worker. אין שיתוף heap בין packages.

**ההחלטה: worker-per-app.**

זה גם מה ש-Android עושה (מתוך תיאוריה — בפועל אפשר לחלק אבל ברירת המחדל היא process-per-app), וזה מה שהמשתמש מצפה: "תהרוג את calculator" → ההצפנה של calculator נמחקת, השאר חי.

### 4.2 ה-topology המבוקש

```
Browser tab
├── tab.js (main thread)              ─── שכבת DOM/canvas
│
└── kernel-worker (main Web Worker)   ─── kernel wasm + framework
    │
    ├──► port-MessagePort + SAB ────► app-worker-shell                    (Shell)
    │                                  └── sys.shell wasm instance
    │
    ├──► port-MessagePort + SAB ────► app-worker-calculator
    │                                  └── com.akiva.calculator wasm instance
    │
    ├──► port-MessagePort + SAB ────► app-worker-hello
    │                                  └── com.akiva.hello wasm instance
    │
    └──► port-MessagePort + SAB ────► app-worker-tictactoe
                                       └── com.akiva.tictactoe wasm instance
```

**התפקידים:**

- **tab.js** — כמו היום, אין שינוי. כל input/output עובר דרך kernel-worker.
- **kernel-worker** — מחזיק את kernel wasm. גם מחזיק את framework (ActivityManager וכו'). הוא ה-"system process".
- **app-worker** — Web Worker שטעון את `app_worker_shell.js`. בתוכו טעון wasm instance של app אחד. הוא לא ניגש ישירות ל-DOM, ל-OPFS, או לkernel — הכל עובר דרך SAB+postMessage ל-kernel-worker.

### 4.3 mapping של callbacks

```
ב-kernel-worker (framework):                      ב-app-worker-X:
─────────────────────────────────                ──────────────────────────────────
launch(intent):                                  worker שומר wasm instance טעון
  spawn app-worker (אם לא קיים)                  ולא עושה כלום עד שמגיע command
  send LIFECYCLE("create", handle, intent_blob)  ◄────────────► postMessage
  wait for LIFECYCLE_ACK                                          │
                                                                  ▼
                                                        akiva_app_create(handle, ...)
                                                        sync run inside wasm
                                                                  │
  send LIFECYCLE("start", handle)               ◄──── postMessage LIFECYCLE_ACK("create")
  wait for LIFECYCLE_ACK                                ...
  ...
```

ה-framework לא קורא יותר `proc.instance.exports.akiva_app_create(...)` ישירות. במקום זה:
1. שולח message ל-app worker
2. ממתין ל-ack
3. ממשיך ל-callback הבא

### 4.4 host calls

ה-flow המלא של `ctx.start_activity(&intent)` מתוך אפליקציה:

```
[app-worker-shell]
├── Shell::on_pointer רץ ב-wasm
└── ctx.start_activity(&intent)
     ├── encode intent → bytes
     ├── write to SAB request buffer
     ├── SAB[0] = REQ_START_ACTIVITY
     ├── SAB[4] = 0 (ack pending)
     ├── postMessage({type:"req"}) → kernel-worker (notification)
     └── Atomics.wait(sab, 4, 0)            ← חוסם פה
                  │
[kernel-worker]   │
├── onmessage("req") from shell-port
├── read SAB[0] → REQ_START_ACTIVITY
├── read intent bytes from SAB
├── frameworks.intents.resolve(intent) → (target_pkg, class)
├── ActivityManager.launch(target_pkg, class)
│    └── (recursively talks to app-worker-calculator the same way)
├── write response to SAB
├── SAB[4] = 1 (ack)
├── Atomics.notify(sab, 4)
                  │
                  │
                  ▼ ← woken
[app-worker-shell]
├── read response from SAB
└── return from start_activity
```

**critical detail:** ה-Atomics.wait ב-app worker חוסם **רק את ה-app worker**. ה-kernel-worker חופשי לטפל ב-events של apps אחרים. אם calculator מבקשת `read_file` במקביל ל-`start_activity` של Shell, kernel-worker מטפל בשניהם בseries (עם FIFO order לכל port), ושני האפליקציות רצות במקביל ב-CPU.

---

## 5. OPFS — האילוץ הקשה ביותר

### 5.1 הבעיה

OPFS `SyncAccessHandle` הוא הדרך היחידה לעשות **synchronous I/O** ב-OPFS. הוא חיוני ל-`StorageEngine` שיש לנו (calculator קוראת/כותבת ב-`on_create`/`on_save_state` — sync). אבל יש מגבלה:

> Only one `SyncAccessHandle` per file can exist at a time.

נגיד kernel-worker פתח `sys/permissions/com.akiva.calculator`. עכשיו calculator app-worker רוצה לפתוח את הקובץ שלה `com.akiva.calculator/savedstate`. זה קובץ אחר — אפשר. אבל:

- כל app-worker צריך לפתוח את **קבציו הוא**
- אם app-worker מנסה לפתוח קובץ שhandle שלו פתוח ב-kernel-worker, נכשל

עוד מסובכות:
- אם app-worker נסגר (`worker.terminate()` או failure), ה-handles שלו לא מתרוקנים אוטומטית בזרימה רגילה. ה-browser בסוף משחרר אבל יש חלון שבו handle "תקוע".
- מערכת הבדיקה של `kernel-worker` (boot scan, install, uninstall) דורשת גישה ל-storage של אפליקציות אחרות.

### 5.2 שלוש אופציות

#### אופציה I: כל ה-storage עובר דרך kernel-worker

ה-app קורא `storage.write(key, value)` → host call → SAB → kernel-worker → kernel-worker מחזיק את ה-handle → returns OK.

**בעד:** מודל פשוט. Single source of truth. kernel-worker יכול לאכוף sandbox prefix (`{package_id}/...`) באופן אטומי.

**נגד:** כל write עובר Atomics.wait. אם calculator שומרת state ב-`on_pause`, זה round-trip ל-kernel-worker. עלות: ~0.1ms-1ms פר call.

#### אופציה II: app-worker מחזיק handles משלו, kernel-worker מתואם

ה-app-worker פותח SyncAccessHandles בעצמו. kernel-worker יודע שיש partition של handles ולא נוגע בקבצים של apps פעילות.

**בעד:** storage I/O מהיר.

**נגד:** מורכב. אם kernel-worker רוצה לעשות `uninstall(calculator)`, הוא חייב לקרוא ל-app-worker "תשחרר את ה-handles", להמתין, ואז למחוק. כל שגיאה ב-protocol הזה משאירה לnefarious states.

#### אופציה III: hybrid

קבצים של ה-app בלבד (`{package_id}/*`) — handle ב-app-worker. קבצים של מערכת (`sys/*`) — kernel-worker בלבד.

**בעד:** המסלול המהיר עבור user data, kernel נשאר authoritative על system data.

**נגד:** עדיין יש את בעיית uninstall. ועוד בעיה: הצורך לוודא שבזמן שapp פעיל, kernel-worker לא נוגע בקבציה — אבל מה אם ה-kernel-worker צריך לעשות migration?

### 5.3 ההחלטה: **אופציה I** — הכל עובר דרך kernel-worker

**הסיבות:**

1. **הbenchmarks שלנו לא צריכים את אופציה II.** Calculator שומרת state פעם בכל `on_pause` (פעם בליצוץ). 1ms latency לא מורגש. אם נמצא ש-app אמיתי דורש high-throughput storage, נוכל לחזור — אבל בלי data, optimization premature.

2. **uninstall הופך טריוויאלי.** kernel מחזיק את ה-handles, יודע מה פתוח, יכול לסגור-ולמחוק.

3. **enforcement חזק.** ה-sandbox prefix `{package_id}/` נאכף ב-kernel-worker רגעי לפני ה-write. ב-app-worker אין דרך לבקש מפתח שלא מתחיל ב-`{package_id}/`.

4. **לא דורש מצב חדש.** kernel-worker היום עושה את זה (כל ה-storage עובר דרכו). השינוי הוא רק ש-host call עובר דרך SAB במקום call ישיר.

### 5.4 implication על ה-storage API

ה-`akiva_sdk::storage` API נשאר זהה. תחת המכסה, כל פונקציה הופכת ל-host call עם round-trip ל-kernel.

Latency budget: storage call ≤ 2ms (round-trip + serialization). אם נמצא ש-app אמיתי משוחק חרוצה storage לטעינה ראשונית (`on_create` עם 50 reads), נוכל להוסיף `bulk_read(keys: &[&str]) → Vec<Result<Vec<u8>>>` ל-API.

---

## 6. Lifecycle כ-state machine async

### 6.1 הבעיה

היום (sync):

```rust
// framework/src/activity_manager.rs (ב-kernel-worker)
pub fn drive_to_resumed(&mut self, handle: ActivityHandle) {
    self.dispatch(handle, "create");   // sync — חוזר אחרי שה-app גמרה
    self.dispatch(handle, "start");
    self.dispatch(handle, "resume");
}
```

מחר (async):

```rust
// כל dispatch הוא postMessage. אנחנו לא יכולים להמשיך מיד.
// אנחנו צריכים state machine שיודע "ה-create עוד לא חזר, אל תקרא ל-start".
```

### 6.2 המודל

הכוונה לאמץ את אותו מודל שLooper של Android משתמשת בו: **המגיש מחזיק את ה-state, ה-app רק מודיע "סיימתי שלב X"**.

```
ActivityRecord:
  state:           Created | Started | Resumed | Paused | Stopped | Destroyed
  pending:         Option<{ target: state, request_id: u64 }>
  inflight_at:     Option<Timestamp>     ← לwatchdog
```

```
פונקציה drive_to(target_state):
  match (current, target):
    Created, Resumed → pending = (target=Created, ...); send "create";
                       on ack → pending = (target=Started, ...); send "start";
                                on ack → pending = (target=Resumed, ...); send "resume";
                                         on ack → state = Resumed; pending = None
```

זאת מכונת מצב. ה-framework לא מתבייש שהיא async — היא חיה ב-tick של kernel frame loop. כל frame, היא בודקת:

1. האם יש pending transition שעבר timeout? → trapped
2. האם יש queued lifecycle requests מ-IntentRouter? → התחל transition חדש

### 6.3 implication על ה-API של framework

הפונקציות שכותבי framework הקיימות מצפות:

```rust
// היום
let result = activity_manager.dispatch_pointer(handle, ev);

// מחר
activity_manager.queue_pointer(handle, ev);  // queue ב-app-worker port
// ה-result מגיע ב-frame הבא דרך onmessage; framework מטפל
```

`queue_pointer` הוא fire-and-forget. אם ה-app לא מטפל ב-event, מודלית — הוא יאבד אחרי X frames של queueing. ה-framework לא צריך לחכות.

### 6.4 implication על tests

ה-test suite היום (141 framework tests) רץ עם `MockHost` שmocks את ה-host calls. עם async lifecycle:

- ה-`MockHost` ימשיך לעבוד **כי הוא לא Worker.** הוא mock — קורא ל-callbacks ישירות.
- tests ש-assert ש-after `drive_to_resumed`, ה-state הוא `Resumed` — ימשיכו לעבור, כי ב-MockHost ה-acks באים בו ברגע.
- tests חדשים נצטרכו עבור: timeout, dropped messages, out-of-order delivery (אם יקרה).

ה-עיקרון: **MockHost נשאר sync עבור unit tests של framework.** התקצירו async-or-sync הוא אובחנת ה-WasmHost trait — ה-Worker implementation הוא async (postMessage), MockHost sync. ה-framework עצמו הוא async-aware: אם השcallback לא קורה מיידית, הוא ממתין. ב-MockHost הוא מיידית, אז הוא לא ממתין. ב-Worker host הוא לא מיידית, אז הוא ממתין.

---

## 7. fault containment — מצב Trapped

### 7.1 שלוש דרכי כשל

1. **wasm trap** (panic, unreachable, out-of-bounds) — ה-call ל-`exports.akiva_app_*` זורק. ב-app-worker, נתפס ב-`try { ... } catch (e) { postMessage("trap", e) }`.
2. **app-worker crash** — exception לא נתפסת ב-app worker top-level. `worker.onerror` ב-kernel-worker יורה.
3. **app-worker hang** — `loop {}` או `while(true) {}`. ה-worker לא מגיב ל-postMessage. נתפס ב-watchdog (סעיף 8).

### 7.2 מצב חדש: `LifecycleState::Trapped`

```rust
pub enum LifecycleState {
    Initialized,
    Created,
    Started,
    Resumed,
    Paused,
    Stopped,
    Destroyed,
    Trapped,   // ← new
}
```

`Trapped` הוא **terminal**. אין transition out. הוא מסמן: "ה-activity הזה מת ולא יחזור."

### 7.3 cleanup flow

כש-activity עוברת ל-Trapped:

1. ה-framework כבר הספיק לקבל `worker.onerror` או `Atomics.wait` timeout.
2. `ActivityManager.mark_trapped(handle)` מעדכן את ה-state.
3. כל activity אחרת של אותו package שהיא ב-Resumed/Paused/Stopped — גם הן **חייבות** לעבור ל-Trapped (heap משותף — ה-Worker הוא אחד).
4. `worker.terminate()` נקרא על ה-app-worker.
5. אם המ-Trapped activity הייתה ב-foreground task → ActivityManager pop's אותה, מקדם את ה-activity הקודמת ב-stack ל-Resumed (זה Shell ברוב המקרים).
6. event log: `[kernel] app com.akiva.calculator trapped: <reason>`.

### 7.4 re-launch

אחרי Trapped, אם המשתמש בוחר את האפליקציה שוב מ-Shell:

1. ActivityManager.launch(intent) — נקרא רגיל.
2. AppRegistry בודק: יש app-worker חי ל-`com.akiva.calculator`? **לא** (terminated).
3. מקים app-worker חדש.
4. טוען wasm מ-OPFS.
5. עובר ל-state machine רגיל (Initialized → Created → ...).

ה-app מתחיל מאפס. saved state מ-OPFS אם יש. **משאירים זאת בדיוק ככה — אין fallback "trapped recovery".** הסיבה: אם ה-app trapped בגלל bug ב-on_create, recovery יציאה ל-trap מיידי שוב. עדיף clean restart.

---

## 8. watchdog

### 8.1 מודל

ה-kernel-worker מחזיק שעון. כל N frames (נניח N=60, כלומר פעם בשנייה ב-60fps), הוא:

1. עבור כל app-worker חי, שולח message `{type: "ping", id: <counter>}`.
2. שומר `last_ping_sent_at[worker_id] = now()`.
3. ב-app-worker, יש handler שאינו עובר דרך wasm: `if (msg.type === "ping") postMessage({type:"pong", id})`. זאת JS-level callback ב-`app_worker_shell.js`. **אין wasm involvement.**
4. כש-pong מגיע, kernel-worker מעדכן `last_pong_at[worker_id] = now()`.

אם בbroadcast הבא (frame N=60 + עוד 60 = N=120), ה-`last_pong_at` עדיין ישן יותר מ-`last_ping_sent_at` הקודם — ה-worker לא ענה ב-1 שנייה. → trap.

### 8.2 למה JS-level pong, לא wasm

זה מפתח. אם ה-app רץ ב-`loop {}` בתוך `on_pointer`, ה-wasm לא חוזר. אבל:

- ה-wasm **לא מקבל** את ה-postMessage event — postMessage נכנס ל-event loop של ה-Worker, וה-event loop חסום על ידי הקריאה ל-wasm function (`exports.akiva_app_pointer`) שלא חזרה.
- אז ה-pong handler לא רץ.
- watchdog ב-kernel-worker מזהה.

**זה בדיוק מה שאנחנו רוצים.** ה-watchdog detects busy-loops ב-app בלי להזדקק לטריגר נוסף.

### 8.3 קצב

- ping פעם בשנייה (60 frames @ 60fps).
- timeout: 1 שנייה.
- אם user app נחסם 1 שנייה — אנחנו הורגים אותו. זה מותאם למחשבה: אנחנו OS, לא debugger. אם 1 שנייה של app אומרת "תקועה", אנחנו לא מצפים ש-on_pointer יצא ל-CPU thrashing.

אפשרות עתידית: hierarchy של timeouts — 100ms warning ("האפליקציה איטית"), 1s trap ("האפליקציה תקועה"). MVP: רק trap.

---

## 9. סדר הbuild — סטונים

מסלול ה-implementation:

### Stone 11.1 — Design doc (זה)
**Definition of Done:** מסמך זה מאושר.

### Stone 11.2 — Async ABI prototype
- כותב `runtime/web/app_worker_shell.js` — ה-shell של app worker.
- `hello` רץ ב-app-worker שלו עם host calls מינימליים (`log_info!` בלבד).
- מודד overhead: כמה ms פר log call? כמה frames drops?
- אם overhead > 1ms — חזרה ל-design (אולי SAB layout שונה).

**Definition of Done:** hello מופיע על המסך, הוא רץ ב-Worker נפרד (ראיה: `console.log` מתוך הworker), overhead מתועד.

### Stone 11.3 — Framework lifecycle async
- `WasmHost` trait עם async lifecycle methods.
- `ActivityRecord` מקבל `pending` slot.
- `MockHost` ממשיכה sync.
- `WorkerHost` (חדש) ממשמת async דרך SAB.
- 141 tests עוברים.

**Definition of Done:** כל 4 האפליקציות רצות ב-Workers נפרדים, ה-test suite עובר.

### Stone 11.4 — SDK adjustments
- ה-host imports ב-SDK עוברים ל-SAB protocol (`runtime.rs` בלבד; ה-API של app נשאר זהה).
- בדיקות: אפליקציה שכותבת `ctx.start_activity` ב-`on_pointer` באמת מפעילה את calc/hello/וכו'.

**Definition of Done:** Shell launching apps עובד end-to-end ב-isolated workers.

### Stone 11.5 — Fault containment
- `LifecycleState::Trapped`.
- `worker.onerror` → mark Trapped.
- `try/catch` סביב כל call ל-wasm exports ב-app-worker-shell → mark Trapped.
- `ActivityManager.cleanup_trapped(handle)`.
- Integration test: app שעושה `panic!("test")` ב-`on_pointer`, Shell ממשיכה לעבוד.

**Definition of Done:** ה-test הנ"ל עובר; manual smoke: לחיצה ב-calc-pointer-panic מחזירה ל-Shell, calculator לא רץ יותר.

### Stone 11.6 — Watchdog
- ping/pong loop ב-kernel-worker.
- termination אחרי timeout.
- log + Trapped mark.
- Integration test: app עם `loop {}` ב-on_pointer, ה-system מזהה, הורג, חוזר ל-Shell. סה"כ ≤ 1.5s.

**Definition of Done:** ה-test עובר; כל 4 האפליקציות הקיימות נשארות פונקציונליות.

---

## 10. Exit criteria של Phase 11

- [ ] hello + calculator + Shell + tic-tac-toe רצים ב-4 Workers נפרדים. אישור: `console.log` מ-`app_worker_shell.js` עם packageId.
- [ ] App עם `loop {}` ב-`on_pointer` לא מקפיא את המערכת; הוא מת ב-≤ 1.5s.
- [ ] App שעושה `panic!()` ב-`on_render` — ActivityRecord עובר ל-Trapped, framework cleanup, Shell ממשיכה ב-resumed.
- [ ] `141 framework tests + 146 sdk tests + 68 kernel tests + 50 vitest` כולם עוברים. ייתכן שיתווספו tests חדשים (10-20 לcontainment, watchdog, async lifecycle). אין regressions.
- [ ] Host call overhead ≤ 1ms בדפדפן סטנדרטי (Chrome/Firefox latest על Linux x86_64). מדידה תועד ב-`docs/perf-baseline.md`.
- [ ] storage roundtrip עבור `set/get` בודדים ≤ 2ms. אם > 2ms — לתעד ולא לחסום.

**הערכת זמן:** 6-10 שבועות.

---

## 11. סיכונים וtrade-offs ידועים

### 11.1 סיכון: COOP/COEP headers

`SharedArrayBuffer` דורש cross-origin isolation:
- `Cross-Origin-Opener-Policy: same-origin`
- `Cross-Origin-Embedder-Policy: require-corp`

זה עלול להיכשל אם משתמש מפעיל את Akiva מ-context שאוסר אחד מהם (extension content-script, certain iframes). ה-mitigation:
- ה-devserver של Akiva CLI ייצא את ה-headers מתחילה.
- `runtime/web/index.html` יבדוק אם `crossOriginIsolated === true`. אם לא — banner מפורש "Akiva requires cross-origin isolation."
- ב-MVP אין fallback ל-MessageChannel-only. אם נצטרך, נוסיף מאוחר יותר.

### 11.2 סיכון: הגישה ל-OPFS מאט את כל ה-storage

כל read/write עוברים round-trip ל-kernel. אם נמצא בעיה — נוסיף batching בעתיד. לא חוסם MVP.

### 11.3 סיכון: `worker.terminate()` לא מנקה הכל

הדפדפן בדרך כלל משחרר את ה-resources של Worker שterminated, אבל לא מבטיח timing. אם ה-app אחז SyncAccessHandle לקובץ של עצמו (אופציה II שדחינו) זה יכול להיות בעיה. תחת אופציה I (שבחרנו) זה לא בעיה — kernel מחזיק handles.

### 11.4 trade-off: complexity vs throughput

המודל שבחרנו (sync facade + Atomics.wait + worker-per-app + storage-via-kernel) מוסיף complexity מהותית למערכת. אם בעוד שנה נחליט שהיינו רוצים async לעומק (אופציה A) — נצטרך rewrite שני. אבל:
- אופציה A דורשת שינוי SDK שובר תאימות. עדיף לעשות פעם אחת בעתיד עם 4+ אפליקציות מ-בpאזרס מ-100+ אפליקציות.
- אופציה B נותנת isolation מיידית עם זברה minimal לאפליקציות קיימות.

החלטה: **לקבל את ה-complexity. תיעוד מקיף ב-stones כדי שתהיה revisable.**

### 11.5 סיכון לא-מתועד: BroadcastChannel cross-tab

היום יש leader election בין tabs. עם פיצול ל-workers, מה קורה אם follower-tab רוצה לשלוח event ל-leader-tab? ב-leader-tab, ה-event מגיע ל-tab.js → kernel-worker → איך מגיע ל-app-worker?

**תשובה:** ה-event flow לא משתנה. tab.js שולח ל-kernel-worker. kernel-worker (framework) מחליט לאיזה app-worker זה הולך, ושולח. ה-cross-tab BroadcastChannel נשאר בין tabs, לא בין workers בתוך tab. ראה [`runtime-web.md §3-4`](runtime-web.md).

### 11.6 סיכון: ה-bundle הולך ויגדל

עוד JS file (`app_worker_shell.js`), עוד SAB layouts, עוד protocols. צופים תוספת של ~5-10KB ל-`runtime/web/`. בbenchmark של גודל מערכת — אנחנו עדיין מתחת ל-100KB total bundle (תוספת זו לא תוציא אותנו מאותו order).

---

## 12. מה מסמך זה **לא** מטפל בו

נשמר ל-Phase 12 (Resource Governance):

- **memory accounting per app.** Phase 12 משלב מדידה.
- **CPU attribution.** דורש את ה-Worker isolation שPhase 11 בונה — Phase 12 מודד עליו.
- **Low-Memory Killer.** Policy של "מי מת ראשון" — Phase 12.
- **App suspension** (CACHED state). Phase 12.

נשמר ל-Phase 13 (WindowManager completion):

- **ויסות frames בין apps.** היום כל app מצייר על surface משלו ב-resume. עם isolation, frame budget בין apps יידרש management. Phase 13.

נשמר לפינ דחוי לחלוטין:

- **`async fn` ב-Activity trait.** ראה סעיף 3.1 אופציה A.
- **`Future`-based SDK API.** ראה סעיף 3.1 אופציה A.

---

## 13. references

- [`ROADMAP.md` Phase 11](../ROADMAP.md) — סטונים ברמה גבוהה.
- [`docs/runtime-web.md`](runtime-web.md) — מבנה current של HAL ב-JS, leader election, event pipeline.
- [`docs/abi.md`](abi.md) — ABI חתום של app↔host. סעיף 11.4 ימשיך להיות תואם.
- [`docs/activity-model.md`](activity-model.md) — lifecycle states. ייתוסף `Trapped`.
- [`docs/storage.md`](storage.md) — OPFS layout. ימשיך להיות תקף; הגישה אליו תעבור דרך SAB.
- [`framework/src/wasm_activity.rs`](../framework/src/wasm_activity.rs) — `WasmHost` trait. ייתוסף async behavior במימוש Worker host.
- [`runtime/web/app_registry.js`](../runtime/web/app_registry.js) — ה-current process pool. ייפצל ל-`app_worker_shell.js` חדש + מנהל workers ב-`app_registry.js`.

---

**מצב המסמך:** Draft 1. דורש review לפני התחלת Stone 11.2.
