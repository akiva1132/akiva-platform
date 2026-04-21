# ארכיטקטורה טכנית — Akiva Platform

## 1. עיקרון מנחה

עקיבא היא **מערכת הפעלה אמיתית**, לא game engine ולא web framework. היא כוללת את כל תתי-המערכות שמצופות מ-OS אפליקטיבית מודרנית: process model, activity lifecycle, resource system עם qualifiers, declarative layout language, intent-based IPC, package manager, window manager. כל אחת מהן **חובה** להיות קיימת, אפילו אם רזה.

"שלד רזה" לא אומר "שלד חסר" — אומר "כל עצם קיים, אבל כל עצם עושה מינימום". אפשר להרחיב בעתיד, אי אפשר "להוסיף" תת-מערכת בסיסית בדיעבד בלי לשכתב הכל.

## 2. מפת תתי-המערכות

```
┌───────────────────────────────────────────────────────────────┐
│                     Akiva User Space                          │
│   ┌──────────┐  ┌──────────┐  ┌──────────┐                    │
│   │ App: Calc│  │ App: Note│  │ App: Shell│  (apps = wasm)     │
│   └─────┬────┘  └─────┬────┘  └────┬─────┘                    │
│         │             │            │                           │
│   ═════════════ ABI boundary ═══════════════                   │
│         │             │            │                           │
├─────────┼─────────────┼────────────┼──────────────────────────┤
│         ▼             ▼            ▼                           │
│                  Akiva Framework                               │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │  ActivityManager   │  PackageManager   │  IntentRouter  │  │
│  │  lifecycle + stack │  app registry     │  routing       │  │
│  ├─────────────────────────────────────────────────────────┤  │
│  │  WindowManager     │  ResourceManager  │  PermissionMgr │  │
│  │  shell composition │  qualifiers + look│  enforcement   │  │
│  ├─────────────────────────────────────────────────────────┤  │
│  │  LayoutInflater    │  LayoutEngine     │  Compositor    │  │
│  │  AKML → widget tree│  measure + place  │  draw commands │  │
│  └─────────────────────────────────────────────────────────┘  │
├───────────────────────────────────────────────────────────────┤
│                    Akiva Kernel (runtime core)                │
│  ┌─────────────────────────────────────────────────────────┐  │
│  │  WasmLoader    │  MemoryManager    │  Scheduler          │  │
│  │  app instances │  per-app memory   │  frame loop + ticks │  │
│  ├─────────────────────────────────────────────────────────┤  │
│  │  EventQueue    │  StorageManager   │  IPCBus             │  │
│  │  input routing │  OPFS-backed      │  intent transport   │  │
│  └─────────────────────────────────────────────────────────┘  │
├───────────────────────────────────────────────────────────────┤
│                    JS HAL (Hardware Abstraction Layer)        │
│   canvas, input events, storage API, wasm loading             │
├───────────────────────────────────────────────────────────────┤
│                    Browser (host kernel)                       │
└───────────────────────────────────────────────────────────────┘
```

### הסבר קצר של כל תת-מערכת

**Akiva Kernel** (הגרעין):
- **WasmLoader** — טוען wasm modules של אפליקציות, מאתחל instances
- **MemoryManager** — עוקב אחרי צריכת זיכרון של כל app, אוכף quotas
- **Scheduler** — מריץ את לולאת ה-frame, מחליט מי מקבל CPU time
- **EventQueue** — מקבל events מה-HAL, מנתב לפי focus
- **StorageManager** — key-value sandboxed per-app, מעל OPFS
- **IPCBus** — מעביר מסרים (intents) בין app instances

**Akiva Framework** (מעל הגרעין, נותן abstractions לאפליקציות):
- **ActivityManager** — מנהל stack של Activities, lifecycle callbacks, task affinities
- **PackageManager** — רישום אפליקציות מותקנות, resolve של intents
- **IntentRouter** — מבצע implicit intent resolution דרך PackageManager
- **WindowManager** — איזה Activity מוצג מתי, animations של מעברים
- **ResourceManager** — טעינת resources עם qualifier matching
- **PermissionMgr** — בודק אם app הכריז על permission ואישור המשתמש
- **LayoutInflater** — מפרסר AKML ובונה widget tree
- **LayoutEngine** — מחשב מידות ומיקומים (measure + layout pass)
- **Compositor** — הופך widget tree אחרי layout ל-draw commands

**JS HAL** (רק זה לא-Rust):
- דקיק. ~500 שורות סך הכל. שום לוגיקה של OS.
- ה-HAL מפוצל פיזית לשניים:
  - **Tab HAL** (`tab.js`, בכל tab): canvas + input events + leader election client. שולח/מקבל מ-Worker דרך MessagePort.
  - **Worker HAL** (`worker.js`, רק ב-leader tab): storage (OPFS), wasm loading, מנהל את ה-runtime ואת ה-renderer.
  - ראה סעיף 9 לפרטי ארכיטקטורת ה-Single-Instance.

## 3. Process Model

### המודל המושגי

כל אפליקציה רצה ב-**wasm instance נפרד**. זה הצורה שלנו ל-"process":
- memory מבודד
- ערך מסוים של quota
- יכול להיהרג (instance destroyed) ולהישחזר (new instance)
- מזוהה ב-`ProcessId` ייחודי

### App States

אותם States שיש באנדרואיד:

```
┌──────────┐
│ NotExists│ ─────────┐
└──────────┘          │
     │                │ (low memory killer)
     │ launch         │
     ▼                │
┌──────────┐          │
│ Created  │          │
└─────┬────┘          │
      │ onStart       │
      ▼               │
┌──────────┐          │
│ Started  │          │
└─────┬────┘          │
      │ onResume      │
      ▼               │
┌──────────┐          │
│ Resumed  │ ◄────────┼─── (user focuses)
└─────┬────┘          │
      │ onPause       │
      ▼               │
┌──────────┐          │
│ Paused   │          │
└─────┬────┘          │
      │ onStop        │
      ▼               │
┌──────────┐          │
│ Stopped  │          │
└─────┬────┘          │
      │ onDestroy     │
      ▼               │
┌──────────┐          │
│ Destroyed│ ─────────┘
└──────────┘
```

### Task Stack

Stack גלובלי של Activity instances (כמו ב-Android):

```
Stack top (user sees this)
┌──────────────────────────┐
│  Notes.EditScreen        │  ← Resumed
├──────────────────────────┤
│  Calculator.MainScreen   │  ← Stopped
├──────────────────────────┤
│  Shell.Home              │  ← Stopped
└──────────────────────────┘
Stack bottom
```

Back button → pop stack top → previous activity resumes.

Launch modes: `standard`, `singleTop`, `singleTask`, `singleInstance` — כמו באנדרואיד.

## 4. Resource System

### מבנה resources

כל אפליקציה כוללת תיקיית `res/`:

```
apps/calculator/
├── manifest.toml
├── calculator.wasm
└── res/
    ├── layout/
    │   └── main.akml
    ├── layout-land/
    │   └── main.akml
    ├── values/
    │   ├── strings.toml
    │   ├── colors.toml
    │   └── dimens.toml
    ├── values-he/
    │   └── strings.toml
    ├── values-night/
    │   └── colors.toml
    └── drawable/
        └── icon.svg
```

### Qualifiers (תחילית במינימום ב-MVP)

- `-he`, `-en` — שפה (ISO 639-1)
- `-ldrtl`, `-ldltr` — כיוון טקסט
- `-land`, `-port` — orientation
- `-night` — dark mode
- `-sw600dp` — עתידי, screen width

### Resolution

ה-ResourceManager ב-boot זמן:
1. קורא את device config: `{ locale: "he-IL", orientation: "portrait", night: false }`
2. לכל resource reference (`@string/welcome`) מוצא את ה-match הכי טוב
3. Fallback: תמיד `values/` ללא qualifier

### יחידות

- `dp` (density-independent pixels): מוכפל ב-device density factor
- `sp` (scale-independent pixels): כמו dp + font size multiplier
- `px`: פיקסלים גולמיים (לא מומלץ)
- `%`: אחוז מה-parent

פורמט רישום: `"16dp"`, `"1.5sp"`, `"50%"`.

## 5. AKML — שפת התיאור הדקלרטיבית

AKML = **Akiva Markup Language**. XML-based. נפרד מהאפליקציה.

### דוגמה

```xml
<!-- res/layout/main.akml -->
<VStack
    id="@+id/root"
    padding="16dp"
    background="@color/surface"
    spacing="8dp">

    <Text
        id="@+id/display"
        text="@string/zero"
        textSize="48sp"
        textColor="@color/on_surface"
        textAlign="end"
        padding="12dp" />

    <Grid columns="4" rows="5" spacing="8dp" flex="1">
        <Button text="@string/clear" onClick="on_clear" style="@style/op_button" />
        <Button text="/" onClick="on_op" tag="div" style="@style/op_button" />
        <Button text="×" onClick="on_op" tag="mul" style="@style/op_button" />
        <!-- וכו' -->
    </Grid>
</VStack>
```

### מה ה-app מקבל

האפליקציה (wasm) בזמן `onCreate`:

```rust
fn on_create(&mut self, ctx: &ActivityContext) {
    ctx.set_content_view("@layout/main");
    // handlers מופעלים אוטומטית כי AKML הצהיר עליהם
}

fn on_clear(&mut self) {
    self.display = "0".to_string();
    self.invalidate("@+id/display");  // runtime יעדכן את ה-Text widget
}
```

ה-runtime מפרסר את ה-AKML, יוצר widgets, קושר callbacks — הכל דרך attribute binding.

### Inflation flow

```
1. App:      ctx.set_content_view("@layout/main")
2. Framework: ResourceManager.get_layout("main") → main.akml (אחרי qualifier matching)
3. Framework: LayoutInflater.inflate(akml_bytes)
4. Framework: מפרסר XML → יוצר widget tree פנימי
5. Framework: כל attribute מתורגם לקריאת runtime API (set_text, set_padding, וכו')
6. Framework: @string/@color/@dimen refs נפתרים דרך ResourceManager
7. Framework: onClick="on_clear" נרשם דרך callback registry של האפליקציה
8. LayoutEngine: measure pass → layout pass
9. Compositor: draw commands → canvas
```

## 6. Intent System

### מבנה Intent

```rust
pub struct Intent {
    pub action: String,           // "akiva.intent.action.VIEW"
    pub data_uri: Option<String>, // "content://..."
    pub mime_type: Option<String>,
    pub extras: HashMap<String, Value>,
    pub target: IntentTarget,     // Explicit | Implicit
}
```

### Intent filters (במניפסט)

```toml
[[activities]]
id = "main"
[[activities.intent_filters]]
actions = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]
```

### Resolution

1. App שולחת intent → `IntentRouter.dispatch(intent)`
2. Implicit: `PackageManager.query_intent(intent)` מחזיר רשימת matches
3. אם יותר מאחד — chooser UI
4. ActivityManager מריץ את ה-activity המתאים, דוחף ל-stack
5. Target activity מקבל את ה-intent ב-`onCreate` / `onNewIntent`

### דוגמה

```rust
// Calculator רוצה לפתוח Settings
let intent = Intent::new("akiva.intent.action.VIEW")
    .with_data("akiva://settings/theme");
ctx.start_activity(intent);
```

## 7. Window Manager

### תפקיד

מחליט **מה מוצג על המסך** בכל רגע.

### קומפוזיציה בסיסית (MVP)

- Activity אחד ברגע נתון (fullscreen)
- Transition animations בין activities
- Overlay layer לסיסטם UI (status bar, navigation bar)

### מורכבות עתידית

- Multi-window (split screen)
- Floating windows
- Picture-in-picture
- Drag & drop בין windows

### Shell

**Shell הוא אפליקציית Akiva רגילה** עם permission מיוחד:
- מופעלת אוטומטית ב-boot
- מטפלת ב-intent `akiva.intent.category.HOME`
- מציגה רשימת אפליקציות מותקנות דרך `PackageManager`
- מציגה task switcher (רשימת activities פתוחות)

**Back button**: WindowManager שולט בזה. לחיצה → ActivityManager.pop().

## 8. Permission Model (Capability-via-Imports)

**עיקרון:** אפליקציה יכולה לקרוא רק לפונקציות ש-ה-runtime ייבאה ל-wasm instance שלה. Permission denied = import חסר. אין runtime check. זה ה-model הטבעי של wasm/WASI ו-ה-Component Model.

### סוגי permissions

- `auto` — ניתנים תמיד (דוגמה: storage sandbox של ה-app עצמה, log, system config).
- `install-time` — משתמש מאשר בזמן התקנה; אחר כך מחוברים לכל lifetime של ה-app (דוגמה: network, cross-app storage).
- `runtime` — משתמש מאשר בזמן ריצה; ה-app מקבלת imports זמניים דרך re-link (MVP: דחוי — נתחיל עם install-time בלבד).
- `signature` — רק apps חתומות ע"י אותו publisher (post-MVP).

### אכיפה ב-instantiation time

```
PackageManager.install(app)
  │
  ▼
Validate manifest.permissions.required
  │
  ▼
User confirms (MVP: auto-approve כי אין app store)
  │
  ▼
Store granted capability set in registry
  │
  ▼
ActivityManager.launch(app)
  │
  ▼
WasmLoader builds import map per-capability:
  │
  ├─ "akiva.permission.STORAGE_SANDBOX" → include `storage` interface
  ├─ "akiva.permission.NETWORK"         → include `network` interface (post-MVP)
  ├─ "akiva.permission.QUERY_PACKAGES"  → include extra methods on `activity-api`
  │
  ▼
Instantiate component with computed imports
  │
  └─ App tries to call an import it doesn't have → link error at instantiate,
     NOT a runtime error. App can't even reach that code path.
```

### הצהרה במניפסט

```toml
[permissions]
required = ["akiva.permission.NETWORK"]
optional = ["akiva.permission.CAMERA"]
```

בזמן build, `akiva build` מפיק warning אם ה-code ייבא interfaces שלא מופיעים ב-permissions (אי אפשר בכלל לקמפל בהצלחה).

### יתרונות הגישה

| יתרון | איך |
|---|---|
| **מהיר** | אין branch על כל API call |
| **unforgeable** | אין שום דרך לאפליקציה להגיע ל-function שלא יובאה — confused deputy בלתי אפשרי |
| **auditable** | `akiva build` מדפיס את ה-capability set; קובץ WIT מתעד את ה-exact surface |
| **Component Model-native** | בדיוק איך ש-WASI עובד |

### Mapping מלא (MVP)

| Capability                          | Interfaces / methods מיובאים                        |
|-------------------------------------|-----------------------------------------------------|
| (אוטומטית)                          | `log`, `system.monotonic-nanos`, `system.wall-millis`, `system.current-config`, `system.random-bytes`, `widget.*`, `resources.*`, `activity-api`, `intent` |
| `akiva.permission.STORAGE_SANDBOX`  | `storage.*` (per-app sandbox, auto-granted)        |
| `akiva.permission.QUERY_PACKAGES`   | `activity-api.query-packages` (Shell only)          |
| `akiva.permission.LAUNCH_ANY_APP`   | `activity-api.start-activity` לכל package (Shell only) |
| `akiva.permission.NOTIFICATIONS`    | `notifications.*` (post-MVP)                        |
| `akiva.permission.NETWORK`          | `network.*` (post-MVP)                              |
| `akiva.permission.CAMERA`           | `camera.*` (post-MVP)                               |

## 9. Single-Instance Architecture (Tabs = Displays, not Devices)

**עיקרון:** יש **OS instance אחת** לכל משתמש ב-origin נתון. שני tabs של Akiva באותו דפדפן — שניהם רואים את אותה מערכת הפעלה, אותן אפליקציות רצות, אותו state. בדיוק כמו שאנדרואיד אחת רצה גם אם מחברים אליה שני מסכים חיצוניים.

**המודל הקונספטואלי:**
- **Akiva OS** רץ ב-**worker יחיד** שמוחזק ע"י ה-tab ה"מוביל" (leader) — מקום אחד, מרכזי, שמחזיק את כל ה-state
- כל **tab = client תצוגה + קלט** — לא מריץ OS, רק מציג canvas ומעביר events
- פתיחת tab חדש = "חיבור עוד מסך" לאותה Akiva
- סגירת ה-tab המוביל → tab אחר נבחר אוטומטית ומעביר את ה-worker אליו
- סגירת כל ה-tabs → ה-OS נכבה (state נשמר ב-OPFS)

### החלטה ארכיטקטונית: Web Locks Leader Election (Primary)

**הגישה שנבחרה:** שימוש ב-[Web Locks API](https://developer.mozilla.org/en-US/docs/Web/API/Web_Locks_API) לבחירת tab מוביל, והרצת ה-OS ב-**Dedicated Worker** שה-leader מחזיק.

**למה לא SharedWorker כ-primary:**
- Safari תמיכה בסיסית בלבד; כיסוי mobile דליל
- דפדפנים הורגים SharedWorker idle גם בזמן שיש tabs פתוחים
- OPFS `SyncAccessHandle` exclusive לקונטקסט יחיד — SharedWorker מתנגש עם tabs על access
- Dedicated Worker יציב יותר, נהרג רק עם ה-tab המחזיק

**מה Web Locks נותנים:**
- lock יחיד בשם `akiva.os.leader` — רק tab אחד מחזיק אותו בכל רגע
- `navigator.locks.request(name)` מוחזק על lifetime של ה-tab; ברגע שה-tab נסגר, הדפדפן משחרר אוטומטית
- tab אחר שממתין על ה-lock מקבל אותו מיד — transition transparent למשתמש
- עובד בכל הדפדפנים (Chrome/Firefox/Safari/Edge) מ-2022

### מימוש טכני

```
┌────────────────┐  ┌────────────────┐  ┌────────────────┐
│   Leader Tab   │  │   Follower Tab │  │   Follower Tab │
│  HAL + Canvas  │  │  HAL + Canvas  │  │  HAL + Canvas  │
│  (display #1)  │  │  (display #2)  │  │  (display #3)  │
│                │  │                │  │                │
│  holds         │  │                │  │                │
│  akiva.os.     │  │                │  │                │
│  leader lock   │  │                │  │                │
│        │       │  │                │  │                │
│        ▼       │  │                │  │                │
│  ┌──────────┐  │  │                │  │                │
│  │ Dedicated│  │  │                │  │                │
│  │  Worker  │  │  │                │  │                │
│  │          │  │  │                │  │                │
│  │ Akiva    │◄─┼──┼── BroadcastChannel + MessageChannel│
│  │ Runtime  │  │  │   ("akiva.os")                      │
│  │  (wasm)  │  │  │                │  │                │
│  └──────────┘  │  │                │  │                │
└────────────────┘  └────────────────┘  └────────────────┘
        │
        ▼
┌───────────────────────────────────────────────────────────┐
│  OPFS (Origin Private File System) — persistent state     │
│  Accessed via SyncAccessHandle from the Worker only        │
└───────────────────────────────────────────────────────────┘
```

**זרימת bootstrap של tab (tab.js בכל tab):**

```js
// pseudo-code
async function boot() {
  const channel = new BroadcastChannel("akiva.os");

  // First, announce presence and ask: is there a leader?
  const leaderPort = await probeForLeader(channel, { timeoutMs: 150 });
  if (leaderPort) {
    connectAsFollower(leaderPort);
    return;
  }

  // No leader answered — race for the lock.
  navigator.locks.request("akiva.os.leader", async (lock) => {
    // We won. Start the Worker and serve others until we're closed.
    const worker = new Worker("worker.js", { type: "module" });
    registerAsLeader(channel, worker);
    connectAsFollower(worker.port);         // the leader tab is also display #1
    await new Promise(() => {});            // hold lock until tab closes
  });
}
```

**הסבר:**
- Tab חדש מפרסם `"who-is-leader?"` ב-BroadcastChannel. אם מישהו עונה תוך 150ms — נעשים follower.
- אחרת מתחרים על ה-lock. ה-winner מרים Worker, מרים את ה-OS, וכשה-BroadcastChannel קיבל `"who-is-leader?"` הוא פותח MessagePort חדש ושולח אותו לשואל.
- הכל זרוק דרך BroadcastChannel כדי לתאם. הטרנספורט בפועל של draw commands / events הוא MessagePort per-tab (point-to-point, מהיר יותר).

### חלוקת האחריות

**HAL בכל tab** (`tab.js`, lightweight, ~200 שורות):
- יוצר canvas + OffscreenCanvas transfer ל-Worker (ראה סעיף 10)
- פותח BroadcastChannel + MessagePort
- ממיר pointer/keyboard events ל-messages
- **אין לוגיקה של OS ב-tab**

**Dedicated Worker ב-Leader** (`worker.js`, ~300 שורות):
- מחזיק את instance ה-wasm של הרנטיים
- מריץ את כל האפליקציות
- מחזיק OPFS `SyncAccessHandle` ל-storage sandbox
- מנהל רשימת displays — לכל follower MessagePort משלו
- שולח draw commands ל-display שבו ה-activity "חי"
- מקבל input events מ-displays

**Service Worker** (אופציונלי, post-MVP):
- Cache ל-wasm של runtime + apps מותקנות
- Offline boot
- **לא** מריץ OS (lifetime קצר מדי)

### Leader Failover

כשה-leader נסגר:
1. הדפדפן משחרר אוטומטית את `akiva.os.leader`.
2. ה-follower הבא שקרא `navigator.locks.request` מקבל אותו.
3. ה-winner מרים Worker חדש. ה-wasm runtime עולה, קורא state מ-OPFS (ActivityManager task stack + saved states), ממשיך משם.
4. Tabs אחרים (followers) פותחים MessagePort חדש ל-leader החדש.
5. המשתמש רואה flash קצר (תלוי כמה זמן לוקח לטעון את ה-wasm — בפועל ~100-300ms מ-Service Worker cache).

**תוצאה:** single OS instance logical שמחזיקה ברציפות את ה-state ללא תלות באיזה tab "לוקח את ההגה".

### Multi-tab scenarios

**משתמש פותח tab ראשון:**
1. Tab מתחרה על lock — מנצח (אין אחרים).
2. מרים Worker, ה-runtime מבצע boot.
3. Tab נרשם כ-display #1 (בתוך ה-Worker).
4. Shell activity מופיעה.

**משתמש פותח tab שני:**
1. Tab מפרסם `"who-is-leader?"` → tab הראשון עונה עם MessagePort.
2. Tab מתחבר כ-display #2.
3. ActivityManager מקצה מה להציג ב-display #2 (MVP: Shell).

**משתמש סוגר את ה-tab המוביל אבל יש tabs אחרים:**
1. Leader נסגר → lock משוחרר → follower מקבל.
2. Worker חדש עולה אצל ה-follower החדש.
3. State נטען מ-OPFS.
4. Tabs אחרים מתחברים ל-leader החדש.

**משתמש סוגר את כל ה-tabs:**
- כל ה-Workers נהרגים.
- State השתמר ב-OPFS דרך `on_save_state` של activities ו-ActivityManager persistence.
- פתיחת tab חדש יעלה הכל שוב.

### מגבלות מוכרות

- **Cross-origin:** לא משתף. akiva.app עם akiva.app עובד; לא עם domain שונה.
- **Incognito:** אין persistence (מקובל, צפוי).
- **iOS Safari:** יש תמיכה ב-Web Locks ו-BroadcastChannel מ-16.4+. לגרסאות ישנות יותר — לא נתמכות (נציג הודעת "Akiva requires a modern browser").
- **Tab freeze (mobile):** דפדפני mobile עלולים להקפיא tab ברקע. ה-leader freeze לא משפיע על ה-Worker באופן מיידי, אבל ה-throttle יכול להוריד ל-60s tick. ב-mobile נתעדף tab פעיל כ-leader אם אפשר.

### Fallback לגרסאות ישנות

אם ה-Web Locks API לא זמין (שלא יקרה בדפדפנים נתמכים):
- Fallback ל-`localStorage` leader election (mutex ע"י timestamp + random nonce). פחות אמין, אבל עובד. תיעוד בלבד — לא נממש ב-MVP.

**החלטה ל-MVP:** Web Locks + Dedicated Worker הם ה-primary. SharedWorker לא נמצא ב-MVP — אולי נחזיר אותו בעתיד כ-optimization path אם נזהה תרחיש שמצדיק.

## 10. Renderer Backend (Abstract)

**עיקרון:** ה-Compositor מפיק שכבת draw commands מופשטת. מאחוריה יושב `RendererBackend` — trait שלו יש implementations שונות. בחירת backend היא החלטה של ה-host בזמן boot, לא קשורה לקוד ה-framework.

**למה abstract מההתחלה:** Canvas 2D נפגע בביצועים ב-~2500 אובייקטים. ברגע שמתחילים לצייר shell עם grid של apps + toolbars + overlays + dialogs, יוצאים מזה. אנחנו לא רוצים לגלות את זה באפליקציה החמישית.

### Trait

```rust
pub trait RendererBackend: Send {
    /// Initialize with an OffscreenCanvas handle + device metrics.
    fn init(&mut self, canvas: RenderSurface, metrics: SurfaceMetrics) -> Result<()>;

    /// Apply one frame's draw commands. Commands reference glyph/image handles
    /// that the backend caches internally.
    fn submit(&mut self, frame: &FrameCommands) -> Result<()>;

    /// Resize the target surface.
    fn resize(&mut self, width_px: u32, height_px: u32);

    /// Upload / evict image + glyph caches (used by ResourceManager + text pipeline).
    fn upload_image(&mut self, id: ImageId, rgba: &[u8], w: u32, h: u32);
    fn evict_image(&mut self, id: ImageId);

    fn destroy(self);
}

pub enum FrameCommand {
    FillRect  { rect: Rect, rgba: u32 },
    StrokeRect{ rect: Rect, rgba: u32, width_px: f32 },
    FillRoundedRect { rect: Rect, rgba: u32, radius_px: f32 },
    FillText  { origin: Point, text: GlyphRun, rgba: u32 },
    DrawImage { dst: Rect, image: ImageId, src: Option<Rect> },
    PushClip  { rect: Rect },
    PopClip,
    PushOpacity { alpha: f32 },
    PopOpacity,
}
```

### Implementations

| Backend          | When used                                  | Notes                                         |
|------------------|--------------------------------------------|-----------------------------------------------|
| `Canvas2DBackend`| MVP default; compatibility floor           | OffscreenCanvas.getContext("2d") from Worker  |
| `WebGPUBackend`  | Primary post-MVP for perf                  | requires `navigator.gpu` — detect at boot     |
| `WebGL2Backend`  | Optional fallback for older browsers       | only if someone ships without WebGPU          |

**בחירה ב-boot (ב-Worker):**
```rust
let backend: Box<dyn RendererBackend> = if gpu_available() {
    Box::new(WebGPUBackend::new())
} else {
    Box::new(Canvas2DBackend::new())
};
```

**מה ב-MVP:** רק `Canvas2DBackend`. אבל ה-trait קיים וה-Compositor כותב מולו — החלפת הבקאנד היא תוספת implementation בלבד.

### OffscreenCanvas mandatory

כל הרינדור קורה ב-**Worker** דרך `OffscreenCanvas` שה-tab העביר עם `transferControlToOffscreen`. ה-main thread של ה-tab מציג רק, לא מצייר. זה מבטל risk של UI jank כשה-OS עסוק.

---

## 11. Graphics Primitives (coordinate system + color + density)

בלי שהסעיף הזה קבוע, כל widget יכתב ספק ומימוש שונה. אלו ה-ground rules:

### Coordinate system
- **Origin**: top-left של כל surface (canvas, widget, frame).
- **Axes**: X הולך ימינה, Y הולך **למטה**. זהה לכל ה-browser APIs.
- **יחידות פנים**: ה-Compositor מקבל dp מ-ה-Layout, ממיר ל-physical pixels (`px = dp * density`) לפני submit ל-backend.
- **Sub-pixel**: קואורדינטות הן `f32`. ה-backend אחראי לעיגול אם צריך.

### Density
- **Baseline**: 160 dpi = density `1.0` (mdpi).
- **Density buckets** (לצורך בחירת drawable variant):
  - `mdpi`   — density ≤ 1.25  (≤200 dpi)
  - `hdpi`   — 1.25 < density ≤ 1.75  (≤280 dpi)
  - `xhdpi`  — 1.75 < density ≤ 2.5   (≤400 dpi)
  - `xxhdpi` — 2.5  < density ≤ 3.5   (≤560 dpi)
  - `xxxhdpi`— density > 3.5          (>560 dpi)
- **מקור**: `window.devicePixelRatio` בתוך ה-tab, משודר ל-Worker ב-display registration.

### Color space
- **MVP**: sRGB בלבד. כל ה-API של צבעים (`0xRRGGBBAA`, TOML `#RRGGBB[AA]`) ב-sRGB.
- **Gamut assumption**: אין תמיכה ב-display-P3 או HDR ב-v0.1. נוסיף ב-v0.3+ דרך color profile ב-manifest.
- **Blending**: "source-over" default. אלפא pre-multiplied בתוך ה-renderer (אבל ה-API מציג straight alpha).

### Font metrics
- **Baseline font stack (MVP)**: system-ui, sans-serif.
- **sp → px**: `px = sp * density * font_scale` כאשר `font_scale` = 1.0 ב-MVP (עתיד: user preference).
- **Text rendering**: Canvas `fillText` ב-MVP (latin + basic unicode). Proper shaping (HarfBuzz) + BiDi post-MVP.

### Clocks
- `system.monotonic-nanos`: `performance.now() * 1_000_000` בתוך ה-Worker.
- `system.wall-millis`: `Date.now()`.

### RNG
- `system.random-bytes(n)` מנותב ל-`crypto.getRandomValues` של ה-Worker. תמיד secure, ללא אפשרות fallback.

---

## 12. Crash Recovery

### ברמת App

- App crash → ActivityManager מציין state = CRASHED
- Task stack שומר snapshot של saved state של ה-activity
- משתמש יכול לבחור relaunch או kill
- Relaunch: instance חדש נוצר, onCreate נקרא עם savedInstanceState

### ברמת Runtime

- Runtime crash → browser page reload
- StorageManager שומר ב-OPFS, מחזיק state גם אחרי reload
- ActivityManager משחזר task stack (רק ה-top activity reloads)

## 13. זרימת boot מלאה

```
1. Browser טוען index.html (בתוך tab)
2. index.html טוען akiva-tab.js (Tab HAL)
3. Tab HAL יוצר <canvas>, רושם event listeners
4. Tab HAL פותח/מתחבר ל-Shared Worker (akiva-worker.js)
5. אם Worker חדש: Worker HAL טוען akiva-runtime.wasm
6. Worker HAL: WebAssembly.instantiate(runtime, imports)
7. Tab נרשם אצל Worker כ-display #N
8. Runtime.boot() (פעם אחת, ב-Worker בלבד):
   a. MemoryManager.init()
   b. StorageManager.init(OPFS)
   c. PackageManager.scan_installed()   → מוצא אפליקציות
   d. PermissionManager.load_grants()
   e. ActivityManager.init()
   f. ResourceManager.load_device_config()
   g. IntentRouter.init()
   h. DisplayManager.init()  ← ניהול tabs כ-displays
9. Framework שולחת intent: action=MAIN, category=HOME
10. PackageManager מחזיר: com.akiva.shell
11. ActivityManager.launch("com.akiva.shell/.HomeActivity", display=1)
    a. WasmLoader.load(shell.wasm) → new instance
    b. קורא akiva_on_create
    c. Shell קורא ctx.set_content_view("@layout/home")
    d. LayoutInflater מפרסר home.akml
    e. LayoutEngine + Compositor → draw commands
    f. Compositor שולח draw commands ל-display #1 דרך MessagePort
    g. Tab HAL מצייר על canvas
12. משתמש לוחץ על icon של calculator
    a. Tab HAL מעביר pointer event ל-Worker
    b. EventQueue מנתב ל-Shell
13. Shell שולח intent: action=MAIN, target=com.akiva.calculator
14. ActivityManager.launch(calculator) — push ל-stack
15. WindowManager: transition animation, draw commands → אותו display
```

## 14. מה שומרים מ-AOSP כ-inspiration

| AOSP | Akiva מחקה את הרעיון של |
|---|---|
| `ActivityManagerService` | ActivityManager |
| `PackageManagerService` | PackageManager |
| `WindowManagerService` | WindowManager |
| `Resources` / `AssetManager` | ResourceManager |
| `LayoutInflater` | LayoutInflater |
| `Intent` + `IntentFilter` | Intent + IntentRouter |
| XML layouts | AKML |
| XML themes/styles | AKML styles |
| `android:id="@+id/..."` | `id="@+id/..."` (זהה) |
| dp/sp units | dp/sp (זהה) |
| `onCreate` / `onPause` lifecycle | זהה |
| `onSaveInstanceState` | `onSaveState` |

**אנחנו לא מעתיקים קוד. אנחנו לוקחים רעיונות שעברו 20 שנה של הוכחה בפרודקשן ומיישמים אותם ב-Rust עם התאמות ל-wasm/browser.**

## 15. מה **לא** לוקחים מ-AOSP

- Java/Kotlin (אנחנו Rust)
- Binder IPC (יש לנו ipc in-process דרך runtime)
- ART (VM — אנחנו wasm)
- SELinux (אנחנו wasm sandbox)
- Hardware services (ה-browser מטפל)
- SystemServer כ-process נפרד (אצלנו framework ב-runtime instance)

## 16. Introspection Surface

**עיקרון:** כשה-runtime לא DOM, כלי ה-debug הרגילים של הדפדפן (Elements, Inspect) מאבדים גישה למבנה ה-UI. המערכת חייבת לספק **שכבת introspection נפרדת** כ-first-class concern, לא כ-afterthought — אחרת פיתוח non-DOM הופך מהר ל"עיוור".

### הבחנה בין שתי שכבות

- **Runtime UI** — ה-renderer האמיתי (Canvas2D/WebGPU; §10), widget tree, frame commands. זה מה שהמשתמש רואה.
- **Introspection** — overlays שמציגים מצב פנימי: widget-tree inspector, event trace, activity-stack view, resource-resolution log, frame timing. נפרד מה-renderer ומנותק מה-widget tree של האפליקציה.

### עקרונות מימוש

- יושב ב-**framework**, לא ב-kernel. קורא מצב דרך APIs פנימיים קיימים של ActivityManager / ResourceManager / Compositor — בלי להרחיב את ה-ABI של האפליקציה.
- מופעל רק במצב dev (flag ברמת ה-session, לא per-app).
- רץ כ-overlay נפרד על גבי ה-`FrameCommands` של ה-Compositor, לא מתערבב בתוכו.
- חשיפה אל מחוץ ל-worker דרך `postMessage` אל ה-tab, כדי שבעתיד תיכתב DevTools extension שתציג את ה-trace באופן אינטראקטיבי.

### MVP vs עתיד

ב-MVP אנחנו **לא** מממשים introspection. הסעיף הזה עוגן ארכיטקטוני שמונע מצב שבו נוסיף debug APIs ad-hoc שמזהמים את ה-ABI. המימוש בפועל יבוא כשה-shell יתחיל להכיל יותר מ-"kernel: boot" — כנראה סביב Phase 3 (window manager + task switcher), כשה-widget tree יהיה מורכב מספיק כדי להצדיק inspector.
