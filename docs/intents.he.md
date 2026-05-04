# Intent System & Package Manager

> **סטטוס מימוש (אפריל 2026):** IntentRouter wired, filter registry פעיל, resolve של explicit + implicit intents עובד. Phase 8 סגור — `.akv` archive + `akiva package`/`install`/`uninstall` פעילים, ה-worker מגלה dev-apps דרך dynamic endpoint `/dev-apps/index.json` (אין יותר hardcoded `DEV_APPS`). **Stone 9.4 נסגר:** `ctx.start_activity(&intent)` מתוך wasm app + `on_new_intent` על SingleTop reuse — פעיל end-to-end (`akiva_host_start_activity` + `hal_app_drain_intent` + `akiva_app_new_intent`, עם wire codec ב-`Intent::encode`/`decode`). חסרים: `start_activity_for_result` (Stone 9.5), chooser UI, SingleTask/SingleInstance reuse path (Stone 9.2 — יקבל את ה-on_new_intent plumbing בחינם כשהוא ננחת). PendingIntent + Broadcast receivers (§13-14) — v0.3+.

## 1. מטרה

Intent = "I intend to do X" — מסר מובנה שאפליקציה שולחת למערכת. המערכת מנתבת אותו לאפליקציה שמכריזה שהיא יודעת לטפל ב-X. זה המנגנון המרכזי של IPC באנדרואיד, ואצלנו.

דוגמאות של intents:
- "Show me settings"
- "View this URL"
- "Edit this image"
- "Share this text"

## 2. Intent Types

### Explicit Intent

מכוון ישירות ל-activity ספציפית:

```rust
let intent = Intent::explicit("com.akiva.calculator", "SettingsActivity");
ctx.start_activity(intent);
```

**שימוש:** תקשורת פנימית בתוך אפליקציה, או כשהיעד ידוע בדיוק.

### Implicit Intent

מתאר פעולה רצויה, המערכת מוצאת את ה-activity המתאימה:

```rust
let intent = Intent::new("akiva.intent.action.VIEW")
    .with_data("https://example.com")
    .with_mime_type("text/html");
ctx.start_activity(intent);
```

**שימוש:** פעולות cross-app — פתיחת קישור, שיתוף, וכו'. ה-user עשוי לראות chooser.

## 3. Intent Structure

```rust
pub struct Intent {
    pub action: String,                      // "akiva.intent.action.VIEW"
    pub data_uri: Option<Uri>,               // "content://..." / "https://..."
    pub mime_type: Option<String>,           // "text/plain", "image/*"
    pub categories: Vec<String>,             // ["akiva.intent.category.DEFAULT"]
    pub target: IntentTarget,                // Explicit | Implicit
    pub extras: HashMap<String, IntentValue>,// serializable extras
    pub flags: IntentFlags,                  // NEW_TASK, CLEAR_TOP, וכו'
}

pub enum IntentTarget {
    Explicit { package_id: String, activity_id: String },
    Implicit,
}

pub enum IntentValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Bytes(Vec<u8>),
}
```

## 4. Standard Actions

Namespace: `akiva.intent.action.*`

| Action | משמעות | Data expected |
|---|---|---|
| `MAIN` | launch point של אפליקציה | — |
| `VIEW` | הצגת משהו | uri |
| `EDIT` | עריכה | uri |
| `PICK` | בחירת פריט | — |
| `SEND` | שליחה/שיתוף | extras: TEXT/STREAM |
| `SHARE` | שיתוף (synonym של SEND) | — |
| `SEARCH` | חיפוש | extras: QUERY |
| `DIAL` | התקשרות | uri: `tel:...` |
| `SETTINGS` | פתיחת settings של app | — |

אפליקציות יכולות להגדיר custom actions, אבל עדיף namespace ייחודי: `com.mycompany.intent.action.FOO`.

## 5. Standard Categories

| Category | משמעות |
|---|---|
| `akiva.intent.category.DEFAULT` | ברירת מחדל (כל activity שלא מכריזה category אחרת מקבלת את זה) |
| `akiva.intent.category.LAUNCHER` | נכנסת ב-Shell |
| `akiva.intent.category.HOME` | ה-Shell עצמה |
| `akiva.intent.category.BROWSABLE` | יכולה להיפתח דרך URL |
| `akiva.intent.category.PREFERENCE` | preference pane |

## 6. Intent Filters (במניפסט)

Activity מצהירה אילו intents היא יודעת לטפל:

```toml
[[activities]]
id = "main"
entry = true

[[activities.intent_filters]]
actions = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]

[[activities]]
id = "viewer"

[[activities.intent_filters]]
actions = ["akiva.intent.action.VIEW"]
categories = ["akiva.intent.category.DEFAULT", "akiva.intent.category.BROWSABLE"]

[[activities.intent_filters.data]]
scheme = "content"
host = "my.content.provider"

[[activities.intent_filters.data]]
scheme = "https"
mime_types = ["image/*"]
```

## 7. Resolution Algorithm

כשיש implicit intent:

```
IntentRouter.dispatch(intent)
  │
  ▼
PackageManager.query_intent(intent)
  │
  ├─ Iterate all installed apps
  ├─ For each app, iterate intent filters
  ├─ Match if:
  │    ✓ action matches (exact)
  │    ✓ ALL categories in intent exist in filter (subset)
  │    ✓ data (scheme, host, mime) matches
  │
  ▼
Return Vec<ResolveInfo>
  │
  ▼
If empty:
  → Return error to caller
If one:
  → ActivityManager.launch(explicit-intent)
If many:
  → WindowManager.show_chooser(matches)
  → User picks
  → ActivityManager.launch(chosen)
```

### Chooser UI

ב-MVP: פשוט דיאלוג עם רשימת apps. משתמש יכול לבחור "just once" או "always" (זה אחרון ישמר ב-preference).

## 8. Intent Delivery

### Start Activity

ה-Intent מועבר ל-activity דרך `onCreate` (אם חדש) או `onNewIntent` (אם `singleTop` / `singleTask`):

```rust
impl App for MyApp {
    fn on_create(&mut self, ctx: &ActivityContext) {
        let intent = ctx.intent();
        if let Some(uri) = intent.data_uri() {
            self.load(uri);
        }
    }

    fn on_new_intent(&mut self, ctx: &ActivityContext) {
        // נקרא רק אם singleTop/singleTask וה-activity כבר קיימת
        let intent = ctx.intent();
        // ...
    }
}
```

### Start For Result

אפליקציה A רוצה תוצאה מאפליקציה B:

```rust
// App A
let intent = Intent::new("akiva.intent.action.PICK")
    .with_mime_type("image/*");
ctx.start_activity_for_result(intent, REQUEST_PICK_IMAGE);

// ... later ...
impl App for MyApp {
    fn on_activity_result(&mut self, request_code: u32, result: ActivityResult) {
        if request_code == REQUEST_PICK_IMAGE {
            if let Some(uri) = result.data_uri() {
                self.show_image(uri);
            }
        }
    }
}

// App B (picker)
ctx.set_result(ResultData::new().with_data_uri(selected_uri));
ctx.finish();
```

## 9. PackageManager

### אחריות

- **Installed registry**: מחזיק database של apps מותקנות (נטען ב-boot מ-OPFS)
- **Install/Uninstall/Upgrade**: מגלם את ה-flow של `.akv` archives ב-storage
- **Resource tables**: קריאת `res/values/*` לכל package והעברתם ל-ResourceManager
- **Saved-state persistence**: מפתחות `sys/packages/{id}/{class}/saved_state`
- **App info**: name, version, version_code, activities, icon, launch modes

### Public API (actual, Phase 8)

המימוש ב-[`framework/src/package_manager.rs`](../framework/src/package_manager.rs) חושף API סינכרוני. Intent resolution הוא responsibility של `IntentRouter` נפרד (לא מוטמע ב-PackageManager כמו ב-Android).

```rust
pub struct PackageRecord {
    pub id: String,
    pub name: String,
    pub version: String,
    pub version_code: u32,
    pub entry_class: String,
    pub manifest: Manifest,
}

impl PackageManager {
    pub fn new() -> Self;
    pub fn load_installed(&mut self, storage: &mut dyn Storage, on_error: impl FnMut(&str, LoadError)) -> usize;
    pub fn install(&mut self, storage: &mut dyn Storage, archive_bytes: &[u8]) -> Result<Report, InstallError>;
    pub fn uninstall(&mut self, storage: &mut dyn Storage, id: &str) -> bool;
    pub fn get(&self, id: &str) -> Option<&PackageRecord>;
    pub fn list(&self) -> impl Iterator<Item = &PackageRecord>;
    pub fn count(&self) -> usize;
    pub fn read_wasm(&self, storage: &dyn Storage, id: &str) -> Result<Vec<u8>, StorageError>;
    pub fn read_resource_tables(&self, storage: &mut dyn Storage, on_error: impl FnMut(&str, ResourceLoadError)) -> Vec<(String, ResourceTable)>;
}

// Intent resolution lives in a sibling subsystem:
impl IntentRouter {
    pub fn register_filter(&mut self, filter: IntentFilter);
    pub fn clear_package(&mut self, package_id: &str);
    pub fn resolve(&self, intent: &Intent) -> Result<(String, String), ResolveError>;
}
```

אין `ResolveInfo` / `PackageInfo` — ה-API האמיתי מחזיר `(String, String)` tuples ו-`&PackageRecord` references. ה-shape של §9 בגרסאות קודמות של המסמך היה aspirational; המימוש בחר דרך אחרת (split בין `PackageManager` ל-`IntentRouter`) כדי לשמור אחריות נקייה.

### Implementation (Phase 3 + Phase 8)

| יכולת | סטטוס | הערה |
|---|---|---|
| `Intent` struct + Explicit/Implicit targets | ✅ | [`framework/src/intent.rs`](../framework/src/intent.rs) |
| Standard action/category constants | ✅ | `intent::action::*`, `intent::category::*` |
| Intent filter registry + resolve (first-match) | ✅ | `IntentRouter` — `NoMatch` / `AmbiguousMatch` ב-error enum |
| Intent wire codec (framework ⇄ sdk) | ✅ Stone 9.4 | `Intent::encode` / `Intent::decode` משני הצדדים — byte-for-byte symmetric, 16 KiB cap, `extras_count=0` reserved ל-9.5 |
| `ctx.start_activity(&intent)` מתוך wasm app | ✅ Stone 9.4 | `akiva_host_start_activity` → worker `_pendingIntent` → kernel `hal_app_drain_intent` → framework `ctx.launch_requested` → `IntentRouter::resolve` |
| `on_new_intent` על SingleTop reuse | ✅ Stone 9.4 | `Framework::launch_app` מזהה reuse (top id matches), קורא ל-`deliver_new_intent` במקום bring-up; SDK export `akiva_app_new_intent`; framework test `singletop_reuse_fires_on_new_intent_instead_of_bring_up` |
| `on_new_intent` על SingleTask/SingleInstance reuse | ⏳ Stone 9.2 | ה-plumbing של on_new_intent מוכן; חסר מימוש reuse ב-`ActivityManager::launch` למודים האלה |
| Data/mime matching (scheme/host/mime) | ⏳ Stone 9.5+ | `IntentFilter.data_schemes`/`mime_types` קיימים כ-fields אבל `resolve` לא משתמש בהם |
| `start_activity_for_result` + `on_activity_result` | ❌ Phase 9.5 | |
| Chooser UI (אם `resolve` מחזיר Ambiguous) | ❌ Phase 9.4 | היום AmbiguousMatch הוא error שמושתק ב-`apply_post` |
| `PackageManager::install` / `uninstall` (archive-based) | ✅ Phase 8 | כולל same-version/downgrade rejection + major-version saved-state wipe |
| `PackageManager::load_installed` מ-OPFS ב-boot | ✅ | per-package errors מדווחים ל-`BootReport` |
| Resource table loading per package | ✅ | `read_resource_tables` → `ResourceManager::register` |
| PendingIntent (§13) | ❌ v0.3+ | |
| Broadcast receivers (§14) | ❌ v0.3+ | |

## 10. דוגמאות שימוש

### דוגמה 1: Shell פותחת Calculator

```rust
// in Shell's on_app_click handler:
let intent = Intent::new("akiva.intent.action.MAIN")
    .with_category("akiva.intent.category.LAUNCHER")
    .with_package("com.akiva.calculator");
ctx.start_activity(intent);
```

### דוגמה 2: אפליקציה פותחת URL

```rust
let intent = Intent::new("akiva.intent.action.VIEW")
    .with_data("https://example.com");
ctx.start_activity(intent);
// PackageManager יבחר את browser המותקן (אם קיים).
```

### דוגמה 3: שיתוף טקסט

```rust
let intent = Intent::new("akiva.intent.action.SEND")
    .with_mime_type("text/plain")
    .with_extra("TEXT", IntentValue::Str("Hello!".into()));
ctx.start_activity(Intent::create_chooser(intent, "Share with..."));
```

### דוגמה 4: בקשת תמונה

```rust
let intent = Intent::new("akiva.intent.action.PICK")
    .with_mime_type("image/*");
ctx.start_activity_for_result(intent, 1001);
```

## 11. Security

- כל intent filter חייב להיות exported=true כדי שאפליקציות אחרות יוכלו לשלוח.
- intent עם `FLAG_GRANT_READ_URI_PERMISSION` נותן גישה ל-URI למוען בלבד, זמני.
- PackageManager מתעד כל intent dispatch ב-log (לצורך debugging ו-audit).

## 12. מגבלות MVP

- אין `PendingIntent` (intent דחוי — ראה §13)
- אין Broadcast receivers (ראה §14)
- אין Content Provider model מלא — רק `content://` scheme ברמה סמלית
- אין deep URI matching מורכב (רק scheme + host + mime)
- Chooser UI פשוט מאוד (dialog רגיל, בלי "הגדר כברירת מחדל")

## 13. PendingIntent (post-MVP, v0.2)

**הבעיה שהוא פותר:** אפליקציה רוצה להגדיר intent שיופעל *אחר כך* על ידי המערכת — למשל, notification tap → פתיחת activity ספציפית, או alarm → הפעלת service.

**המבנה:**

```rust
pub struct PendingIntent {
    pub id: PendingIntentId,
    pub creator: PackageId,
    pub target: Intent,
    pub flags: PendingIntentFlags,  // OneShot | UpdateCurrent | ...
}
```

**תרחיש:**

```rust
// App creates a pending intent
let trigger = Intent::new("com.akiva.calendar.action.SHOW_EVENT")
    .with_extra("event_id", IntentValue::Str(event.id.clone()));
let pending = ctx.package_manager().create_pending_intent(trigger, PendingFlags::OneShot);

// App attaches it to a notification (post-MVP)
notifications.notify("reminder", NotificationSpec {
    title: "Meeting in 10 min",
    tap_intent: Some(pending),
    ..Default::default()
});

// Later, user taps notification → framework dispatches the pending intent
// → calendar.ShowEventActivity opens with event_id in extras.
```

**API (draft):**

```rust
trait PackageManager {
    fn create_pending_intent(&mut self, intent: Intent, flags: PendingFlags) -> PendingIntent;
    fn dispatch_pending_intent(&mut self, id: PendingIntentId) -> Result<(), IntentError>;
    fn cancel_pending_intent(&mut self, id: PendingIntentId);
}
```

**אבטחה:** PendingIntent שנוצר ע"י app A יכול להיות dispatch ע"י app B (זה כל הפוינט של notifications עם actions). אבל ה-target activity רואה את ה-intent כאילו app A שלח אותו, לא B. זה manage ע"י הרצה בתוך ה-process של ה-creator, עם "impersonation" של ה-caller רק לצורכי identity check.

## 14. Broadcast Intents (post-MVP, v0.2)

**הבעיה שהוא פותר:** מספר apps רוצים לדעת כש-event system קרה — locale change, network state change, screen-on, low-battery.

**המבנה:**
- Sender: אפליקציה או framework שולחים `broadcast_intent(intent)`.
- Receivers: apps מצהירים ב-manifest ש-activity או "broadcast receiver" מוכן לקבל.
- Framework fan-out: לכל receiver — instance נוצר (אם לא קיים) ונמסר ה-intent.

**סוגי broadcasts:**

| סוג | מקור | דוגמאות |
|---|---|---|
| System | Framework בלבד | `BOOT_COMPLETED`, `LOCALE_CHANGED`, `SCREEN_ON`, `BATTERY_LOW` |
| Ordered | Apps יכולים | fan-out עם priority, receiver אחד יכול לבטל את השאר |
| Sticky | Framework | "latest value" נשמר, receiver חדש מקבל את האחרון (discouraged) |

**Receiver declaration:**

```toml
[[receivers]]
class = "BootReceiver"
[[receivers.intent_filters]]
actions = ["akiva.intent.action.BOOT_COMPLETED"]
```

**Standard system broadcasts (MVP stubs):**

גם אם אין receiver framework, ה-framework כבר **שולחת** את ה-intents האלה פנימית לצורכי config change:
- `LOCALE_CHANGED` — שפה
- `ORIENTATION_CHANGED`
- `UI_MODE_CHANGED` — light/dark
- `DENSITY_CHANGED`
- `DISPLAY_ADDED` / `DISPLAY_REMOVED` (tab פתח/נסגר)

ל-MVP: activities מקבלות דרך `on_config_change` callback, לא דרך intents. ב-v0.2: יחזרו כ-broadcasts ש-apps אחרים יכולים לקבל.

**Security:** broadcast עם `exported = false` מגיע רק לאפליקציה ששלחה אותו. Framework broadcasts נפתחים לכולם כברירת מחדל, חוץ מאלה הדורשים permission ספציפית.
