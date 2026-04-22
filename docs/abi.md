# Akiva ABI — חוזה בין Runtime ל-Applications

## 1. מטרה

ה-ABI מגדיר איך wasm component של אפליקציה מדבר עם ה-runtime. חוזה יציב, מתועד, version-tracked, מתואר ב-**WIT (WebAssembly Interface Types)** של ה-Component Model.

## 1.1 שני רבדים: v0 "raw" ↔ v1 "component"

התיעוד מחולק לשני מצבים:

| רובד | מצב | איפה מוגדר |
|---|---|---|
| **v0 — Raw exports** | המצב הנוכחי ב-Phase 2b. apps הם פשוט `wasm32-unknown-unknown` modules שמייצאים `extern "C"` functions. אין Component Model, אין wit-bindgen. | §2 למטה |
| **v1 — Component Model** | היעד. apps הם Components שמייצאים את ה-world `akiva:app/guest` ומייבאים `akiva:app/host`. | §3 ואילך |

הסיבה לרובד הכפול: ל-browser wasm עדיין אין runtime מובנה ל-Component Model. אימוץ מוקדם של wit-bindgen מחייב toolchain ו-glue נוספים לכל שפה. ה-v0 הוא חוזה זמני שמאפשר לנו ל-boot את הפלטפורמה ב-Rust ללא תלויות חיצוניות, תוך שמירה על ה-v1 כ-shape של העתיד.

המעבר מ-v0 ל-v1 יהיה additive, לא breaking: ה-WIT interfaces ב-§5 תוכננו כך שה-raw ABI של v0 נמפה אליהם 1:1 (משפחות הקריאות זהות).

## 1.2 עיקרון: ABI narrow-by-design

ABI v0 קטן בכוונה — 7 exports app-side ו-7 imports kernel→host. זה לא accident; זה policy. ABI רחב מדי מוקדם מדי הופך לעוגן שמאט כל שינוי עתידי, ומכתיב decisions שכבת framework לעוד שנים.

כל הרחבה ל-ABI — בין אם export חדש ב-app, import חדש ב-host, או שדה חדש ב-wire format — חייבת לעבור על שלושת המבחנים:

1. **Use case מוכח.** לפחות reference app אחד שבלי ההרחבה לא פועל בצורה סבירה. "might be useful later" לא מספיק.
2. **Cross-app justification.** הרחבה שמשרתת app בודד או use case נישתי שייכת ל-framework או ל-sdk, לא ל-ABI. ABI משרת רק את הדברים שכל app, או runtime core, חייבים.
3. **Forward-compat ל-v1.** יש mapping ברור ל-WIT interface ב-§5. אם ההרחבה לא מתורגמת לצורה נקייה ב-Component Model, זה תמרור אזהרה שה-shape שלה לא נכון.

הכלל המעשי: עדיף פונקציה אחת חסרה היום מאשר שבע פונקציות מיותרות שנצטרך לגרור לנצח. הרחבות נוספות ל-`hal_app_*` או ל-paint-op opcodes עוברות review מפורש ב-PR, ומתועדות בסעיף גרסאות (§10).

---

## 2. App ABI v0 — raw exports (Phase 2b)

### 2.1 מודל ריצה

כל app הוא wasm module עצמאי שה-host (worker.js) טוען לצד ה-kernel wasm. ה-kernel **לא** מזהה את האפליקציה ב-address space שלו — הוא שולח את הבקשות ל-host דרך imports בשם `hal_app_*`, וה-host מנתב אותן ל-instance הנכון של ה-app. יש לכך יתרונות מיידיים: isolation בין apps, memory growth עצמאי, crash containment.

```
  Kernel wasm           Worker JS host            App wasm (one per package)
  ───────────           ──────────────            ─────────────────────────
  WasmActivity   ──►    hal_app_lifecycle   ──►   akiva_app_on_resume(handle)
  (framework)           (routes by handle)         (app-side)
                  ◄──   bytes via hal_app_render  ◄── akiva_app_render → *u8
```

ה-`handle` הוא `u32` לא-אפסי שה-host מקצה ב-`hal_app_register` ושומר בו את ה-tuple `(package_id, class, wasm_instance)`. ל-kernel אין מושג מה ה-handle מייצג.

### 2.2 Exports שה-app חייב לספק

כל app חייב לייצא את ה-wasm `memory` (מופעל אוטומטית ע"י rustc ל-`wasm32-unknown-unknown`) ואת הפונקציות הבאות (C ABI). `runtime/web/app_registry.js::REQUIRED_EXPORTS` בודק את הרשימה בזמן טעינה; apps שחסרים משהו נדחים לפני הקריאה הראשונה למחזור חיים.

```rust
// Memory — ה-host משתמש בכדי להזריק data ל-app (intents, saved state, ...).
extern "C" fn akiva_app_alloc(size: u32) -> *mut u8;
extern "C" fn akiva_app_dealloc(ptr: *mut u8, size: u32);

// ABI version — null-terminated UTF-8 ("0.1.0").
extern "C" fn akiva_app_abi_version() -> *const u8;

// Lifecycle — handle הוא per-activity.
extern "C" fn akiva_app_create(handle: u32, class_ptr: *const u8, class_len: u32);
extern "C" fn akiva_app_start(handle: u32);
extern "C" fn akiva_app_resume(handle: u32);
extern "C" fn akiva_app_pause(handle: u32);
extern "C" fn akiva_app_stop(handle: u32);
extern "C" fn akiva_app_destroy(handle: u32);

// Input.
extern "C" fn akiva_app_pointer(
    handle: u32, x: f32, y: f32, button: u32, phase: u32, ts_ms: f64,
);
extern "C" fn akiva_app_key(
    handle: u32, code: u32, modifiers: u32, phase: u32, ts_ms: f64,
);
extern "C" fn akiva_app_resize(
    handle: u32, width_px: u32, height_px: u32, density: f32,
);

// Render — מחזיר את הכתובת של buffer paint-ops ב-memory של ה-app.
// ה-host קורא את האורך מ-*out_len_ptr, מעתיק, וממשיך.
extern "C" fn akiva_app_render(handle: u32, ts_ms: f64, out_len_ptr: *mut u32) -> *const u8;

// Saved state — סימטרי ל-render. ה-app מסדר את ה-UI state שלו לפי
// הפורמט של `SavedState` ([sdk-rust/src/activity.rs]) ומחזיר pointer
// ל-buffer app-owned שתקף עד הקריאה הבאה ל-save_state עם אותו handle.
// ב-restore, ה-host מעתיק bytes שמורים פנימה (דרך akiva_app_alloc) וקורא
// ל-restore_state; ה-app מפענח ומעדכן את ה-state שלו בפנים.
extern "C" fn akiva_app_save_state(handle: u32, out_len_ptr: *mut u32) -> *const u8;
extern "C" fn akiva_app_restore_state(handle: u32, bytes_ptr: *const u8, bytes_len: u32);
```

ה-buffer שמוחזר מ-`akiva_app_render` הוא app-owned: ה-app חייב להחזיק אותו בחיים לפחות עד הקריאה הבאה ל-`akiva_app_render` עם אותו handle. ה-convention הוא buffer per-handle שנמחזר כל frame — מקצה פעם אחת, מנקה עם `clear()`, ומחזיר את הכתובת שוב.

### 2.3 Imports (host → app)

ה-host מייצא ל-wasm של האפליקציה imports תחת module name `env`. המינימום המונח ב-Phase 2b.3:

```rust
// Write a UTF-8 string to the host log with per-app prefix.
// level: 0=trace 1=debug 2=info 3=warn 4=error (מקביל ל-hal_log של ה-kernel).
// ptr/len: תוך ה-memory של ה-app; ה-host קורא אותן inline.
extern "C" fn akiva_host_log(level: u32, ptr: *const u8, len: u32);

// Measure the rendered width of a UTF-8 string at the given font size,
// in the same font the renderer uses (system-ui stack). Height is
// deterministic from `size` (line-height ratio), so the host only
// returns width; the SDK computes height locally.
extern "C" fn akiva_host_text_width(ptr: *const u8, len: u32, size: f32) -> f32;

// Resolve @color/name for the calling package. Returns 0xAARRGGBB.
// Undefined names return 0xFFFF00FF (magenta) and log a host-side
// warning — missing refs surface visually, not silently.
extern "C" fn akiva_host_resource_color(name_ptr: *const u8, name_len: u32) -> u32;

// Resolve @string/name for the calling package. Writes UTF-8 bytes into
// [out_ptr, out_ptr + out_cap) and returns the *actual* length. If the
// returned length exceeds out_cap, nothing was written — caller
// reallocates to that size and calls again. Undefined names write
// "⟨missing:NAME⟩" and return its length.
extern "C" fn akiva_host_resource_string(
    name_ptr: *const u8, name_len: u32,
    out_ptr: *mut u8, out_cap: u32,
) -> u32;

// Probes: 1 if the ref is registered, 0 otherwise. Used by call sites
// that need to distinguish "undefined" from "happens to equal the
// placeholder". Optional for most apps — the AKML inflater doesn't
// probe; it just renders the placeholder when a ref misses.
extern "C" fn akiva_host_resource_has_color(name_ptr: *const u8, name_len: u32) -> u32;
extern "C" fn akiva_host_resource_has_string(name_ptr: *const u8, name_len: u32) -> u32;
```

כל app מקבל את ה-set הבסיסי הזה אוטומטית ללא הצהרה. הרחבות נוספות (finish, start_activity, storage read/write, intent dispatch) תיכנסנה כשאפליקציות אמיתיות יצטרכו אותן, ובכפוף למבחני §1.2 — כל הרחבה דורשת use case מוכח ו-mapping ל-WIT interface ב-§5.

**Stone 9.3 + 9.4 — מיושם:**

```rust
// אפליקציה מסיימת את עצמה (מקביל ל-Android Activity.finish()).
// ה-host מפעיל את אותו finish_requested flag שה-Shell framework-side
// משתמש בו היום. handle מזהה את ה-activity; קריאה מ-on_create תגרום
// ל-Created→Destroyed shortcut (skip על Start/Resume/Pause/Stop).
extern "C" fn akiva_host_finish(handle: u32);

// Stone 9.4 — app-initiated start_activity. intent_ptr/len מצביעים
// ל-blob ב-memory של ה-app שנוצר ע"י `Intent::encode` (ראה §2.6).
// ה-host מעתיק מייד החוצה, ה-framework מוציא אותו דרך
// `hal_app_drain_intent` בסוף ה-callback, ה-IntentRouter פותר.
// קריאות מרובות באותו dispatch — האחרון מנצח (single slot, מתאים
// ל-`ctx.launch_requested` הבודד בצד ה-framework).
extern "C" fn akiva_host_start_activity(handle: u32, intent_ptr: *const u8, intent_len: u32);
```

**מתוכננות ל-Phase 9+** (לא מיושמות עדיין):

```rust
// Stone 9.3 follow-up — system-wide Back (pops stack top אם לא root;
// אחרת no-op). שונה מ-akiva_host_finish רק בסמנטיקה: Back הוא Shell
// policy, finish הוא כוונה מפורשת של ה-app. נדחה כרגע — ה-Framework
// כבר קוצר על Esc מ-key events, אין קריאה מ-app שצריכה זאת.
extern "C" fn akiva_host_back();

// Stone 9.5 — אותו דבר עם request code שה-framework יזכור וימסור חזרה
// דרך akiva_app_activity_result.
extern "C" fn akiva_host_start_activity_for_result(
    intent_ptr: *const u8, intent_len: u32, request_code: u32,
);
extern "C" fn akiva_host_set_result(result_ptr: *const u8, result_len: u32);
```

Exports חדשים בצד ה-app שה-host קורא להם (מיושמים):

```rust
// Stone 9.4 — SingleTop (ו-Phase 9.2 SingleTask/SingleInstance) reuse
// fires this with the fresh intent. handle מזהה את ה-activity הקיימת;
// ptr/len — blob של `Intent::encode` שה-host מעתיק לתוך app memory לפני
// הקריאה (symmetric עם `akiva_app_restore_state`). ה-activity נשארת
// `Resumed` ולא עוברת דרך on_create/on_start/on_resume.
extern "C" fn akiva_app_new_intent(handle: u32, ptr: *const u8, len: u32);
```

**Stone 9.5 — מיושם:**

```rust
// Callee sees — set the result bytes that will be delivered when it
// finishes. Idempotent; last call wins. Silently dropped if the
// activity wasn't launched via start_activity_for_result.
extern "C" fn akiva_host_set_result(handle: u32, result_ptr: *const u8, result_len: u32);

// Caller sees — result hand-off from a for-result launch. bytes encode
// an ActivityResult (see §2.5.2). Fires AFTER the callee's on_destroy,
// BEFORE the caller's Paused→Resumed edge (Android parity).
extern "C" fn akiva_app_activity_result(
    handle: u32, request_code: u32, ptr: *const u8, len: u32,
);
```

ב-v1 כל ה-imports יתמפו ל-interfaces המובנים של ה-world `akiva:app/host`: `log`, `activity-api`, `storage`, `intent`, וכו'. נקודת ה-mapping: `akiva_host_log` ⇔ `log.log(level, msg)`, `akiva_host_text_width` ⇔ `text.measure-width(text, size)`.

### 2.3.1 Imports by Capability (Stone 10.3)

האם ה-host מספק לאפליקציה import ספציפי נקבע לפי ה-permissions ש-ה-`PermissionController` הקצה לה ב-install time. ה-Rust-side mapping ב-[`framework/src/permissions.rs`](../framework/src/permissions.rs) הוא ה-source of truth; worker.js שואל את ה-kernel (ב-Stone 10.3b, עדיין לא ב-wiring) ומסנן את ה-`env` object לפני `WebAssembly.instantiate`. אפליקציה שמנסה לייבא פונקציה לא מורשה — `LinkError` ב-instantiation, לא runtime panic.

| Capability                          | Host imports mounted                                  |
|-------------------------------------|-------------------------------------------------------|
| `auto` (ללא הצהרה)                  | `akiva_host_log`, `akiva_host_text_width`, `akiva_host_resource_color`, `akiva_host_resource_string`, `akiva_host_resource_has_color`, `akiva_host_resource_has_string`, `akiva_host_finish`, `akiva_host_set_result` |
| `akiva.permission.LAUNCH_ANY_APP`   | `akiva_host_start_activity`, `akiva_host_start_activity_for_result` |
| `akiva.permission.STORAGE_SANDBOX`  | *(reserved — `akiva_host_storage_*` לא נחת ב-HAL של ה-app עדיין; framework-side save/restore עובר דרך `hal_app_save_state` / `hal_app_restore_state` ב-§2.4)* |
| `akiva.permission.QUERY_PACKAGES`   | *(reserved — `akiva_host_query_packages` post-MVP)* |
| `akiva.permission.NOTIFICATIONS`    | *(reserved — post-MVP)* |
| `akiva.permission.NETWORK`          | *(reserved — post-MVP)* |
| `akiva.permission.CAMERA`           | *(reserved — post-MVP)* |

ה-auto set כולל את `akiva_host_set_result` למרות שהוא "app writing to host" — כי כל activity שקריאו לה-`start_activity_for_result` חייבת יכולת להחזיר תוצאה, גם אם ה-caller ההוא משייך לאפליקציה אחרת. הגדרת ה-mapping מפורשת ב-Rust: `AUTO_IMPORTS` ו-`imports_for_capability` (ראה [permissions.rs](../framework/src/permissions.rs)).

**עיקרון האכיפה:** "link error, not runtime error." ה-permission check לא קורה בזמן call-site; הוא קורה פעם אחת ב-instantiation. אם ה-host לא סיפק את ה-import, ה-wasm module לא הצליח בכלל להיטען — ה-app לא יכול להגיע ל-code path שמשתמש ב-permission שאין לו.

### 2.4 Imports (kernel → host) — `hal_app_*`

אלה imports שה-kernel wasm מבקש; ה-host (worker.js) מספק. מוגדרים ב-[kernel/src/host.rs](../kernel/src/host.rs):

```rust
fn hal_app_register(
    package_id_ptr: *const u8, package_id_len: usize,
    class_ptr: *const u8,      class_len: usize,
) -> u32;  // 0 = unknown package/class
fn hal_app_unregister(handle: u32);
fn hal_app_lifecycle(handle: u32, transition: u32);  // 0=Create .. 5=Destroy
fn hal_app_pointer(handle: u32, x: f32, y: f32, button: u32, phase: u32, ts_ms: f64);
fn hal_app_key(handle: u32, code: u32, modifiers: u32, phase: u32, ts_ms: f64);
fn hal_app_resize(handle: u32, width_px: u32, height_px: u32, density: f32);
fn hal_app_render(handle: u32, ts_ms: f64, out_ptr: *mut u8, out_cap: u32) -> u32;
// ^ returns total bytes produced by the app. If > out_cap the kernel's
//   buffer was truncated; kernel should retry with a larger allocation.

fn hal_app_save_state(handle: u32, out_ptr: *mut u8, out_cap: u32) -> u32;
// ^ same two-phase buffer contract as render: returns total bytes the
//   app produced. Truncated blobs trigger a retry with a larger buffer.
fn hal_app_restore_state(handle: u32, bytes_ptr: *const u8, bytes_len: u32) -> u32;
// ^ 0 on success, non-zero on handle miss / alloc failure / host error.

// Stone 9.3 — read + clear the per-handle "finish requested" flag the
// app raised via `akiva_host_finish`. Called by `WasmActivity` at the
// end of every ctx-bearing callback; return 1 flips
// `ActivityContext.finish_requested = true` in the active dispatch.
fn hal_app_drain_finish(handle: u32) -> u32;

// Stone 9.4 — read + clear the per-handle pending intent blob staged
// via `akiva_host_start_activity`. Two-phase buffer: the return value
// is the total encoded size; if it exceeds `out_cap` nothing is copied
// and the blob stays armed so the kernel can retry with a larger
// buffer (same shape as `hal_app_render` / `hal_app_save_state`).
// Zero means "nothing pending" — distinct from "empty intent" (which
// can't happen; the encoder's minimum is 17 bytes).
fn hal_app_drain_intent(handle: u32, out_ptr: *mut u8, out_cap: u32) -> u32;

// Deliver an already-resolved intent to a live activity handle (fires
// the app's `akiva_app_new_intent` export). Symmetric with
// `hal_app_restore_state`: host copies bytes into app memory via
// `akiva_app_alloc`, calls the export, frees after return. Returns 0
// on success; non-zero on handle miss / alloc failure / app trap.
fn hal_app_new_intent(handle: u32, bytes_ptr: *const u8, bytes_len: u32) -> u32;

// Stone 9.5 — three primitives matching the Stone 9.4 shapes.
//
// Drain a pending start_activity_for_result request. Two-phase buffer:
// returns total size of the encoded Intent. `rc_out` is always written
// with the request_code on a successful probe — so a retry (when
// `needed > out_cap`) finds the same value. Zero return = nothing
// pending.
fn hal_app_drain_start_for_result(
    handle: u32, out_ptr: *mut u8, out_cap: u32, rc_out: *mut u32,
) -> u32;

// Drain whatever bytes the app stashed via akiva_host_set_result.
// Same two-phase contract; payload is an encoded ActivityResult
// (see §2.5.2). Zero return = nothing stashed.
fn hal_app_drain_set_result(handle: u32, out_ptr: *mut u8, out_cap: u32) -> u32;

// Fire the app's akiva_app_activity_result export with the
// caller-chosen request_code and the encoded ActivityResult bytes.
// Same rc convention as hal_app_new_intent.
fn hal_app_activity_result(
    handle: u32, request_code: u32, bytes_ptr: *const u8, bytes_len: u32,
) -> u32;
```

### 2.5 Paint-op wire format

ה-app מ-serialize את פקודות הציור שלו ל-stream שטוח של bytes שה-kernel מפרשר חזרה ל-[`FrameCommands`](../framework/src/compositor.rs). כל op מתחיל ב-`u8` opcode:

| Opcode | Op          | Payload (little-endian)                                          |
|--------|-------------|------------------------------------------------------------------|
| `0x00` | `Clear`     | `u32 rgba`                                                       |
| `0x01` | `FillRect`  | `f32 x, f32 y, f32 w, f32 h, u32 rgba`                          |
| `0x02` | `DrawText`  | `f32 x, f32 y, f32 size, u32 rgba, u32 len, <len> UTF-8 bytes`  |
| `0x03` | `PushClip`  | `f32 x, f32 y, f32 w, f32 h`                                    |
| `0x04` | `PopClip`   | —                                                                |
| `0x05` | `PushTransform` | `f32 dx, f32 dy, f32 scale`                                  |
| `0x06` | `PopTransform`  | —                                                             |

הקודק המלא ב-[framework/src/wasm_activity.rs](../framework/src/wasm_activity.rs) — `encode_op` / `decode_frame`. תוספות (additive) יקבלו opcodes חדשים ב-continuation של הרצף; ה-decoder דוחה opcodes לא מוכרים כ-error (אי אפשר לדעת את אורך ה-payload כדי לדלג).

### 2.5.1 Intent wire format (Stone 9.4)

פורמט שטוח שה-SDK מ-encode עם `Intent::encode` וה-framework מ-decode עם `Intent::decode`. ה-blob נוסע ב-שני הכיוונים — app→kernel דרך `akiva_host_start_activity` + `hal_app_drain_intent`, ו-kernel→app דרך `hal_app_new_intent` + `akiva_app_new_intent`.

```
u32  action_len + <action_len> UTF-8 bytes
u32  category_count
  for each:  u32 len + <len> UTF-8 bytes
u8   has_data_uri       (0/1) + if 1: u32 len + bytes
u8   has_mime_type      (0/1) + if 1: u32 len + bytes
u8   has_target_package (0/1) + if 1: u32 len + bytes
u8   has_target_class   (0/1) + if 1: u32 len + bytes
u32  extras_count       (v0.2: חייב להיות 0 — reserved ל-Stone 9.5)
```

- **Max size:** 16 KiB (`Intent::MAX_WIRE_BYTES`) — בודק בשני הצדדים; blob גדול יותר → `IntentDecodeError::TooLarge`.
- **Min size:** 17 bytes (action של 0 bytes + 4 × flag=0 + extras_count=0). לכן `drain_intent` מחזיר `0` ⇒ "אין pending" באופן חד-משמעי — כל intent ממשי יהיה ≥ 17.
- **Extensibility:** `extras_count=0` הוא checkpoint. גרסת extras תוסיף payload של (`u32 key_len` + bytes + `u8 tag` + payload לפי tag). reader של גרסה ישנה שיפגוש `extras_count > 0` ידחה עם `ExtrasNotSupported` במקום להתבלבל על ה-offsets. Stone 9.5 לא נזקק ל-extras — הוא משתמש ב-`ActivityResult` נפרד (§2.5.2).
- **Identical encode on both sides:** `framework::intent::Intent::encode` ו-`sdk-rust::intent::Intent::encode` חייבים להישאר byte-for-byte symmetric — כמו ה-paint codec, כל שינוי הוא breaking event שעובר מבחני §1.2.

הקודק המלא ב-[framework/src/intent.rs](../framework/src/intent.rs) ומקבילו ב-[sdk-rust/src/intent.rs](../sdk-rust/src/intent.rs).

### 2.5.2 ActivityResult wire format (Stone 9.5)

הקטן ביותר משני פורמטי ה-wire של ה-stone: קוד `i32` + בלוב bytes אטום באורך משתנה. נוסע משני הכיוונים — app→kernel דרך `akiva_host_set_result` + `hal_app_drain_set_result`, ו-kernel→app דרך `hal_app_activity_result` + `akiva_app_activity_result`.

```
i32  code           (little-endian — RESULT_OK=0, RESULT_CANCELED=1, שאר הערכים app-defined)
u32  data_len + <data_len> bytes
```

- **Max size:** 16 KiB (`ActivityResult::MAX_WIRE_BYTES`) — אותו cap של Intent; result ממוצע של "picked=42" הוא עשרות bytes, כל מה שמעבר לזה כדאי לעבור דרך shared storage.
- **Min size:** 8 bytes (code + data_len=0). `drain_set_result` מחזיר `0` ⇒ "אין result stashed"; synthesis של `RESULT_CANCELED` עם data ריק קורה בצד ה-framework ב-`deliver_activity_result_for`.
- **Opaque data:** ה-framework לא בודק את ה-bytes. זוג caller+callee מסכמים על format (JSON, bincode, number כ-LE u32 ישיר) — אין lineage schema.
- **Identical encode on both sides:** `framework::result::ActivityResult::encode` ו-`sdk-rust::result::ActivityResult::encode` byte-for-byte symmetric. breaking-change discipline כמו Intent.

הקודק ב-[framework/src/result.rs](../framework/src/result.rs) ומקבילו ב-[sdk-rust/src/result.rs](../sdk-rust/src/result.rs).

### 2.6 שכבת הביניים ב-framework

ב-framework עצמו:

- `akiva_framework::wasm_activity::WasmHost` — trait שה-kernel מיישם (`KernelWasmHost`) דרך ה-externs למעלה; בבדיקות יחידה מיישמים `MockHost` שמצבע את הקריאות.
- `WasmActivity` — struct שמיישם את ה-`Activity` trait הפנימי, ומעביר כל callback ל-`WasmHost` עם ה-handle שלו. זו נקודת ה-bridge היחידה בין ה-framework ל-wasm apps.
- `LifecycleTransition`, `opcode::*`, `encode_op`, `decode_frame` — ה-ABI במלואו מרוכז במקום אחד.

### 2.7 מה לא כלול ב-v0

- **Imports מלאים** — רק `akiva_host_log` זמין ב-Phase 2b.3. `finish`, `start_activity`, `storage`, notifications — כל אלה ידרשו PR נפרד ויעברו את מבחני §1.2.
- **Intents** — routing בין apps מופעל ב-Phase 3.
- **Permissions** — ה-host טוען כל wasm עם ה-imports הבסיסיים בלבד. PermissionController נכנס ב-v1 כשה-imports נהיים conditional.

---

> **שאר המסמך מתאר את רובד v1 (Component Model).** הסעיפים הממוספרים שלהלן (עקרונות, WIT definitions, capability model וכו') נשארים כ-spec היעד. הם אינם מתממשים ב-Phase 2b, אבל חוזקת ה-API של §2 מעוצבת במודע להתיישר איתם.

## 3. עקרונות (v1)

1. **Component Model + WIT** — ה-ABI מוגדר דרך [WIT](https://component-model.bytecodealliance.org/design/wit.html). אין `extern "C"`, אין raw pointers ידניים, אין alloc/free ידני בצד ה-host. `wit-bindgen` מייצר את קוד ה-glue אוטומטית לכל שפה (Rust, C, Go, Python, JS).
2. **Typed resources** — widgets, activities, intents הם `resource` של WIT, לא `u32 handle`. זה נותן type safety ו-lifetime management שה-runtime אוכף.
3. **Strings, records, lists, options, results** — ברמה native של WIT. אין marshalling ידני.
4. **Version-tracked** — כל world מקבל SemVer (`akiva:app/activity@0.1.0`). breaking changes → major bump.
5. **Capability via imports** — אפליקציה מקבלת רק imports שה-runtime החליט להעניק. Permission denied = import חסר = link error, לא runtime check. ראה סעיף 6.
6. **Additive only בתוך major** — תוספות תואמות-אחורה בתוך gap של major version.

## 4. מבנה המודול

אפליקציית Akiva היא **Component** שמיישמת את ה-world `akiva:app/guest` ומייבאת את ה-world `akiva:app/host`.

```
akiva:app package:
├── world guest      ← מה שהאפליקציה חייבת לייצא (lifecycle + event dispatch)
├── world host       ← מה שה-runtime חושפת (widgets, resources, intents, storage, ...)
└── interfaces:
    ├── lifecycle
    ├── events
    ├── widget
    ├── resources
    ├── intent
    ├── storage
    ├── system
    ├── log
    └── types (shared)
```

---

## 5. WIT Definitions

### 4.1 Shared types

```wit
package akiva:app@0.1.0;

interface types {
    /// Widget identity. Stable for the lifetime of the widget in the tree.
    resource widget;

    /// An active Activity instance (distinct from the declared manifest entry).
    resource activity;

    /// An Intent — a composable message for IPC.
    resource intent;

    /// Opaque handle returned when registering an event listener; used for unregister.
    type listener-id = u32;

    /// Logical pixel value (dp). Runtime applies density.
    type dp = f32;

    /// Scale-independent pixel value (sp). Runtime applies font scale + density.
    type sp = f32;

    /// RGBA in 0xRRGGBBAA layout.
    type rgba = u32;

    /// Monotonic elapsed nanoseconds since boot. Clock source is the host.
    type mono-nanos = u64;

    /// Wall-clock milliseconds since Unix epoch.
    type wall-millis = u64;

    record dimension {
        /// Explicit value; if absent, the widget uses intrinsic sizing.
        value: option<size-value>,
    }

    variant size-value {
        dp(dp),
        percent(f32),
        match-parent,
        wrap-content,
    }

    record insets {
        top: dp,
        right: dp,
        bottom: dp,
        left: dp,
    }

    enum visibility {
        visible,
        invisible,  // takes space, not drawn
        gone,       // no space, not drawn
    }

    enum text-align {
        start,
        center,
        end,
    }

    enum event-kind {
        click,
        long-click,
        text-change,
        focus,
        blur,
        key-down,
        key-up,
        pointer-enter,
        pointer-leave,
    }

    variant event-payload {
        point(tuple<f32, f32>),       // click, pointer
        text(string),                  // text-change
        key(key-event),                // key-down/up
        none,                          // focus/blur/enter/leave
    }

    record key-event {
        code: u32,
        modifiers: u32,  // bitmask: 1=shift 2=ctrl 4=alt 8=meta
    }

    variant widget-error {
        invalid-parent,
        widget-destroyed,
        invalid-value(string),
    }

    variant resource-error {
        not-found,
        wrong-type,
    }

    variant storage-error {
        not-found,
        quota-exceeded,
        io-error(string),
    }

    variant intent-error {
        no-match,
        permission-denied,
        invalid,
    }
}
```

### 4.2 Lifecycle (exports — guest must implement)

```wit
interface lifecycle {
    use types.{activity};

    /// ABI semver the guest targets. Runtime rejects if major mismatch.
    abi-version: func() -> string;  // e.g. "0.1.0"

    on-create: func(act: borrow<activity>, saved-state: option<list<u8>>);
    on-start: func(act: borrow<activity>);
    on-resume: func(act: borrow<activity>);
    on-pause: func(act: borrow<activity>);
    on-stop: func(act: borrow<activity>);
    on-destroy: func(act: borrow<activity>);

    /// Called before on-stop. Returned bytes persist across process kills (<= 64 KiB).
    on-save-state: func(act: borrow<activity>) -> list<u8>;

    /// Config change with bitmask (see types.config-changes). Only called if the
    /// activity declared it handles these changes in manifest; otherwise recreate.
    on-config-change: func(act: borrow<activity>, changes: u32);

    /// Delivered when a new intent arrives for an already-existing instance
    /// (launch modes singleTop / singleTask).
    on-new-intent: func(act: borrow<activity>, in: intent);

    /// Result from start-activity-for-result. `data` is none if canceled.
    on-activity-result: func(
        act: borrow<activity>,
        request-code: u32,
        result-code: s32,
        data: option<intent>,
    );
}

interface events {
    use types.{activity, widget, event-kind, event-payload};

    /// Dispatched for every widget event wired via widget.set-listener.
    on-event: func(
        act: borrow<activity>,
        w: borrow<widget>,
        kind: event-kind,
        payload: event-payload,
    );
}
```

### 4.3 Widget (imports — runtime provides)

```wit
interface widget {
    use types.{widget, activity, insets, size-value, visibility,
              text-align, rgba, sp, dp, event-kind, listener-id, widget-error};

    // ---- Creation ----
    // parent: none => root of the activity's content view.
    create-vstack:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-hstack:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-grid:    func(parent: option<borrow<widget>>, cols: u32, rows: u32)
                      -> result<widget, widget-error>;
    create-frame:   func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-text:    func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-button:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-image:   func(parent: option<borrow<widget>>) -> result<widget, widget-error>;
    create-spacer:  func(parent: option<borrow<widget>>) -> result<widget, widget-error>;

    // ---- Lookup ----
    find-by-id: func(act: borrow<activity>, id: string) -> option<widget>;

    // ---- Size & spacing ----
    set-width:   func(w: borrow<widget>, v: size-value);
    set-height:  func(w: borrow<widget>, v: size-value);
    set-padding: func(w: borrow<widget>, p: insets);
    set-margin:  func(w: borrow<widget>, m: insets);
    set-flex:    func(w: borrow<widget>, v: f32);

    // ---- Color ----
    set-background-color: func(w: borrow<widget>, c: rgba);
    set-foreground-color: func(w: borrow<widget>, c: rgba);

    // ---- Visibility & state ----
    set-visibility: func(w: borrow<widget>, v: visibility);
    set-enabled:    func(w: borrow<widget>, on: bool);

    // ---- Text widget ----
    set-text:        func(w: borrow<widget>, text: string);
    set-text-size:   func(w: borrow<widget>, size: sp);
    set-text-align:  func(w: borrow<widget>, a: text-align);
    set-font-weight: func(w: borrow<widget>, weight: u32);   // 400/500/700

    // ---- Arbitrary tag (useful in click handlers) ----
    set-tag: func(w: borrow<widget>, tag: string);
    get-tag: func(w: borrow<widget>) -> option<string>;

    // ---- Tree ----
    clear-children: func(w: borrow<widget>);
    remove:         func(w: widget);                         // own handle → drop
    parent:         func(w: borrow<widget>) -> option<widget>;
    child-count:    func(w: borrow<widget>) -> u32;
    child-at:       func(w: borrow<widget>, index: u32) -> option<widget>;

    // ---- Events ----
    set-listener:    func(w: borrow<widget>, kind: event-kind) -> listener-id;
    clear-listener:  func(w: borrow<widget>, kind: event-kind);
}
```

### 4.4 Resources (imports)

```wit
interface resources {
    use types.{activity, widget, rgba, dp, resource-error};

    get-string: func(ref: string) -> result<string, resource-error>;
    get-string-fmt: func(ref: string, args: list<tuple<string, string>>)
                     -> result<string, resource-error>;
    get-color:  func(ref: string) -> result<rgba, resource-error>;
    get-dimen:  func(ref: string) -> result<dp, resource-error>;

    /// Inflate an AKML layout as a child of `parent` (root of activity if none).
    /// All @+id children become findable via widget.find-by-id.
    inflate-layout: func(
        act: borrow<activity>,
        layout-ref: string,
        parent: option<borrow<widget>>,
    ) -> result<widget, resource-error>;

    /// Drawable returned as opaque handle the Image widget understands.
    resource drawable;
    get-drawable: func(ref: string) -> result<drawable, resource-error>;
    set-image-drawable: func(w: borrow<widget>, d: borrow<drawable>);
}
```

### 4.5 Activity & Intent (imports)

```wit
interface intent {
    use types.{activity, intent, intent-error};

    // ---- Construction ----
    new: func(action: string) -> intent;
    set-data:     func(i: borrow<intent>, uri: string);
    set-mime:     func(i: borrow<intent>, mime: string);
    add-category: func(i: borrow<intent>, category: string);
    set-target:   func(i: borrow<intent>, package: string, activity: string);

    put-string: func(i: borrow<intent>, key: string, value: string);
    put-int:    func(i: borrow<intent>, key: string, value: s64);
    put-bool:   func(i: borrow<intent>, key: string, value: bool);
    put-bytes:  func(i: borrow<intent>, key: string, value: list<u8>);

    // ---- Inspection ----
    get-action:     func(i: borrow<intent>) -> string;
    get-data:       func(i: borrow<intent>) -> option<string>;
    get-mime:       func(i: borrow<intent>) -> option<string>;
    get-categories: func(i: borrow<intent>) -> list<string>;

    get-string:  func(i: borrow<intent>, key: string) -> option<string>;
    get-int:     func(i: borrow<intent>, key: string) -> option<s64>;
    get-bool:    func(i: borrow<intent>, key: string) -> option<bool>;
    get-bytes:   func(i: borrow<intent>, key: string) -> option<list<u8>>;
}

interface activity-api {
    use types.{activity, intent, intent-error};

    set-content-view: func(act: borrow<activity>, layout-ref: string);

    /// Returns an opaque request-id the runtime maps back to the caller on result.
    start-activity: func(act: borrow<activity>, i: intent) -> result<_, intent-error>;

    start-activity-for-result: func(
        act: borrow<activity>,
        i: intent,
        request-code: u32,
    ) -> result<_, intent-error>;

    finish: func(act: borrow<activity>);

    set-result: func(
        act: borrow<activity>,
        code: s32,
        data: option<intent>,
    );

    /// Current intent that started this activity (or the one last delivered
    /// via on-new-intent).
    current-intent: func(act: borrow<activity>) -> option<intent>;
}
```

### 4.6 Storage (imports) — capability gated

The storage interface is **only linked** if the app's manifest declared
`akiva.permission.STORAGE_SANDBOX` (which is auto-granted — it's the app's own
sandbox). Cross-app storage is a separate, user-granted capability.

```wit
interface storage {
    use types.{storage-error};

    /// Per-app sandboxed key-value store backed by OPFS.
    set:    func(key: string, value: list<u8>) -> result<_, storage-error>;
    get:    func(key: string) -> result<list<u8>, storage-error>;
    remove: func(key: string) -> result<_, storage-error>;
    keys:   func() -> list<string>;
    size-of: func(key: string) -> option<u64>;
}
```

### 4.7 System & logging (imports)

```wit
interface log {
    enum level { debug, info, warn, error }
    write: func(level: level, message: string);

    /// Tracing spans — handle is returned as an opaque id that must be passed
    /// to span-end. Runtime emits structured traces to the host observability sink.
    span-begin: func(name: string) -> u32;
    span-end:   func(id: u32);
}

interface system {
    use types.{mono-nanos, wall-millis};

    // ---- Device config snapshot ----
    record device-config {
        locale: string,              // BCP-47, e.g. "en-US"
        layout-direction: layout-dir,
        orientation: orientation,
        ui-mode: ui-mode,
        density: f32,                // 1.0 = 160 dpi baseline
        screen-width-dp: u32,
        screen-height-dp: u32,
    }
    enum layout-dir   { ltr, rtl }
    enum orientation  { portrait, landscape }
    enum ui-mode      { light, dark }

    current-config: func() -> device-config;

    // ---- Clock ----
    monotonic-nanos: func() -> mono-nanos;
    wall-millis:     func() -> wall-millis;

    // ---- Toasts (lightweight notifications, non-interactive) ----
    enum toast-duration { short, long }
    show-toast: func(message: string, duration: toast-duration);

    // ---- Secure RNG ----
    // Backed by the host's CSPRNG (crypto.getRandomValues). Always available.
    random-bytes: func(len: u32) -> list<u8>;
}
```

### 4.8 World composition

```wit
world guest {
    export lifecycle;
    export events;
}

world host {
    import widget;
    import resources;
    import intent;
    import activity-api;
    import storage;      // gated by capability (see section 6)
    import log;
    import system;
}
```

An Akiva app is a component that imports `host` and exports `guest`. The runtime
inspects the component's imports, validates each against the granted capability
set, and instantiates only if every import is permitted.

---

## 6. Capability-via-imports Model

Permissions in Akiva are enforced at **instantiation time**, not at call time.

- The `manifest.toml` declares which capabilities the app requires / may use.
- The PackageManager maps each capability to a concrete set of WIT imports.
- On instantiate, the runtime builds an import map containing **only** the
  imports the app is authorized to use. Calls to missing imports fail at link
  time, not with a runtime permission error.

Example mapping:

| Capability                         | Imports gated                        |
|------------------------------------|--------------------------------------|
| `akiva.permission.STORAGE_SANDBOX` | `storage` (always granted)           |
| `akiva.permission.NETWORK`         | `network` (post-MVP)                 |
| `akiva.permission.CAMERA`          | `camera` (post-MVP)                  |
| `akiva.permission.QUERY_PACKAGES`  | extra methods on `activity-api`      |

Benefits over a runtime-check model:
- **Fast**: no branch on every API call.
- **Unforgeable**: an app with no NETWORK capability cannot synthesize a network
  call by any trick — the function doesn't exist in its import namespace.
- **Auditable**: capability set == linked imports, visible in `akiva build`
  output.

---

## 7. Observability hooks

The `log` interface is always imported. Output is sinked in the host as
follows:

| Sink             | Audience        | Routing                                       |
|------------------|-----------------|-----------------------------------------------|
| Browser console  | Developer       | `log.write` at info+ ; all debug if devtools flag |
| In-app log view  | End user / QA   | Shell's "Developer → Logs" (post-MVP)         |
| Structured trace | Host tooling    | `span-begin` / `span-end` → OT-compatible events |

See [docs/observability.md](observability.md) for how the host wires this up.

---

## 8. Example — Calculator boot (conceptual)

```
Runtime:       instantiate(calc.wasm, imports={widget, resources, intent,
                                                activity-api, storage, log,
                                                system})
Runtime → App: lifecycle.on-create(act, saved-state=none)
App:           activity-api.set-content-view(act, "@layout/main")
Runtime:       (layout inflater walks AKML, calls widget.create-* internally,
                registers listeners declared via onClick="on_digit")
Runtime → App: lifecycle.on-start(act)
Runtime → App: lifecycle.on-resume(act)

User taps "7":
Tab HAL:       pointer-down(100, 300) → Worker
Runtime:       hit-test → widget w12 (button "7")
Runtime → App: events.on-event(act, w12, click, point(100.0, 300.0))
App:           widget.get-tag(w12) → "7"
App:           widget.set-text(display_widget, "7")
Runtime:       next frame re-layout + re-composite
```

---

## 9. Not in v1 ABI

- Network (`network` interface — post-MVP).
- Camera / microphone / sensors.
- Custom font loading.
- Animation driver (post-MVP; fade/slide handled by runtime transitions).
- WASI-Threads (when it stabilizes on the browser).
- Broadcast receivers (post-MVP).
- PendingIntent (post-MVP).

## 10. Versioning

- Each interface has its own SemVer. Guest declares via `abi-version()`.
- Runtime keeps a compatibility matrix: "runtime X can host guests with ABI
  0.1.0–0.1.z".
- Breaking changes require major bump; guests pin to a range in manifest
  (`min_abi_version = "0.1.0"`).
