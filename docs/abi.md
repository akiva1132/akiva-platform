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

## 1.2 ABI versioning policy (Stone 11.6.8 #5)

`ABI_VERSION` מוצהר בשני צדדים — `sdk-rust/src/lib.rs` ו-`framework/src/lib.rs` — ונבדק ב-instantiation. mismatch fails-loudly. הסעיף הזה קובע *איך ומתי* ה-version עולה, מה נחשב breaking, ומי מאשר.

### 1.2.1 In-scope של ה-ABI

הדברים שהשינוי בהם דורש version bump:
- **Extern function signatures** בשני הצדדים — `akiva_app_*` exports (kernel→app) ו-`akiva_host_*` / `hal_app_*` imports (app→host).
- **`op_kind` constants** — fire kinds, io kinds (`STORAGE_GET`, `QUERY_PACKAGES`, …), cmd kinds (CREATE, RESIZE, …).
- **Status code constants** — POLL_STATUS::PENDING/READY/ERROR, future status values, `STATUS_BUSY` (post-11.6.8 #2).
- **Wire format byte layouts** — intent encoding (§2.5), paint encoding (§2.7), saved-state, pkgs blob, result blob, capability list. שינוי field order, alignment, או prefix length הוא MAJOR.
- **Capability constants** — `akiva.permission.*` strings ב-manifest.
- **Public `MAX_*_BYTES` constants** — `Intent::MAX_WIRE_BYTES`, etc. הקטנת המגבלה היא breaking; הגדלה אדיטיבית.

### 1.2.2 NOT in-scope של ה-ABI

שינויים שלא דורשים bump:
- **SDK Rust ergonomics** — שמות פונקציות, error types, doc comments, helper signatures. `WriteHandle` קיבל `must_use` ב-Stone 11.6.7 #1 בלי bump כי החתימה ה-binary לא השתנתה.
- **Internal kernel implementation** — kernel internals, framework internal logic, `runtime/web` JS internals, scheduler tweaks, watchdog thresholds.
- **Test harness** — test fixtures, mock implementations, vitest helpers.
- **Documentation** — text in `docs/`, README, code comments, log messages.

### 1.2.3 Bump rules

**PATCH** (`0.X.Y` → `0.X.(Y+1)`): bug fix במימוש, contract לא משתנה. חסום בעידן 0.X — לא נשתמש; קיים על נייר ל-1.0+.

**MINOR** (`0.X.Y` → `0.(X+1).0`): additive change. דוגמאות:
- New op_kind מוסף לטבלה
- New extern import (apps שלא קוראות אותו ממשיכות לעבוד)
- New capability string (apps שלא דורשות אותה אינן מושפעות)
- Field appended ל-wire format שמתועד כ-"may be extended" (decoder ישן מתעלם מ-trailing bytes)
- Status code חדש (apps שלא בודקות אותו ספציפית מקבלות אותו כ-fallback ERROR)

Apps שנבנו על MINOR-N עובדות על MINOR-(N+k) ללא rebuild. Apps שנבנו על MINOR-(N+k) **לא** מובטחות לעבוד על MINOR-N (יחסר להן primitive).

**MAJOR** (pre-1.0: `0.X` → `0.(X+1).0`; post-1.0: `X.Y` → `(X+1).0.0`): breaking change. דוגמאות:
- הסרה / שינוי שם של primitive
- שינוי טיפוס של arg
- Reorder של field-ים ב-wire format
- שינוי semantics של op_kind קיים
- תוספת field ל-wire format שלא מתועד כ-extensible

Apps חייבות rebuild + retest.

### 1.2.4 Promotion ל-1.0

קריטריונים שכולם חייבים להיות ירוקים לפני `1.0.0`:
- **Phase 11 + 12 + 13 exit criteria** all met (workers, governance, WindowManager).
- **שלוש apps third-party** באקוסיסטם הציבורי השתמשו ב-ABI ושרדו `MINOR` bump אחד לפחות בלי breakage.
- **WIT migration plan** מוגדר בכתב (לא ביצוע — תכנון מחייב).
- **CHANGELOG.md** מקיף עם נימוק לכל MAJOR / MINOR מאז `0.1.0`.
- **Public docs** ב-`docs/abi.md` סומנו כ"frozen surface" (לא draft).

עד ש-1.0 נופל, MAJOR bumps ב-0.X חופשיים יחסית — אבל כל אחד דורש `CHANGELOG.md` entry ו-PR review מפורש.

### 1.2.5 Review process

- **MINOR / PATCH**: PR אחד; reviewer יחיד; checklist של "הוספתי entry ב-CHANGELOG, הברתי `ABI_VERSION` בשני הצדדים".
- **MAJOR (pre-1.0)**: PR + design memo קצר ב-`docs/research-*.md` שמסביר למה ה-old shape שגוי. ה-CHANGELOG entry חייב לציין break-compat path.
- **MAJOR (post-1.0)**: דורש 2 reviewer-im ו-30-day announcement window לפני merge ל-`main`. ה-window מאפשר ל-third-party apps להגיב ולשנות planning.

### 1.2.6 Symbolic source of truth

מערכת מקור-אמת לערך הנוכחי של `ABI_VERSION`:

```
sdk-rust/src/lib.rs::ABI_VERSION  ←  קנוני
framework/src/lib.rs::ABI_VERSION ←  חייב להיות שווה; mismatch = panic ב-load
CHANGELOG.md                       ←  history של כל bump עם הסבר
docs/abi.md (this section)         ←  policy של איך ומתי לעלות
```

שינוי של `ABI_VERSION` שלא מתועד ב-CHANGELOG חייב להיתפס ב-CI (TODO: tooling — Stone 11.8 או 12).

---

## 1.3 עיקרון: ABI narrow-by-design

ABI v0 קטן בכוונה. זה לא accident; זה policy. ABI רחב מדי מוקדם מדי הופך לעוגן שמאט כל שינוי עתידי, ומכתיב decisions שכבת framework לעוד שנים.

כל הרחבה ל-ABI — בין אם export חדש ב-app, import חדש ב-host, או שדה חדש ב-wire format — חייבת לעבור על שלושת המבחנים:

1. **Use case מוכח.** לפחות reference app אחד שבלי ההרחבה לא פועל בצורה סבירה. "might be useful later" לא מספיק.
2. **Cross-app justification.** הרחבה שמשרתת app בודד או use case נישתי שייכת ל-framework או ל-sdk, לא ל-ABI. ABI משרת רק את הדברים שכל app, או runtime core, חייבים.
3. **Forward-compat ל-v1.** יש mapping ברור ל-WIT interface ב-§5. אם ההרחבה לא מתורגמת לצורה נקייה ב-Component Model, זה תמרור אזהרה שה-shape שלה לא נכון.

הכלל המעשי: עדיף פונקציה אחת חסרה היום מאשר שבע פונקציות מיותרות שנצטרך לגרור לנצח. הרחבות נוספות ל-`hal_app_*` או ל-paint-op opcodes עוברות review מפורש ב-PR, ומתועדות ב-CHANGELOG (§1.2.6) ובסעיף גרסאות (§10).

---

## 2. App ABI v0 — raw exports (Phase 2b)

### 2.1 מודל ריצה

כל app הוא wasm module עצמאי שה-host (worker.js) טוען לצד ה-kernel wasm. ה-kernel **לא** מזהה את האפליקציה ב-address space שלו — הוא שולח את הבקשות ל-host דרך imports בשם `hal_app_*`, וה-host מנתב אותן ל-instance הנכון של ה-app. יש לכך יתרונות מיידיים: isolation בין apps, memory growth עצמאי, crash containment.

```
  Kernel wasm           Worker JS host             App wasm (one per package)
  ───────────           ──────────────             ─────────────────────────
  WasmActivity   ──►    hal_app_dispatch    ──►    akiva_app_dispatch(cmd, payload)
  (framework)           (routes by handle)          (app-side)
                  ◄──   bytes via               ◄── akiva_app_get_result → *u8
                        hal_app_get_result
```

ה-`handle` הוא `u32` לא-אפסי שה-host מקצה ב-`hal_app_register` ושומר בו את ה-tuple `(package_id, class, wasm_instance)`. ל-kernel אין מושג מה ה-handle מייצג.

### 2.2 Exports שה-app חייב לספק

כל app חייב לייצא את ה-wasm `memory` (מופעל אוטומטית ע"י rustc ל-`wasm32-unknown-unknown`) ואת הפונקציות הבאות (C ABI). `runtime/web/app_registry.js::REQUIRED_EXPORTS` בודק את הרשימה בזמן טעינה; apps שחסרים משהו נדחים לפני הקריאה הראשונה למחזור חיים.

#### 2.2.1 Memory ABI (always required)

```rust
// ה-host משתמש בכדי להזריק data ל-app (intents, saved state, dispatch payloads).
extern "C" fn akiva_app_alloc(size: u32) -> *mut u8;
extern "C" fn akiva_app_dealloc(ptr: *mut u8, size: u32);

// ABI version — null-terminated UTF-8 ("0.1.0").
extern "C" fn akiva_app_abi_version() -> *const u8;
```

#### 2.2.2 Unified dispatch entry (P0 #4 async-resumable)

ה-Exports שהוצגו ב-Stone 11.4b הפכו ב-P0 #4 (May 2026) ל-async-resumable. שלושה exports מחליפים את הזוג הקודם:

```rust
// Hand-off into the SDK runtime. dispatch_id is allocated by the
// host (the AppRegistry's per-package monotonic counter) and is the
// stable handle the host uses for all follow-up calls related to
// this dispatch (poll_dispatch + get_result). Every payload begins
// with [handle: u32 LE, ...] — the cmd_kind table at §2.2 lists the
// per-cmd remainder.
//
// Returns a status code:
//   0  — accepted; host should drive to terminal status via
//        poll_dispatch.
//   !=0 — hard refusal (bad payload, unknown cmd_kind, OOM during
//        slot construction). The host treats it as ack-only and
//        does NOT poll.
extern "C" fn akiva_app_dispatch(
    dispatch_id: u64,
    cmd_kind: u32,
    payload_ptr: *const u8,
    payload_len: u32,
) -> u32;

// Drive a dispatch one poll forward. Returns packed (status, len):
//   * status (low 32 bits):
//       0 PENDING — wasm is parked on a host_op; host should call
//                   again after `host_op_done` arrives at the shell.
//       1 READY   — bytes (if any) available via get_result.
//       2 ERROR   — future trapped, wire bug, or unknown dispatch_id.
//   * len (high 32 bits): byte count for READY/ERROR; 0 for
//     PENDING and ack-only dispatches.
extern "C" fn akiva_app_poll_dispatch(dispatch_id: u64) -> u64;

// Two-phase fetch of the result bytes for a READY dispatch.
// `*out_len_ptr` receives the length; returned pointer + length are
// valid until this single call evicts the dispatch_id (single-shot).
// For ack-only dispatches (lifecycle, input, intent, etc.) the
// pointer may be null and length 0.
//
// DESTROY is special: get_result also tears down the slot once the
// bytes are drained (which are always 0 for destroy). This delays
// slot teardown until AFTER any pending future has dropped its
// borrows into the slot.
extern "C" fn akiva_app_get_result(
    dispatch_id: u64,
    out_len_ptr: *mut u32,
) -> *const u8;
```

`cmd_kind` הוא integer יציב מ-`framework::wire::op_kinds::cmd::*` (mirror ב-`akiva_sdk::op_kinds::cmd::*` וב-`runtime/web/op_kinds.mjs::CMD`). הטווחים מקובעים לעולם:

| Range          | קטגוריה   | Ops                                                       |
|----------------|-----------|-----------------------------------------------------------|
| `0x01..=0x0F`  | Lifecycle | CREATE / START / RESUME / PAUSE / STOP / DESTROY          |
| `0x10..=0x1F`  | Input     | POINTER / KEY / RESIZE                                    |
| `0x20..=0x2F`  | Render    | RENDER                                                    |
| `0x30..=0x3F`  | State     | SAVE_STATE / RESTORE_STATE                                |
| `0x40..=0x4F`  | Intent    | NEW_INTENT / ACTIVITY_RESULT                              |
| `0x50..=`      | reserved  | streams / pushed events (Stone 11.5+)                     |

Payload layout פר-cmd (כל אחד מתחיל ב-`[handle: u32 LE, ...]`):

| cmd_kind          | payload after handle                                                     |
|-------------------|--------------------------------------------------------------------------|
| CREATE            | `class: utf8` (host-augmented — kernel passes only `[handle]`)           |
| START/RESUME/PAUSE/STOP/DESTROY | אין                                                        |
| POINTER           | `x: f32, y: f32, button: u32, phase: u32, ts: f64` (24 bytes)            |
| KEY               | `code: u32, modifiers: u32, phase: u32, ts: f64` (20 bytes)              |
| RESIZE            | `width: u32, height: u32, density: f32` (12 bytes)                       |
| RENDER            | `ts: f64` (8 bytes); result = paint-op stream                            |
| SAVE_STATE        | אין; result = `SavedState::encode` bytes                                 |
| RESTORE_STATE     | `state: bytes`                                                           |
| NEW_INTENT        | `intent: encoded Intent`                                                 |
| ACTIVITY_RESULT   | `request_code: u32, result: encoded ActivityResult`                      |

`dispatch_id` הוא monotonic counter ש-AppRegistry מקצה כל פעם ש-`hal_app_dispatch` נקרא. ה-id משמש כמפתח גם ב-host-side `_dispatches` map וגם ב-app-side per-handle pending registry — שני הצדדים מסכימים על אותו ערך, מה שעושה את ה-debugging trivial (אותו int במסד נתונים אחד).

ה-buffer שמוחזר מ-`akiva_app_get_result` הוא app-owned ותקף עד הקריאה עצמה תופסת אותו (single-shot consume). למשל ל-RENDER, ה-paint buffer reused על-כל-handle; ה-pointer שמוחזר עתה תקף עד ה-RENDER הבא על אותו handle. ל-SAVE_STATE/ack-only ה-buffer-lifetime זהה.

#### 2.2.3 Async-resumable dispatch — pending → host_op_done → re-poll

P0 #4 (May 2026) שינה את ה-protocol מ-"sync-or-handle" ל-async-resumable כדי לתמוך ב-`storage::read().await` באופן שקוף:

1. App-worker shell מקבל `{type:"dispatch", dispatch_id, cmd_kind, payload}` מ-AppRegistry.
2. Shell מעתיק את ה-payload לזיכרון של ה-wasm, קורא `akiva_app_dispatch(dispatch_id, cmd, ptr, len)`. ה-runtime ב-wasm בונה future, polls פעם אחת.
3. Shell קורא `akiva_app_poll_dispatch(dispatch_id)`:
   * **READY** — שולף bytes (אם יש) דרך `get_result`, שולח `{type:"dispatch_done", dispatch_id, bytes}` ל-AppRegistry.
   * **PENDING** — שומר ב-`pendingDispatches` Map, **לא** שולח dispatch_done.
   * **ERROR** — שולח dispatch_done עם null bytes; trapped notice אופציונלי.
4. עבור pending dispatches: כש-`host_op_done` מגיע ל-shell (storage read resolved, network reply, וכו'), shell עושה re-poll על **כל** ה-pending dispatches. כל אחד מהם יכול להיות parked על ה-future הזה. זה במובן מ-O(n) iteration — אבל n ≤ 1 בפועל (framework allows רק dispatch אחד per handle).
5. כשה-poll מחזיר READY, הflow ב-§3 ממשיך כרגיל.

ה-AppRegistry על הצד של ה-kernel לא מחליף — הוא ממשיך לראות `dispatch_id` יוצא ו-`dispatch_done` חוזר, איפה שהוא רק חיכה יותר זמן. ה-kernel polling דרך `hal_app_poll_future` ממשיך לראות PENDING עד שה-shell שולח dispatch_done.

### 2.3 Imports (host → app)

ה-host מייצא ל-wasm של האפליקציה imports תחת module name `env`. נכון ל-Stone 11.3 ה-import surface מכוון ל-**unified ABI** של שלוש פונקציות multiplexer + ארבע פונקציות resource lookup. כל פעולה host-mediated עוברת דרך `akiva_host_fire` (fire-and-forget), `akiva_host_io_start` + `akiva_host_io_poll` (async עם future ID), או דרך אחת מ-resource lookup הקלים. ה-mapping ל-`op_kind` הקבוע מתועד ב-§2.3.2.

#### 2.3.1 Resource lookups (legacy, נשארים)

ארבע פונקציות readonly per-package שלא משתלבות יפה ב-fire/io shape (אין side effect, אין handle context, אין async). מועברות אוטומטית בלי gating:

```rust
// Resolve @color/name for the calling package. Returns 0xAARRGGBB.
// Undefined names return 0xFFFF00FF (magenta) and log a host-side
// warning — missing refs surface visually, not silently.
extern "C" fn akiva_host_resource_color(name_ptr: *const u8, name_len: u32) -> u32;

// Resolve @string/name. Writes UTF-8 bytes into [out_ptr, out_ptr+out_cap)
// and returns the *actual* length. If the returned length exceeds
// out_cap, nothing was written — caller reallocates to that size and
// calls again. Undefined names write "⟨missing:NAME⟩".
extern "C" fn akiva_host_resource_string(
    name_ptr: *const u8, name_len: u32,
    out_ptr: *mut u8, out_cap: u32,
) -> u32;

// Probes: 1 if the ref is registered, 0 otherwise.
extern "C" fn akiva_host_resource_has_color(name_ptr: *const u8, name_len: u32) -> u32;
extern "C" fn akiva_host_resource_has_string(name_ptr: *const u8, name_len: u32) -> u32;

// Stone 13.0prep C — resolve @drawable/name to a stable per-package
// drawable_id. The host built the index at install / boot time by
// scanning res/drawable/*.png and decoding each PNG to an ImageBitmap;
// this lookup is an O(1) Map fetch. Id 0 is the missing-drawable
// sentinel (renderer paints a magenta placeholder).
extern "C" fn akiva_host_resource_drawable(name_ptr: *const u8, name_len: u32) -> u32;
```

#### 2.3.2 Unified ABI multiplexers (Stone 11.3)

שלושה imports שמעבירים את כל הפעולות host-mediated (log, finish, start_activity, set_result, text_width, query_packages, storage…) דרך ה-`op_kind` integer. ה-args layout לכל op_kind ידוע ב-side של ה-SDK ושל ה-host; ה-encoder/decoder מוצמד 1:1 בין `framework/src/wire/op_kinds.rs` (kernel side) ל-`sdk-rust/src/op_kinds.rs` (app side, mirror) — מבחן unit cross-crate מאמת equality.

```rust
// Fire-and-forget — ה-host מבצע את הפעולה ולא מחזיר נתונים. ערך החזרה:
// 0 = הצליח, !=0 = שגיאה (op_kind לא ידוע, args malformed, capability
// not granted). אופייני ל-LOG, FINISH, START_ACTIVITY, START_ACTIVITY
// _FOR_RESULT, SET_RESULT.
extern "C" fn akiva_host_fire(
    op_kind: u32,
    args_ptr: *const u8,
    args_len: u32,
) -> u32;

// Async start — ה-host מקצה future_id ומתחיל לעבד. ערך החזרה: future_id
// non-zero בהצלחה, 0n בכישלון (op_kind לא ידוע, capability not granted,
// args malformed). אופייני ל-TEXT_WIDTH, QUERY_PACKAGES, STORAGE_GET/SET.
extern "C" fn akiva_host_io_start(
    op_kind: u32,
    args_ptr: *const u8,
    args_len: u32,
) -> u64;

// Poll עד שה-future מתממש. ערך החזרה packed (status: u32 low, length:
// u32 high). status: 0=PENDING (לא מוכן עדיין, עוד לא חוקי ב-11.3
// single-worker אבל יקרה ב-11.4+), 1=READY (length bytes נכתבו ל-out_ptr),
// 2=ERROR. אם length > out_cap — שום דבר לא נכתב; ה-caller מקצה buffer
// בגודל הנדרש ומנסה שוב (two-phase contract — ה-future נשמר עד שהקריאה
// מצליחה). אחרי קריאה מוצלחת ה-future evicted מה-registry.
extern "C" fn akiva_host_io_poll(
    future_id: u64,
    out_ptr: *mut u8,
    out_cap: u32,
) -> u64;
```

**op_kind layout** (פרטים מלאים ב-`framework/src/wire/op_kinds.rs`):

| Range            | Transport       | קבועים                                                                                              |
|------------------|-----------------|-----------------------------------------------------------------------------------------------------|
| `0x0001..0x00FF` | `fire`          | `LOG=0x01`, `FINISH=0x02`, `START_ACTIVITY=0x03`, `START_ACTIVITY_FOR_RESULT=0x04`, `SET_RESULT=0x05` |
| `0x0101..0x01FF` | `io_start`/poll | `TEXT_WIDTH=0x101`, `QUERY_PACKAGES=0x102`, `STORAGE_GET=0x103`, `STORAGE_SET=0x104`, `STORAGE_DELETE=0x105`, `TIMER_SLEEP=0x106` (Stone 13.0prep A), `SHOW_DIALOG=0x107` (Stone 13.2) |
| `0x0200..`       | reserved        | streams / pushed events ל-Stone 11.4+                                                              |

Args layout per op_kind (LE byte-stream — encoded inline ב-dispatcher):

| op_kind                       | args                                                       | result                                  |
|-------------------------------|-----------------------------------------------------------|-----------------------------------------|
| `FIRE::LOG`                   | `[level: u32 LE \| msg_bytes…]`                            | —                                       |
| `FIRE::FINISH`                | `[handle: u32 LE]`                                         | —                                       |
| `FIRE::START_ACTIVITY`        | `[handle: u32 LE \| intent_bytes…]`                        | —                                       |
| `FIRE::START_ACTIVITY_FOR_RESULT` | `[handle: u32 LE \| request_code: u32 LE \| intent_bytes…]` | —                                       |
| `FIRE::SET_RESULT`            | `[handle: u32 LE \| result_bytes…]`                        | —                                       |
| `IO::TEXT_WIDTH`              | `[size: f32 LE \| text_bytes…]`                            | `[width: f32 LE]`                       |
| `IO::QUERY_PACKAGES`          | `[]`                                                       | `pkgs_wire blob` (ראה `wire/pkgs.rs`)    |
| `IO::STORAGE_GET`             | TBD (Stone 11.6)                                           | TBD                                     |
| `IO::STORAGE_SET`             | TBD (Stone 11.6)                                           | TBD                                     |
| `IO::TIMER_SLEEP`             | `[delay_ms: u32 LE]`                                       | `[]` (READY after delay)                |
| `IO::SHOW_DIALOG`             | `[handle: u32 LE \| title_len: u16 LE \| title: utf8… \| body_len: u16 LE \| body: utf8… \| btn_count: u8 \| (btn_len: u16 LE \| btn: utf8…)*]` | `[kind: u8, btn_idx: u8]` — `kind == 0` → `Button(btn_idx)`, `kind == 1` → `Dismissed` |

**Capability gating moved into the dispatcher.** עד Stone 10.3b ה-gating קרה בזמן instantiate (LinkError על import חסר). מאז Stone 11.3 ה-gating קורה per-call: ה-import עצמו תמיד מותקן, אבל ה-dispatcher בודק את ה-grants לפני שהוא מבצע op_kind שדורש capability. App ללא הרשאה → return code שגיאה (1 או 2 לפי הנתיב), warning בלוג. הצדק לבחירה: import אחד משרת ~250 op_kinds, חלקם auto חלקם gated; מסננים ב-call site, לא ב-link.

**Exports שה-app חושף עבור delivery של intent / result** (אלה לא מולטיפלקסר — נשארים separate exports):

```rust
// Stone 9.4 — SingleTop / SingleTask / SingleInstance reuse fires this
// with the fresh intent. handle מזהה את ה-activity הקיימת; ptr/len —
// blob של `Intent::encode` שה-host מעתיק לתוך app memory לפני הקריאה
// (symmetric עם `akiva_app_restore_state`). ה-activity נשארת `Resumed`
// ולא עוברת דרך on_create/on_start/on_resume.
extern "C" fn akiva_app_new_intent(handle: u32, ptr: *const u8, len: u32);

// Stone 9.5 — caller sees — result hand-off from a for-result launch.
// bytes encode an ActivityResult (see §2.5.2). Fires AFTER the callee's
// on_destroy, BEFORE the caller's Paused→Resumed edge (Android parity).
extern "C" fn akiva_app_activity_result(
    handle: u32, request_code: u32, ptr: *const u8, len: u32,
);
```

**ב-v1 (Component Model)** כל ה-op_kinds ימופו ל-WIT interfaces: `LOG` → `log.write`, `TEXT_WIDTH` → `text.measure-width`, `START_ACTIVITY` → `activity-api.start-activity`, וכו'. ה-multiplexers נעלמים — אבל ה-sematics זהות (futures, fire-and-forget, status codes) כך שה-translation layer נקייה.

#### 2.3.3 Capability gating (Stone 10.3 → 11.3)

ה-`PermissionController` מקצה ל-package הרשאות ב-install time וה-framework דוחף אותן ל-host דרך `hal_host_set_grants`. ה-Rust-side source of truth ב-[`framework/src/permissions.rs`](../framework/src/permissions.rs) (`AUTO_IMPORTS` + `imports_for_capability`) ומקבילו ב-`runtime/web/app_registry.js` (`AUTO_IMPORTS` + `CAPABILITY_IMPORTS`); מבחני vitest + Rust מאמתים את ה-symmetry.

| Capability                          | Imports mounted at link time           | op_kinds gated at dispatch time           |
|-------------------------------------|----------------------------------------|-------------------------------------------|
| `auto` (ללא הצהרה)                  | 4 resource lookups + 3 multiplexers    | `FIRE::LOG`, `FIRE::FINISH`, `FIRE::SET_RESULT`, `IO::TEXT_WIDTH` |
| `akiva.permission.LAUNCH_ANY_APP`   | —                                      | `FIRE::START_ACTIVITY`, `FIRE::START_ACTIVITY_FOR_RESULT` |
| `akiva.permission.QUERY_PACKAGES`   | —                                      | `IO::QUERY_PACKAGES`                      |
| `akiva.permission.STORAGE_SANDBOX`  | —                                      | `IO::STORAGE_GET`, `IO::STORAGE_SET` (Stone 11.6 wires) |
| `akiva.permission.NOTIFICATIONS`    | —                                      | *(reserved — post-MVP)*                   |
| `akiva.permission.NETWORK`          | —                                      | *(reserved — post-MVP)*                   |
| `akiva.permission.CAMERA`           | —                                      | *(reserved — post-MVP)*                   |

**שני שלבי האכיפה:**

1. **Link time** — ה-import set מוגבל ל-7 פונקציות (4 resource + 3 multiplexers). ה-`AUTO_IMPORTS` הוא הסט שכל app מקבל; שום capability היום לא מוסיפה imports נוספים. אם wasm app מנסה לייבא שם שלא נמצא → `LinkError` ב-instantiation. זה תופס שמות שגויים מאחר ש-(אסור) או imports שכבר לא קיימים אחרי refactor.

2. **Dispatch time** — בתוך `akiva_host_fire` / `akiva_host_io_start` ה-dispatcher בודק את ה-grants לפני ביצוע op_kind שצריך capability. App ללא הרשאה → `fire` מחזיר `2` (denied), `io_start` מחזיר `0n` (couldn't start). ה-warning בלוג מאפשר למפתח לראות את ה-deny.

**הצדק לחלוקה.** עד 11.3 ה-gating היה בלעדית link-time (כל capability מיפתה לקבוצת imports); אחרי 11.3 import אחד משרת ~250 op_kinds, רובם auto, חלקם gated. gating ב-link time היה מצריך מולטיפלקסר נפרד לכל capability — מסבך את ה-ABI מבלי להשיג שום דבר. ה-dispatch-time gating משאיר את ה-import surface קטן (7 ערכים) ומרכז את הלוגיקה במקום אחד שקל לבדוק.

### 2.4 Imports (kernel → host) — `hal_app_*`

אלה imports שה-kernel wasm מבקש; ה-host (worker.js) מספק. מוגדרים ב-[kernel/src/host.rs](../kernel/src/host.rs):

```rust
fn hal_app_register(
    package_id_ptr: *const u8, package_id_len: usize,
    class_ptr: *const u8,      class_len: usize,
) -> u32;  // 0 = unknown package/class
fn hal_app_unregister(handle: u32);

// ── Unified host→app dispatch (Stone 11.4b) ───────────────────────
//
// `hal_app_dispatch` is what the framework's `KernelWasmHost` calls
// for every per-callback transition (lifecycle, input, render, state,
// intent). The JS host reads `[handle: u32 LE, ...]` from the payload,
// looks up the wasm process that owns the handle, copies the payload
// into app memory, and forwards to the app's `akiva_app_dispatch`.
// CREATE special case: kernel passes only `[handle]`; JS host augments
// with the slot's class string before forwarding.
//
// Returns the app's u64 future_id:
//   0           — sync, no result bytes (every ack-only cmd_kind).
//   non-zero    — sync, bytes available; kernel calls
//                 `hal_app_get_result(future_id, ...)` next.
fn hal_app_dispatch(
    cmd_kind: u32, payload_ptr: *const u8, payload_len: u32,
) -> u64;

// Companion to `hal_app_dispatch`: fetch the bytes for a non-zero
// future_id (RENDER paint stream, SAVE_STATE encoded state). Two-phase
// buffer contract: returns the *needed* length; truncated copies
// trigger a retry with a larger buffer.
fn hal_app_get_result(
    future_id: u64, out_ptr: *mut u8, out_cap: u32,
) -> u32;

// Stone 9.3 — read + clear the per-handle "finish requested" flag the
// app raised via `akiva_host_fire(FIRE::FINISH, …)`. Called by
// `WasmActivity` at the end of every ctx-bearing callback; return 1
// flips `ActivityContext.finish_requested = true` in the active
// dispatch.
fn hal_app_drain_finish(handle: u32) -> u32;

// Stone 9.4 — read + clear the per-handle pending intent blob staged
// via `akiva_host_fire(FIRE::START_ACTIVITY, …)`. Two-phase buffer:
// the return value is the total encoded size; if it exceeds `out_cap`
// nothing is copied and the blob stays armed so the kernel can retry
// with a larger buffer. Zero means "nothing pending" — distinct from
// "empty intent" (which can't happen; the encoder's minimum is 17
// bytes).
fn hal_app_drain_intent(handle: u32, out_ptr: *mut u8, out_cap: u32) -> u32;

// Stone 9.5 — two drain primitives matching the Stone 9.4 shapes.
// (Host→app delivery for ACTIVITY_RESULT goes through `hal_app_dispatch`
// with cmd_kind = ACTIVITY_RESULT; no dedicated import.)
//
// Drain a pending start_activity_for_result request. Two-phase buffer:
// returns total size of the encoded Intent. `rc_out` is always written
// with the request_code on a successful probe — so a retry (when
// `needed > out_cap`) finds the same value. Zero return = nothing
// pending.
fn hal_app_drain_start_for_result(
    handle: u32, out_ptr: *mut u8, out_cap: u32, rc_out: *mut u32,
) -> u32;

// Drain whatever bytes the app stashed via
// `akiva_host_fire(FIRE::SET_RESULT, …)`. Same two-phase contract;
// payload is an encoded ActivityResult (see §2.5.2). Zero return =
// nothing stashed.
fn hal_app_drain_set_result(handle: u32, out_ptr: *mut u8, out_cap: u32) -> u32;
```

### 2.5 Paint-op wire format

ה-app מ-serialize את פקודות הציור שלו ל-stream שטוח של bytes שה-kernel מפרשר חזרה ל-[`FrameCommands`](../framework/src/compositor.rs). כל op מתחיל ב-`u8` opcode:

| Opcode | Op              | Payload (little-endian)                                          |
|--------|-----------------|------------------------------------------------------------------|
| `0x00` | `Clear`         | `u32 rgba`                                                       |
| `0x01` | `FillRect`      | `f32 x, f32 y, f32 w, f32 h, u32 rgba`                           |
| `0x02` | `DrawText`      | `f32 x, f32 y, f32 size, u32 rgba, u32 len, <len> UTF-8 bytes`   |
| `0x03` | `PushClip`      | `f32 x, f32 y, f32 w, f32 h`                                     |
| `0x04` | `PopClip`       | —                                                                |
| `0x05` | `PushTransform` | `f32 dx, f32 dy, f32 scale`                                      |
| `0x06` | `PopTransform`  | —                                                                |
| `0x07` | `FillRoundRect` | `f32 x, f32 y, f32 w, f32 h, f32 radius, u32 rgba` *(Stone 11.6.9 #1)* |
| `0x08` | `StrokeRect`    | `f32 x, f32 y, f32 w, f32 h, f32 width, u32 rgba` *(Stone 13.0prep B)* |
| `0x09` | `StrokeRoundRect` | `f32 x, f32 y, f32 w, f32 h, f32 radius, f32 width, u32 rgba` *(Stone 13.0prep B)* |
| `0x0A` | `DrawLine`      | `f32 x0, f32 y0, f32 x1, f32 y1, f32 width, u32 rgba` *(Stone 13.0prep B)* |
| `0x0B` | `DrawImage`     | `u32 drawable_id, f32 x, f32 y, f32 w, f32 h` *(Stone 13.0prep C)* — `drawable_id` from `akiva_host_resource_drawable`; `0` paints magenta placeholder |

הקודק המלא ב-[framework/src/wasm_activity.rs](../framework/src/wasm_activity.rs) — `encode_op` / `decode_frame`. תוספות (additive) יקבלו opcodes חדשים ב-continuation של הרצף; ה-decoder דוחה opcodes לא מוכרים כ-error (אי אפשר לדעת את אורך ה-payload כדי לדלג).

### 2.5.1 Intent wire format (Stone 9.4)

פורמט שטוח שה-SDK מ-encode עם `Intent::encode` וה-framework מ-decode עם `Intent::decode`. ה-blob נוסע ב-שני הכיוונים — app→kernel דרך `akiva_host_fire(FIRE::START_ACTIVITY, …)` + `hal_app_drain_intent`, ו-kernel→app דרך `hal_app_new_intent` + `akiva_app_new_intent`.

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

הקטן ביותר משני פורמטי ה-wire של ה-stone: קוד `i32` + בלוב bytes אטום באורך משתנה. נוסע משני הכיוונים — app→kernel דרך `akiva_host_fire(FIRE::SET_RESULT, …)` + `hal_app_drain_set_result`, ו-kernel→app דרך `hal_app_activity_result` + `akiva_app_activity_result`.

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

- **Storage** — `IO::STORAGE_GET` ו-`IO::STORAGE_SET` יש להם op_kind מוקצה, אבל ה-handler עדיין מחזיר ERROR (Stone 11.6 ימפה אל OPFS readonly handles ב-app-worker + write-through דרך ה-kernel). **השם וה-args layout נמצאים בתהליך תכנון** — אל תסתמך עליהם עד 11.6.
- **Notifications** — אין op_kind עדיין; ה-capability `akiva.permission.NOTIFICATIONS` reserved.
- **Network / Camera** — אין op_kind, אין HAL ב-host. post-MVP, יחתכו את §2.7 כשהם נוחתים.
- **Streams / pushed events** — range `0x0200..` reserved; ה-shape ייעקב ב-Stone 11.4+ (worker-per-app מאפשר push events מ-host ל-app בלי polling).

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
