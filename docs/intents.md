# Intent System & Package Manager

> **Implementation status (May 2026):** the IntentRouter is wired,
> the filter registry is live, and `resolve` for explicit and
> implicit intents works. Phase 8 is closed — `.akv` archives +
> `akiva package` / `install` / `uninstall` are live; the worker
> discovers dev apps through the `/dev-apps/index.json` endpoint
> (no hardcoded `DEV_APPS`). **Stones 9.4 + 9.5 are closed:**
> `ctx.start_activity(&intent)` from a wasm app + `on_new_intent`
> on `SingleTop` / `SingleTask` / `SingleInstance` reuse +
> `start_activity_for_result` / `set_result` /
> `on_activity_result` are all live end-to-end. Still open:
> chooser UI, data/mime matching beyond exact action+category,
> PendingIntent (§13), Broadcast receivers (§14) — those are
> v0.3+.

## 1. Purpose

An Intent is "I intend to do X" — a structured message an app
sends to the system. The system routes it to the app that
declared it knows how to handle X. This is the central IPC
mechanism on Android, and here.

Examples:

- "Show me settings."
- "View this URL."
- "Edit this image."
- "Share this text."

## 2. Intent types

### Explicit intent

Targets a specific activity directly:

```rust
let intent = Intent::explicit("com.akiva.calculator", "SettingsActivity");
ctx.start_activity(&intent).await;
```

**Use case:** internal communication inside an app, or when the
target is exactly known.

### Implicit intent

Describes a desired action; the system picks an appropriate
activity:

```rust
let intent = Intent::new("akiva.intent.action.VIEW")
    .with_data("https://example.com")
    .with_mime_type("text/html");
ctx.start_activity(&intent).await;
```

**Use case:** cross-app actions — opening a link, sharing,
sending. The user may see a chooser when multiple apps match
(chooser UI not yet implemented).

## 3. Intent structure

```rust
pub struct Intent {
    pub action:     String,                       // "akiva.intent.action.VIEW"
    pub data_uri:   Option<Uri>,                  // "content://..." / "https://..."
    pub mime_type:  Option<String>,               // "text/plain", "image/*"
    pub categories: Vec<String>,                  // ["akiva.intent.category.DEFAULT"]
    pub target:     IntentTarget,                 // Explicit | Implicit
    pub extras:     HashMap<String, IntentValue>, // serialisable extras
    pub flags:      IntentFlags,                  // NEW_TASK, CLEAR_TOP, …
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

## 4. Standard actions

Namespace: `akiva.intent.action.*`.

| action      | meaning                                            | data expected                |
|-------------|----------------------------------------------------|------------------------------|
| `MAIN`      | launch point of an app                             | —                            |
| `VIEW`      | display something                                  | uri                          |
| `EDIT`      | edit something                                     | uri                          |
| `PICK`      | pick an item                                       | —                            |
| `SEND`      | send / share                                       | extras: `TEXT` / `STREAM`    |
| `SHARE`     | share (synonym of `SEND`)                          | —                            |
| `SEARCH`    | search                                             | extras: `QUERY`              |
| `DIAL`      | place a call                                       | uri: `tel:...`               |
| `SETTINGS`  | open the app's settings                             | —                            |

Apps can define custom actions, but a unique namespace is
preferred: `com.mycompany.intent.action.FOO`.

## 5. Standard categories

| category                            | meaning                                                                |
|-------------------------------------|------------------------------------------------------------------------|
| `akiva.intent.category.DEFAULT`     | default (every activity not explicitly declaring a category gets this) |
| `akiva.intent.category.LAUNCHER`    | shows up in Shell                                                      |
| `akiva.intent.category.HOME`        | the Shell activity itself                                              |
| `akiva.intent.category.BROWSABLE`   | can be opened via a URL                                                |
| `akiva.intent.category.PREFERENCE`  | a preference pane                                                      |

## 6. Intent filters (in the manifest)

An activity declares which intents it can handle:

```toml
[[activities]]
id    = "main"
entry = true

[[activities.intent_filters]]
actions    = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]

[[activities]]
id = "viewer"

[[activities.intent_filters]]
actions    = ["akiva.intent.action.VIEW"]
categories = ["akiva.intent.category.DEFAULT", "akiva.intent.category.BROWSABLE"]

[[activities.intent_filters.data]]
scheme = "content"
host   = "my.content.provider"

[[activities.intent_filters.data]]
scheme     = "https"
mime_types = ["image/*"]
```

## 7. Resolution algorithm

For an implicit intent:

```
IntentRouter.resolve(intent)
  │
  ▼
Iterate registered intent filters
  │
  ├─ For each app, iterate intent filters
  ├─ Match if:
  │    ✓ action matches (exact)
  │    ✓ every category in the intent is present in the filter (subset)
  │    ✓ (planned) data — scheme / host / mime — matches
  │
  ▼
Return either Ok((package, activity)) (first match)
       or Err(ResolveError::{NoMatch, AmbiguousMatch})
  │
  ▼
If NoMatch:
  → caller sees the error
If single match:
  → ActivityManager.launch(explicit-intent)
If many:
  → (future) WindowManager.show_chooser(matches)
  → user picks
  → ActivityManager.launch(chosen)
```

The current `IntentRouter::resolve` returns the first match; the
chooser UI is not yet implemented (`AmbiguousMatch` is currently
swallowed in `apply_post`).

### Chooser UI *(planned)*

A simple dialog with a list of apps. The user can pick "just
once" or "always" (the latter is stored in a preference).

## 8. Intent delivery

### Start activity

The intent reaches the activity through `on_create` (when
freshly created) or `on_new_intent` (when reused under
`singleTop` / `singleTask` / `singleInstance`):

```rust
impl Activity for MyApp {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        if let Some(intent) = ctx.intent() {
            if let Some(uri) = intent.data_uri() {
                self.load(uri);
            }
        }
    }

    async fn on_new_intent(&mut self, ctx: &mut Context<'_>, intent: &Intent) {
        // Fires only on singleTop / singleTask / singleInstance reuse.
        if let Some(uri) = intent.data_uri() {
            self.load(uri);
        }
    }
}
```

### Start for result

App A wants a result from App B:

```rust
// App A
let intent = Intent::new("akiva.intent.action.PICK")
    .with_mime_type("image/*");
ctx.start_activity_for_result(&intent, REQUEST_PICK_IMAGE).await;

// ... later ...
impl Activity for MyApp {
    async fn on_activity_result(
        &mut self, ctx: &mut Context<'_>,
        request_code: u32, result: ActivityResult,
    ) {
        if request_code == REQUEST_PICK_IMAGE {
            if let Some(uri) = result.data_uri() {
                self.show_image(uri);
            }
        }
    }
}

// App B (the picker)
ctx.set_result(&ResultData::new().with_data_uri(selected_uri));
ctx.finish();
```

## 9. PackageManager

### Responsibilities

- **Installed registry:** holds the database of installed apps
  (loaded at boot from OPFS).
- **Install / Uninstall / Upgrade:** the flow of `.akv`
  archives in storage.
- **Resource tables:** reads `res/values/*` per package and
  hands them to the ResourceManager.
- **Saved-state persistence:** keys `sys/packages/{id}/{class}/saved_state`.
- **App info:** name, version, version_code, activities, icon,
  launch modes.

### Public API (Phase 8)

The implementation in
[`framework/src/package_manager.rs`](../framework/src/package_manager.rs)
exposes a synchronous API. Intent resolution lives in a separate
`IntentRouter` (not embedded in PackageManager as on Android).

```rust
pub struct PackageRecord {
    pub id:           String,
    pub name:         String,
    pub version:      String,
    pub version_code: u32,
    pub entry_class:  String,
    pub manifest:     Manifest,
}

impl PackageManager {
    pub fn new() -> Self;
    pub fn load_installed(
        &mut self, storage: &mut dyn Storage,
        on_error: impl FnMut(&str, LoadError),
    ) -> usize;
    pub fn install(
        &mut self, storage: &mut dyn Storage, archive_bytes: &[u8],
    ) -> Result<Report, InstallError>;
    pub fn uninstall(&mut self, storage: &mut dyn Storage, id: &str) -> bool;
    pub fn get(&self, id: &str)               -> Option<&PackageRecord>;
    pub fn list(&self)                        -> impl Iterator<Item = &PackageRecord>;
    pub fn count(&self)                       -> usize;
    pub fn read_wasm(&self, storage: &dyn Storage, id: &str)
        -> Result<Vec<u8>, StorageError>;
    pub fn read_resource_tables(
        &self, storage: &mut dyn Storage,
        on_error: impl FnMut(&str, ResourceLoadError),
    ) -> Vec<(String, ResourceTable)>;
}

// Intent resolution lives in a sibling subsystem:
impl IntentRouter {
    pub fn register_filter(&mut self, filter: IntentFilter);
    pub fn clear_package(&mut self, package_id: &str);
    pub fn resolve(&self, intent: &Intent) -> Result<(String, String), ResolveError>;
}
```

There is no `ResolveInfo` or `PackageInfo` — the actual API
returns `(String, String)` tuples and `&PackageRecord`
references. Earlier drafts of this section showed an aspirational
shape; the implementation chose a different split (between
`PackageManager` and `IntentRouter`) for cleaner ownership.

### Implementation (Phase 3 + Phase 8 + Phase 9)

| capability                                                      | status              | location                                                                         |
|-----------------------------------------------------------------|---------------------|----------------------------------------------------------------------------------|
| `Intent` struct + Explicit / Implicit targets                    | ✅                  | [`framework/src/intent.rs`](../framework/src/intent.rs)                          |
| Standard action / category constants                             | ✅                  | `intent::action::*`, `intent::category::*`                                       |
| Intent filter registry + resolve (first-match)                   | ✅                  | `IntentRouter` — `NoMatch` / `AmbiguousMatch` in the error enum                  |
| Intent wire codec (framework ⇄ SDK)                              | ✅ Stone 9.4         | `Intent::encode` / `Intent::decode` on both sides — symmetric, 16 KiB cap        |
| `ctx.start_activity(&intent)` from a wasm app                    | ✅ Stone 9.4         | `akiva_host_start_activity` → drain → `IntentRouter::resolve`                    |
| `on_new_intent` on SingleTop reuse                               | ✅ Stone 9.4         | `Framework::launch_app` detects reuse and calls `deliver_new_intent`             |
| `on_new_intent` on SingleTask / SingleInstance reuse             | ✅ Stone 9.2         | reuse path scans the foreground task / all tasks for `(pkg, class)` matches      |
| Data / mime matching (scheme / host / mime)                      | ⏳                   | `IntentFilter.data_schemes` / `mime_types` are fields; `resolve` does not yet honor them |
| `start_activity_for_result` + `on_activity_result`               | ✅ Stone 9.5         | end-to-end SDK + framework + wire (see [activity-model §2](activity-model.md))   |
| Chooser UI (when `resolve` returns ambiguous)                    | ❌                   | currently the ambiguous case errors out                                          |
| `PackageManager::install` / `uninstall` (archive-based)          | ✅ Phase 8           | including same-version / downgrade rejection + major-version saved-state wipe    |
| `PackageManager::load_installed` from OPFS at boot               | ✅                   | per-package errors reported in `BootReport`                                      |
| Resource table loading per package                               | ✅                   | `read_resource_tables` → `ResourceManager::register`                             |
| PendingIntent (§13)                                              | ❌ v0.3+             |                                                                                  |
| Broadcast receivers (§14)                                        | ❌ v0.3+             |                                                                                  |

## 10. Examples

### Example 1: Shell launches Calculator

```rust
// In the Shell's on_app_click handler:
let intent = Intent::new("akiva.intent.action.MAIN")
    .with_category("akiva.intent.category.LAUNCHER")
    .with_package("com.akiva.calculator");
ctx.start_activity(&intent).await;
```

### Example 2: an app opens a URL

```rust
let intent = Intent::new("akiva.intent.action.VIEW")
    .with_data("https://example.com");
ctx.start_activity(&intent).await;
// PackageManager picks the installed browser (if any).
```

### Example 3: share text

```rust
let intent = Intent::new("akiva.intent.action.SEND")
    .with_mime_type("text/plain")
    .with_extra("TEXT", IntentValue::Str("Hello!".into()));
ctx.start_activity(&Intent::create_chooser(intent, "Share with…")).await;
```

### Example 4: ask for an image

```rust
let intent = Intent::new("akiva.intent.action.PICK")
    .with_mime_type("image/*");
ctx.start_activity_for_result(&intent, 1001).await;
```

## 11. Security

- Every intent filter must be `exported = true` for other apps
  to be able to send to it.
- An intent with `FLAG_GRANT_READ_URI_PERMISSION` extends
  temporary URI access to the receiver only.
- The PackageManager logs every intent dispatch (for debugging
  and audit).

## 12. MVP limits

- No `PendingIntent` (deferred intent — see §13).
- No broadcast receivers (see §14).
- No full Content Provider model — only the `content://`
  scheme symbolically.
- No deep URI matching (only scheme + host + mime).
- The chooser UI is missing.

## 13. PendingIntent *(post-MVP, v0.3+)*

**Problem it solves.** An app wants to define an intent that the
system fires *later* — e.g. on a notification tap, opening a
specific activity, or on alarm, kicking off a service.

**Shape:**

```rust
pub struct PendingIntent {
    pub id:      PendingIntentId,
    pub creator: PackageId,
    pub target:  Intent,
    pub flags:   PendingIntentFlags,  // OneShot | UpdateCurrent | …
}
```

**Scenario:**

```rust
// App creates a pending intent
let trigger = Intent::new("com.akiva.calendar.action.SHOW_EVENT")
    .with_extra("event_id", IntentValue::Str(event.id.clone()));
let pending = ctx.package_manager()
    .create_pending_intent(trigger, PendingFlags::OneShot);

// App attaches it to a notification (post-MVP)
notifications.notify("reminder", NotificationSpec {
    title:      "Meeting in 10 min",
    tap_intent: Some(pending),
    ..Default::default()
});

// Later, the user taps the notification → framework dispatches
// the pending intent → calendar.ShowEventActivity opens with
// event_id in extras.
```

**API draft:**

```rust
trait PackageManager {
    fn create_pending_intent(&mut self, intent: Intent, flags: PendingFlags) -> PendingIntent;
    fn dispatch_pending_intent(&mut self, id: PendingIntentId) -> Result<(), IntentError>;
    fn cancel_pending_intent(&mut self, id: PendingIntentId);
}
```

**Security.** A PendingIntent created by app A can be dispatched
by app B (that's the whole point of notifications with actions).
But the target activity sees the intent as if app A sent it, not
B. This is managed by running it inside the creator's process,
with caller "impersonation" used only for identity checks.

## 14. Broadcast intents *(post-MVP, v0.3+)*

**Problem it solves.** Multiple apps want to know when a system
event happens — locale change, network state change, screen-on,
low-battery.

**Shape:**

- **Sender:** an app or the framework calls
  `broadcast_intent(intent)`.
- **Receivers:** apps declare in their manifest that an activity
  or a "broadcast receiver" is willing to receive.
- **Framework fan-out:** for every receiver, an instance is
  created (if not already) and the intent is delivered.

**Broadcast kinds:**

| kind     | source                     | examples                                                       |
|----------|----------------------------|----------------------------------------------------------------|
| System   | Framework only              | `BOOT_COMPLETED`, `LOCALE_CHANGED`, `SCREEN_ON`, `BATTERY_LOW` |
| Ordered  | Apps may                    | fan-out with priority; one receiver may cancel the rest        |
| Sticky   | Framework                   | "latest value" retained, new receivers see the last one (discouraged) |

**Receiver declaration:**

```toml
[[receivers]]
class = "BootReceiver"
[[receivers.intent_filters]]
actions = ["akiva.intent.action.BOOT_COMPLETED"]
```

**Standard system broadcasts (planned MVP stubs):**

The framework already **emits** these intents internally for
config-change purposes:

- `LOCALE_CHANGED` — language;
- `ORIENTATION_CHANGED`;
- `UI_MODE_CHANGED` — light / dark;
- `DENSITY_CHANGED`;
- `DISPLAY_ADDED` / `DISPLAY_REMOVED` (tab opened / closed).

For the MVP, activities receive these via the
`on_config_change` callback, not via broadcasts. In the
broadcast phase they will become real broadcasts that other apps
can subscribe to.

**Security.** A broadcast with `exported = false` reaches only
the sending app. Framework broadcasts are open to everyone by
default, except those guarded by a specific permission.
