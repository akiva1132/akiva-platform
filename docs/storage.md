# Storage

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
- רק ה-leader מחזיק את ה-handle. OPFS SyncAccessHandle exclusive per-origin-per-file, מה שמבטיח אוטומטית שאין שני workers שכותבים בו-זמנית.

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

## 3. המימוש ב-HAL

[`runtime/web/storage.js`](../runtime/web/storage.js) מחזיק:

- **In-memory map** — `Map<string, Uint8Array>` של כל ה-KV.
- **Single file at OPFS root** — `akiva-store.v1.bin`.
- **SyncAccessHandle אחד חי** למשך חיי ה-worker. נפתח ב-`storage.load()` לפני `akiva_boot`.

### מחזור חיים

1. `worker.js::bootKernel` קורא `await storage.load()` לפני `WebAssembly.instantiate` + `akiva_boot`. זה מבטיח שמכל קריאה ל-`hal_storage_*` מתוך `akiva_boot` הלאה, ה-in-memory map מאוכלס ותקף.
2. הקריאות עצמן (`hal_storage_read/write/delete`) הן סינכרוניות טהורות — `Map.get/set` בזיכרון, ועל write/delete נעשה rewrite של הקובץ כולו דרך `SyncAccessHandle.write()`.
3. בזמן failover: ה-worker הישן נהרג → הדפדפן משחרר את ה-handle → ה-worker החדש (ב-leader הבא) פותח handle משלו ב-`storage.load()`. אין צורך בסינכרון מפורש.

### פורמט הקובץ (little-endian)

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

קובץ חסר / באורך אפס / פגום → מטופל כ-store ריק, ונדרס ב-write הבא. זה מכוון: העדפת survivability על פני דיוק, כי store פגום בהקשר של MVP אומר "כנראה שינוי פורמט" ולא "איבוד נתונים".

### Fallback

אם `navigator.storage.getDirectory()` זורק (למשל בחלק מדפדפנים בלי OPFS) — ה-HAL נכנס למצב **in-memory only**. הקריאות עדיין עובדות, אבל כלום לא שורד רענון. זה מסומן ב-`Storage.persistent = false` ונרשם warning אחד.

## 4. Namespacing לפי חבילה

ברמת ה-kernel ה-namespace הוא שטוח. הפרדה בין אפליקציות היא **convention** ב-SDK:

- `sdk-rust::storage::Store::open(app)` יחזיק את `app.id` פנימית וידביק אותו אוטומטית לכל key: `"com.acme.calc/theme"` וכו'.
- ה-kernel המסגרת רשאית להשתמש ב-prefix `sys/` לשימוש פנימי שלה (למשל `sys/boot_count`, `sys/pm/installed`).
- מפתחות app-id בפורמט DNS-reverse: `com.acme.calc`. ה-slash מפריד את ה-path.

ב-Phase 2 (PermissionManager) אכיפה תעבור לשכבת ה-SDK/framework: ה-framework ידחה ניסיון של app לקרוא מפתח שלא מתחיל ב-`{own-app-id}/`.

## 5. מבחן עשן אוטומטי

[`kernel::bump_boot_counter`](../kernel/src/lib.rs) רץ בכל `akiva_boot`:

1. קורא `sys/boot_count` (4 בתים, u32 LE)
2. מוסיף 1
3. כותב בחזרה
4. רושם `kernel: boot #N (prev=M)` ל-log

דרך לאמת שה-storage עובד: לפתוח את ה-devtools, לראות `kernel: boot #1 (prev=0)` בפעם הראשונה, לרענן, ולראות את המספר עולה. לפתוח טאב שני כ-follower — המונה **לא** גדל (רק leader בוטהקר). סגירת ה-leader → הטאב השני מקודם → boot #N+1.

## 6. מגבלות Phase 1b (מה **לא** עובד עדיין)

- **Whole-file rewrite ב-write**: O(total bytes). מתאים לעד כמה KB/KV. עבור עשרות MB צריך log-structured או per-key sub-file. יטופל כשתהיה אפליקציה אמיתית שדורשת זאת.
- **ללא transactions**: שני `write()` עוקבים אפשר שיראו state ביניים אם ה-worker קורס אמצע הרצף. לא שואפים לעקביות מול crashes לפני שיהיה מנוע רציני.
- **ללא iteration / prefix scan**: אין API ל-`list_keys("com.acme.calc/")`. יתווסף כש-PackageManager ידרוש אותו.
- **ללא quota enforcement**: ה-MemoryManager של ה-kernel לא חוסם כתיבות לפי חבילה. OPFS עצמו מוגבל ע"י quota של הדפדפן, אבל אין הגבלה per-app.
- **ללא encryption**: OPFS מוגן ע"י origin isolation בלבד. מפתחות רגישים (tokens, credentials) יעברו ל-credential store נפרד ב-Phase 3.

## 7. דברים שכן-צריך-לזכור

- ה-HAL מעתיק את ה-value פעמיים: פעם ב-`new Uint8Array(memory.buffer, vptr, vlen).slice()` (כי view ל-wasm memory פג בתוקף אחרי grow), ופעם נוספת ב-`Storage.set` (הגנה על ה-caller מפני mutation חיצוני). לא מיטבי, אבל נקי. אם זה יופיע ב-profile נוכל לסגור ל-copy אחד.
- `hal_storage_read` מחזיר buffer שה-kernel צריך לשחרר — ה-HAL קורא ל-`akiva_alloc` וה-kernel (ב-`storage::read`) קורא ל-`host_dealloc` אחרי העתקה ל-`Vec<u8>`.
- `storage.close()` נקרא ב-`unload` של ה-worker. זה best-effort; הדפדפן יבטל את ה-handle גם אם לא קראנו, אבל flush מפורש מצמצם חלון לאיבוד של write אחרון.
