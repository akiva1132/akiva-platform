# Storage

> **סטטוס מימוש (מאי 2026 — Stone 11.6a-c + 11.6.7 #3):** KV API של ה-kernel + HAL מעל OPFS `SyncAccessHandle` פעילים. שכבת SDK per-app **קיימת** דרך `akiva_sdk::storage` עם sandbox automatic per-package, async-shape API (`ReadHandle::poll`-loop, `WriteHandle::await`) — ראה §8. **Multi-frame deferred-handle pattern** הוא האילוץ הנוכחי בגלל ה-cross-worker boundary; ראה [`docs/sdk-rust.md` §19.1](sdk-rust.md) לתיקון המתוכנן (embassy executor). **Per-key OPFS file layout** (Stone 11.6.7 #3, מאי 2026) — `pkgs/<pkg>/<userKey>` directory tree, app-workers פותחים readonly handles ישירות לקריאות sub-millisecond, kernel-worker בעלים בלעדי על ה-write side עם invalidation broadcast. Migration אוטומטית מ-legacy `akiva-store.v1.bin`. Transactions / prefix scan / quota enforcement / encryption — **Phase 2+ / post-MVP**.

מסמך זה מתעד את שכבת ה-storage של Akiva: הממשק שה-kernel חושף, ה-HAL שמממש אותו מעל OPFS, ומה נכון למצב נוכחי מול מה שיגיע בשלבים הבאים.

## 1. שכבות

```
  App (Rust, wasm)
    │    sdk-rust::storage::Store (עתיד: Phase 2)
    ▼
  Kernel (Rust, wasm)
    │    kernel::storage  ← API סינכרוני, string-key/bytes-value
    ▼ extern "C" imports  (see kernel/src/host.rs)
  HAL (JS, Worker)
    │    runtime/web/storage.js  (Storage class)
    ▼
  OPFS
       navigator.storage.getDirectory() → SyncAccessHandle
```

נקודות מפתח:
- ה-kernel **לא** נוגע ב-filesystem ישירות. כל I/O עובר דרך ה-HAL.
- ה-HAL רץ ב-Worker בלבד, כי רק שם זמין `createSyncAccessHandle()` שמאפשר API סינכרוני.
- ה-kernel-worker מחזיק את הזכות הבלעדית לכתיבה (`readwrite` mode); app-workers פותחים `read-only` handles לקבצים של עצמם בלבד, מהדבריקטוריה `pkgs/<package_id>/`. שני הצדדים מסתנכרנים דרך postMessage `storage_invalidate` (kernel→app) — לפני כל write הkernel סוגר את ה-readwrite, השולח invalidate, ה-app סוגר את ה-readonly. ראה §3.2.

## 2. ה-API ברמת ה-kernel

[`kernel/src/storage.rs`](../kernel/src/storage.rs) חושף:

```rust
pub enum StorageError {
    NotFound,
    BadKey,
    Io,
}

pub fn read(key: &str) -> Result<Vec<u8>, StorageError>;
pub fn write(key: &str, value: &[u8]) -> Result<(), StorageError>;
pub fn delete(key: &str) -> Result<(), StorageError>;
```

Keys הם מחרוזות UTF-8 שרירותיות. ברמת ה-kernel אין namespace enforcement — ה-convention שה-SDK מוסיף prefix של `app.id/` לכל key (ראה §4).

## 3. המימוש ב-HAL (Stone 11.6.7 #3 — per-key files)

[`runtime/web/storage.js`](../runtime/web/storage.js) מחזיק:

- **In-memory map** — `Map<string, Uint8Array>` של כל ה-KV. מקור-אמת לקריאות סינכרוניות.
- **Per-key OPFS files** — כל key הוא קובץ נפרד; שיני `/` הופכים ל-directory boundaries. דוגמאות:
  - `packages/sys.shell/manifest.toml` → `OPFS:/packages/sys.shell/manifest.toml`
  - `pkgs/com.example/notes/2026.txt` → `OPFS:/pkgs/com.example/notes/2026.txt`
- **Async write queue** — `set/delete` עוברים על ה-mirror בזיכרון מיד (sync return), ה-flush ל-OPFS נעשה בקובץ הספציפי דרך תור async (`_drainQueue`). ה-host_op handler עובר `drain()` לפני שהוא משיב, כדי ש-`WriteHandle::await` יישאר durable.
- **Lazy SyncAccessHandle** — לא נשמר handle פתוח לאורך זמן ב-kernel; כל write open-write-close, כדי ש-app-workers יוכלו להחזיק `read-only` handles במקביל. Open עם retry-with-backoff מטפל ב-race עם invalidate-handler של ה-app.

### 3.1 פורמט נתיב (storage_path.mjs — shared)

הkey מפוצל על `/`, וכל segment עובר percent-encoding. הסט המותר ללא קידוד: `[A-Za-z0-9._-]`. כל תו אחר (כולל non-ASCII) מקודד לפי UTF-8 bytes כ-`%XX...`. הקידוד מוגדר ב-[`runtime/web/storage_path.mjs`](../runtime/web/storage_path.mjs) ומשמש את שני הצדדים — kernel-worker ו-app-worker — כדי שיסכימו על אותו נתיב OPFS לכל key.

### 3.2 Invalidation protocol (kernel → app)

לפני כל write/delete על key מתחת ל-`pkgs/<pkg>/<userKey>`, ה-Storage מפעיל `onInvalidate(packageId, userKey)`. ה-kernel-worker (worker.js) חיבר את ה-callback ל-`AppRegistry.broadcastStorageInvalidate`, ששולח postMessage `{type: "storage_invalidate", userKey}` לעובד של החבילה (אם הוא נטען).

ה-app worker shell ([`app_worker_shell.js`](../runtime/web/app_worker_shell.js)) שומר `Map<userKey, SyncAccessHandle>` של readonly handles. `storage_invalidate` סוגר את ה-handle ל-userKey נתון; `userKey === null` סוגר הכל ומאפס את ה-package directory cache (uninstall path).

### 3.3 Direct readonly read path (Stone 11.6.7 #3)

```
app worker:
  STORAGE_GET(key) →
    cache hit (cached SyncAccessHandle)? ──yes──→ sync read, return future READY  (~0.1ms)
    cache miss?                          ──→ post host_op to kernel (~2ms)
                                              + warmStorageHandle(key) async (אופן רקע — ה-handle
                                                ב-cache לקריאה הבאה)
```

הdesign target: קריאה רגילה (warm cache) ≤0.5ms; cold read ≤2ms (זהה ל-pre-Stone 11.6.7).

### 3.4 מחזור חיים

1. `worker.js::bootKernel` קורא `await storage.load()` לפני `WebAssembly.instantiate` + `akiva_boot`. ה-`load()` מבצע:
   - `navigator.storage.getDirectory()` → root handle.
   - **Migration**: אם `akiva-store.v1.bin` קיים בroot, parsing entry-by-entry → write לכל key לקובץ הנפרד שלו → `removeEntry` של הקובץ הישן.
   - **Walk**: רקורסיבי על העץ, populate ה-in-memory mirror באמצעות `getFile()` + `arrayBuffer()` (async, no handle lock).
2. הקריאות עצמן: `get` סינכרוני מה-mirror; `set/delete` סינכרוני על ה-mirror + enqueue flush.
3. ה-flush pump async opens `SyncAccessHandle` על הקובץ הנדרש (עם retry-with-backoff אם app holding readonly), כותב את הbytes, סוגר.
4. Failover: leader מת → הדפדפן משחרר handles → leader חדש מבצע `load()` שוב, walks the tree, populates mirror.

### 3.5 פורמט legacy (migration source)

הקובץ הישן `akiva-store.v1.bin` (little-endian):

```
offset  size    content
0       4       magic = "AKSV" (0x56534b41 LE)
4       4       version = 1
8       4       entry_count
12      *       entries:
                  key_len: u32
                  key:     UTF-8 bytes
                  val_len: u32
                  val:     raw bytes
```

`Storage._parseLegacyBlob` קורא את הפורמט הזה ב-`load()` ומעביר entry-by-entry ל-layout החדש. אחרי migration מוצלחת ה-legacy מוסר.

### 3.6 Fallback

אם `navigator.storage.getDirectory()` זורק (למשל בחלק מדפדפנים בלי OPFS) — ה-HAL נכנס למצב **in-memory only**. הקריאות עדיין עובדות, אבל כלום לא שורד רענון. זה מסומן ב-`Storage.persistent = false` ונרשם warning אחד. ב-mode הזה ה-async write queue פשוט לא רץ.

## 4. Namespacing לפי חבילה

ברמת ה-kernel ה-namespace הוא שטוח. הפרדה בין אפליקציות היא **convention** ב-SDK:

- `sdk-rust::storage::Store::open(app)` יחזיק את `app.id` פנימית וידביק אותו אוטומטית לכל key: `"com.acme.calc/theme"` וכו'.
- ה-framework רשאי להשתמש ב-prefix `sys/` לשימוש פנימי (`sys/boot_count`, `sys/packages/index`).
- חבילות מותקנות יושבות תחת `packages/{id}/...` — ראה `framework/src/package_manager.rs`. ה-PackageManager ניגש אל ה-storage דרך ה-trait `framework::storage::Storage`; ב-runtime האמיתי ה-kernel מספק `KernelStorage` שעטיפה של `kernel::storage`, ובבדיקות יחידה `MemoryStorage` החלופי.
- מפתחות app-id בפורמט DNS-reverse: `com.acme.calc`. ה-slash מפריד את ה-path.

### מפתחות שמורים למערכת

| Key | בעלים | תוכן |
|---|---|---|
| `sys/boot_count` | kernel | `u32` LE — מונה boots (smoke test) |
| `sys/packages/index` | framework.package_manager | רשימת package IDs, LF-separated |
| `packages/{id}/manifest.toml` | framework.package_manager | תוכן ה-manifest UTF-8 |
| `packages/{id}/app.wasm` | framework.package_manager | ה-module של ה-app |
| `{app.id}/...` | SDK (per-app sandbox) | storage לאפליקציה |

ב-Phase 2 (PermissionManager) אכיפה תעבור לשכבת ה-SDK/framework: ה-framework ידחה ניסיון של app לקרוא מפתח שלא מתחיל ב-`{own-app-id}/`.

### Implementation (Phase 1b + 11.6.7 #3)

| יכולת | סטטוס | איפה |
|---|---|---|
| `Storage::read/write/delete` trait (framework-side) + `MemoryStorage` לבדיקות | ✅ | [`framework/src/storage.rs`](../framework/src/storage.rs) |
| `kernel::storage` API סינכרוני string→bytes | ✅ | [`kernel/src/storage.rs`](../kernel/src/storage.rs) |
| HAL מעל OPFS — per-key files, in-memory mirror, async write queue | ✅ Stone 11.6.7 #3 | [`runtime/web/storage.js`](../runtime/web/storage.js) |
| Migration `akiva-store.v1.bin` → per-key layout | ✅ Stone 11.6.7 #3 | `Storage._migrateLegacyIfPresent` |
| Shared path encoding (kernel + apps) | ✅ Stone 11.6.7 #3 | [`runtime/web/storage_path.mjs`](../runtime/web/storage_path.mjs) |
| `akiva-store.v1.bin` נטען לפני `akiva_boot` (worker.js::bootKernel) | ✅ historical (auto-migrated) | |
| OPFS unavailable → in-memory fallback + `persistent=false` flag | ✅ | `Storage.persistent` |
| Kernel-only write (exclusive readwrite handle, brief acquire-write-close) | ✅ Stone 11.6.7 #3 | `Storage._writeFile` |
| App-side direct readonly handles (Direct read path) | ✅ Stone 11.6.7 #3 | [`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js) |
| `storage_invalidate` postMessage על write/delete/uninstall | ✅ Stone 11.6.7 #3 | `AppRegistry.broadcastStorageInvalidate` |
| Framework saved-state writes (`sys/packages/{id}/{class}/saved_state`) | ✅ | `package_manager::key_saved_state` |
| SDK per-app `Store::open(app)` עם prefix אוטומטי | ❌ Phase 2+ | apps היום לא עושים storage ישיר |
| `list_keys` / prefix scan | ❌ | יתווסף כשיידרש |
| Transactions (atomic multi-write) | ❌ | כנ"ל |
| Quota per-app + enforcement ע"י MemoryManager | ❌ Phase 10 | |
| Encryption at rest | ❌ post-MVP | |

## 5. מבחן עשן אוטומטי

[`kernel::bump_boot_counter`](../kernel/src/lib.rs) רץ בכל `akiva_boot`:

1. קורא `sys/boot_count` (4 בתים, u32 LE)
2. מוסיף 1
3. כותב בחזרה
4. רושם `kernel: boot #N (prev=M)` ל-log

דרך לאמת שה-storage עובד: לפתוח את ה-devtools, לראות `kernel: boot #1 (prev=0)` בפעם הראשונה, לרענן, ולראות את המספר עולה. לפתוח טאב שני כ-follower — המונה **לא** גדל (רק leader בוטהקר). סגירת ה-leader → הטאב השני מקודם → boot #N+1.

## 6. מגבלות (מה **לא** עובד עדיין)

- ~~**Whole-file rewrite ב-write**~~ — ✅ נסגר ב-Stone 11.6.7 #3. כל key הוא קובץ נפרד; write רק על הקובץ של ה-key הספציפי.
- **ללא transactions**: שני `write()` עוקבים אפשר שיראו state ביניים אם ה-worker קורס אמצע הרצף. לא שואפים לעקביות מול crashes לפני שיהיה מנוע רציני.
- **ללא iteration / prefix scan**: אין API ל-`list_keys("com.acme.calc/")`. יתווסף כש-PackageManager ידרוש אותו.
- **ללא quota enforcement**: ה-MemoryManager של ה-kernel לא חוסם כתיבות לפי חבילה. OPFS עצמו מוגבל ע"י quota של הדפדפן, אבל אין הגבלה per-app.
- **ללא encryption**: OPFS מוגן ע"י origin isolation בלבד. מפתחות רגישים (tokens, credentials) יעברו ל-credential store נפרד ב-Phase 3.
- **Async-flush durability gap**: write על key ש-app worker קורס מיידית אחרי שלוקח את ה-status `READY` יכול לאבד את ה-bytes אם ה-tab נסגר לפני ש-`drain()` סיים. ב-host_op channel מבטיח: `_handleHostOp` קורא `await this._storage.drain()` לפני `host_op_done`, כך ש-`WriteHandle::await` חוזר רק אחרי ש-OPFS flush הסתיים.

## 7. דברים שכן-צריך-לזכור

- ה-HAL מעתיק את ה-value פעמיים: פעם ב-`new Uint8Array(memory.buffer, vptr, vlen).slice()` (כי view ל-wasm memory פג בתוקף אחרי grow), ופעם נוספת ב-`Storage.set` (הגנה על ה-caller מפני mutation חיצוני). לא מיטבי, אבל נקי. אם זה יופיע ב-profile נוכל לסגור ל-copy אחד.
- `hal_storage_read` מחזיר buffer שה-kernel צריך לשחרר — ה-HAL קורא ל-`akiva_alloc` וה-kernel (ב-`storage::read`) קורא ל-`host_dealloc` אחרי העתקה ל-`Vec<u8>`.
- `storage.close()` נקרא ב-`unload` של ה-worker. זה best-effort; הדפדפן יבטל את ה-handle גם אם לא קראנו, אבל flush מפורש מצמצם חלון לאיבוד של write אחרון.

## 8. SDK per-app storage (Stone 11.6a-c — מאי 2026)

מ-Stone 11.6 a/b/c apps יכולות להשתמש ב-storage דרך SDK API:

```rust
use akiva_sdk::storage;

storage::write("note", b"hello");           // fire-and-forget
let handle = storage::read("note");         // -> Option<ReadHandle>
storage::remove("note");                    // fire-and-forget
```

### 8.1 Sandbox per-package (kernel side)

ה-host (`AppRegistry._dispatchStorage` ב-`runtime/web/app_registry.js`) מתחיל כל user-key ב-prefix:

```
pkgs/{package_id}/{user_key}
```

לדוגמה: app `com.example.notes` שכותב `"note"` יראה את הbytes בOPFS תחת `pkgs/com.example.notes/note`. App אחר שכותב לאותו user-key לא רואה את ה-data של ה-app הראשון. ה-sandbox נאכף ע"י ה-AppRegistry — ה-SDK עצמו אינו עושה את ה-prefix.

### 8.2 Wire format (host_op channel)

ראה [`framework/src/wire/op_kinds.rs`](../framework/src/wire/op_kinds.rs) `pub mod io`:

| Op | Args | Result |
|---|---|---|
| `STORAGE_GET` (0x0103) | `[key utf8...]` | `[present:u8, value...]` |
| `STORAGE_SET` (0x0104) | `[key_len:u32 LE, key, value]` | `[]` |
| `STORAGE_DELETE` (0x0105) | `[key utf8...]` | `[]` |

ה-`present` byte ב-GET מבדיל בין "key absent" (=0, no value bytes) ל-"key exists with empty value" (=1, then 0 bytes value). זה מאפשר ל-`Option<Vec<u8>>` במשמע ה-SDK להיות מדויק.

### 8.3 Capability gate

`STORAGE_GET/SET/DELETE` כולם דורשים `akiva.permission.STORAGE_SANDBOX`. ה-grant הזה ב-MVP **autoMatic** לכל manifest שמצהיר עליו (אין UI לconfirmation; ה-app declares ב-manifest, ה-PackageManager grants). אכיפה: `AppRegistry._handleHostOp` בודק לפני שmosbim ל-`_dispatchStorage`. Apps ללא ה-grant מקבלים `STATUS::ERROR` עם UTF-8 reason.

### 8.4 Direct readonly path (Stone 11.6.7 #3)

מ-Stone 11.6.7 #3, `STORAGE_GET` מהו-app יכולה להיענות ישירות מ-OPFS בלי round-trip ל-kernel. ה-app-worker shell פותח `read-only` `SyncAccessHandle` לקובץ של ה-key (תחת `pkgs/<package_id>/<userKey>`) ב-cache lazy:

- **Cache hit** — קריאה sync מה-handle, סופקת את ה-future מיד ב-status READY (~0.1ms).
- **Cache miss** — fall-through ל-host_op (kernel הוא in-memory mirror); במקביל async open populates the cache לקריאה הבאה.
- **Invalidation** — אחרי כל write/delete של ה-kernel, נשלח postMessage `storage_invalidate {userKey}` שסוגר את ה-handle ב-cache. הקריאה הבאה אסינכרונית-נפתחת מחדש מקובץ עדכני.

ה-design tradeoff: קריאות חמות הן sub-millisecond; הקריאה הראשונה לכל key (cold) משלמת host_op (~2ms) פעם אחת ואז נשארת מהירה.

### 8.5 Multi-frame deferred reads (limitation)

`storage::read` לא יכול להיות sync תחת cross-worker boundary — ה-app-worker חייב לחזור ל-JS event loop כדי שה-host_op_done יגיע. לכן ה-API:

```rust
let handle = storage::read("k");  // returns Option<ReadHandle> — fast
// In on_render, polled until ready:
if let Some(h) = &self.loading {
    if let Some(result) = h.poll() { ... }
}
```

ראה [`docs/sdk-rust.md` §19.1](sdk-rust.md) לרקע מלא וה-fix המתוכנן (embassy executor, יבטל את ה-handle pattern).

### 8.6 Fire-and-forget writes — closed (Stone 11.6.7 #1)

מ-Stone 11.6.7 #1, `storage::write` מחזיר `WriteHandle: Future<Output=Result<(), StorageError>>`. אפליקציות שדורשות durability עושות `await`, ה-host_op ב-kernel מחזיר רק אחרי `storage.drain()` שמבטיח שה-OPFS flush הסתיים. Drop של ה-handle ללא await שומר על סמנטיקת fire-and-forget של pre-11.6.7. ראה [`sdk-rust/src/storage.rs::WriteHandle`](../sdk-rust/src/storage.rs).

### 8.7 Updated Implementation status (Stone 11.6a-c + 11.6.7 #1, #3)

| יכולת | סטטוס | איפה |
|---|---|---|
| SDK per-app `storage::read/write/remove` עם sandbox automatic | ✅ Stone 11.6c | [`sdk-rust/src/storage.rs`](../sdk-rust/src/storage.rs) |
| Host-side sandbox enforcement (`pkgs/{package_id}/...` prefix) | ✅ Stone 11.6b | [`runtime/web/app_registry.js::_dispatchStorage`](../runtime/web/app_registry.js) |
| Wire format `STORAGE_GET/SET/DELETE` op_kinds | ✅ Stone 11.6a | [`framework/src/wire/op_kinds.rs`](../framework/src/wire/op_kinds.rs) |
| Cross-worker `host_op` round-trip | ✅ Stone 11.4c-3 | AppRegistry `_handleHostOp` |
| `STORAGE_SANDBOX` capability gate | ✅ | per-call check ב-`_handleHostOp` |
| Multi-frame `ReadHandle` pattern | ⚠ workaround | sdk-rust/src/storage.rs |
| `WriteHandle` (durability handle) | ✅ Stone 11.6.7 #1 | sdk-rust/src/storage.rs |
| Direct readonly OPFS handles ב-app-worker | ✅ Stone 11.6.7 #3 | [`runtime/web/app_worker_shell.js`](../runtime/web/app_worker_shell.js) |
| Per-key OPFS file layout + migration | ✅ Stone 11.6.7 #3 | [`runtime/web/storage.js`](../runtime/web/storage.js) |
| `let bytes = storage::read(k).await` (fluent) | ❌ depends on §19.1 | TODO |
| `list_keys` / prefix scan | ❌ Phase 2+ | |
| Quota per-app | ❌ Phase 10 | |
