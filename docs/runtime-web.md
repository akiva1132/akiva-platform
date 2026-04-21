# runtime/web — JS HAL

המסמך הזה מתעד את שכבת ה-JS שמחזיקה את Akiva בדפדפן: מה רץ ב-main thread, מה רץ ב-worker, איך מתרחשת בחירת ה-leader, ואיך event נע מהדפדפן אל תוך ה-kernel ובחזרה אל ה-canvas.

## 1. מבנה

```
runtime/web/
├── index.html          # מעטפת DOM: canvas + boot banner + a11y overlay
├── tab.js              # HAL של ה-tab (main thread): canvas, input, role handling
├── leader.js           # Session — election + transport
├── worker.js           # Dedicated Worker: מארח kernel wasm, מריץ frame loop
└── renderer/
    └── canvas2d.js     # Backend של RendererBackend מעל OffscreenCanvas 2D
```

ה-kernel wasm (`akiva-kernel.wasm`) נבנה מה-crate `kernel/` ומועתק לתוך `runtime/web/` לפני הרצה. (ב-Phase 2 ה-CLI יטפל בזה אוטומטית.)

## 2. תפקידים: main thread מול worker

| רכיב | Main thread (tab.js) | Worker (worker.js) |
|---|---|---|
| Canvas | בעלים עד לבחירת leader, ואז מעביר ל-worker | Receives `OffscreenCanvas` דרך `transferControlToOffscreen` |
| DOM events | לוכד `pointerdown/move/up/cancel`, `keydown/up`, `visibilitychange`, `resize` | לא ניגש ל-DOM |
| רינדור | **אף פעם לא מצייר** (אחרי העברה) | כל הציור דרך RendererBackend |
| Kernel wasm | לא מריץ | מריץ — מנהל boot, frame loop, events |
| BroadcastChannel | כן — ל-leader election ותקשורת cross-tab | לא |
| Web Lock | כן — מחזיק `akiva.os.leader` כשהוא leader | לא |

הכלל: main thread הוא שכבת I/O דקה מעל canvas. ה-worker הוא המקום היחיד שבו רץ kernel wasm — זה נחוץ גם לבידוד הביצועים וגם כדי שרק tab אחד בעולם ירוץ kernel אקטיבי.

## 3. פרוטוקול בחירת leader

ב-`leader.js` מוגדרת מחלקת `Session`. כל tab יוצר Session אחד בעת עלייה.

### 3.1 מצבי Session

```
electing ──┬──► leader  (ישירות, אם הוא היחיד או שה-lock פנוי מיד)
           │
           └──► follower ──► leader  (אם ה-leader הקודם מת; זה promotion)
```

`leader` הוא מצב סופי — אין demotion. מרגע שה-tab הפך ל-leader הוא מחזיק את ה-kernel עד לסגירה.

### 3.2 האלגוריתם

ב-`Session.start()` שני זרמים רצים במקביל:

```
navigator.locks.request("akiva.os.leader", exclusive, callback)
   │
   └── callback רץ ברגע שה-lock זמין. הוא קורא ל-_becomeLeader()
       ואז ממתין לנצח (keeping the lock until tab close).

BroadcastChannel probe
   │
   └── שולח {type:"probe", probeId}
       ממתין LOCK_WAIT_SOFT_MS (25ms) לראות אם callback ה-lock רץ
         ─► אם כן: אנחנו leader, סיימנו.
         ─► אם לא: ממתין PROBE_TIMEOUT_MS (150ms) לתשובה "leader_here"
             ─► תשובה חיובית:   _becomeFollower().
             ─► timeout:          ממתין ל-lock שיגיע (race edge case).
```

ה-Lock request נשאר בתור FIFO ברקע לכל טאב ששלח אותו. כשה-leader מת (הטאב נסגר או קורס), הדפדפן משחרר את ה-lock וה-callback הבא בתור יקרא אוטומטית — ה-follower הבא מקודם ל-leader **בלי פולינג או timeouts ידניים**.

### 3.3 למה שני ערוצים

- ה-**Web Lock** הוא האמת היחידה לגבי leadership. הוא atomic ומנוהל בדפדפן.
- ה-**BroadcastChannel** הוא transport — גם לבחירה המהירה (probe) וגם להעברת הודעות cross-tab בזמן ריצה.

אילו היינו משתמשים רק ב-lock, ה-followers לא היו יכולים לתקשר עם ה-leader. אילו היינו משתמשים רק ב-channel, לא היה לנו failover דטרמיניסטי.

### 3.4 Failover — רצף עובדות

1. Leader-A מחזיק את ה-lock. Follower-B ו-Follower-C בתור.
2. משתמש סוגר את tab A. הדפדפן משחרר את ה-lock.
3. ה-callback של Follower-B רץ. `_becomeLeader()` נקרא:
   - מרים `Worker`, טוען `akiva-kernel.wasm`.
   - קורא ל-`akiva_boot()` → `akiva_frame()` חדש.
   - מאזין ל-`follower_msg` מה-channel ומעביר ל-worker שלו.
4. Session.dispatchEvent(`role`, { role: "leader", promoted: true }).
5. `tab.js` מקבל את ה-event, מעביר את ה-canvas שלו ל-worker החדש:
   `surfaceEl.transferControlToOffscreen()` → `session.send("attach_display", …)`.
6. מערכת כעת יציבה עם Follower-B כ-leader חדש.

**מה נשמר:** שום state-in-memory מה-kernel הישן. Storage (OPFS, Phase 2) נשאר — זה המקור האמיתי של persistence.
**מה מתאפס:** stack של activities, scroll positions, ViewModel memory. זה התנהגות MVP מתועדת — לא באג.

## 4. Pipeline של events

מקליק עכבר ועד לפיקסלים על המסך:

```
Canvas 'pointerdown'                          (main thread, tab.js)
    │
    ▼
pointerEvent("down")(e)                       (tab.js)
    │  builds {type:"event", kind:"pointer", x, y, button, phase}
    ▼
session.send(msg)                             (leader.js → Session.send)
    │
    ├── role === "leader":  worker.postMessage(msg)
    └── role === "follower": bc.postMessage({type:"follower_msg", payload:msg})
                             └─► other tab's Session in leader mode receives
                                 it via _onBroadcast and posts it to its worker
    ▼
worker.addEventListener("message")            (worker.js)
    │
    ▼
forwardEvent(msg)                             (worker.js)
    │  encodes to binary wire format (10 bytes for pointer)
    │  calls akiva_alloc(len)
    │  writes bytes into kernel memory at returned ptr
    ▼
instance.exports.akiva_on_event(kind, ptr, len)   (boundary: JS → wasm)
    │
    ▼
Event::decode(kind, bytes)                    (kernel/event_queue.rs)
    │
    ▼
kernel().events.push(Event::Pointer{...})     (kernel/lib.rs)
    │  queued; returns to JS immediately
    │
    │   ... next frame (rAF) ...
    ▼
akiva_frame(ts)                               (kernel/lib.rs)
    │  drains event queue:
    ▼
Kernel::handle_event(Event::Pointer{...})
    │  DisplayManager::set_pointer(x, y, pressed)
    ▼
DisplayManager::render_frame(ts)
    │  hal_draw_clear → hal_draw_text → hal_draw_rect (pointer marker)
    │  → hal_draw_present
    ▼
Canvas2DBackend (inside worker)               (runtime/web/renderer/canvas2d.js)
    │
    ▼
OffscreenCanvas paints                        (pixels appear on screen)
```

### 4.1 Wire format

כל event מקודד little-endian. ה-encoder הוא JS-side (`worker.js:forwardEvent`), ה-decoder הוא wasm-side (`kernel/src/event_queue.rs:Event::decode`). השדות חייבים להסכים.

| kind | סוג | מבנה |
|---|---|---|
| 1 | pointer | `f32 x, f32 y, u8 button, u8 phase (0=down, 1=move, 2=up, 3=cancel)` |
| 2 | key | `u32 code, u8 modifiers, u8 phase (0=down, 1=repeat, 2=up)` |
| 3 | resize | `u32 width_px, u32 height_px, u32 density_q8_8` |
| 4 | visibility | `u8 visible` |

density ב-q8.8 (fixed-point) — מכפלת 256 של `devicePixelRatio`. DPR של 2.0 → 512.

### 4.2 Coalescing

ה-EventQueue ב-kernel מאחד רצף של `Pointer { phase: Move }` רצופים — אחרון מחליף קודם שלא נצרך. זה מונע backlog של מאות מהלכי עכבר שהצטברו בשנייה. clicks (`Down`/`Up`) תמיד נשמרים ללא איבוד.

## 5. ניהול ה-canvas

```
boot                 follower                leader                promotion
────                 ────────                ──────                ─────────
canvas.width=set     canvas.width=set        canvas transferred    canvas transferred
canvas main-thread   canvas main-thread      → worker               → worker (late)
boot banner shown    boot banner hidden      boot banner hidden    boot banner hidden
no draws             no draws                worker draws          worker draws
```

עיקרון יסוד: `transferControlToOffscreen()` הוא פעולה חד-פעמית בלתי-הפיכה. ברגע שקרא, ה-main thread לא יכול יותר לקרוא או לכתוב ל-`canvas.width`/`height`/`getContext`. כל resize צריך לעבור ל-worker דרך הודעה.

לכן ה-state ב-`tab.js` שומר `state.ownsCanvas` — טבעי כ-`true`, הופך ל-`false` אחרי transfer. פונקציית `applyCanvasSize` נקראת רק כשה-main thread עדיין בעלים.

## 6. פקודות להרצה

```sh
# בנייה של ה-kernel ל-wasm
cargo build -p akiva-kernel --release --target wasm32-unknown-unknown

# העתקה למקום שה-HAL מצפה
cp target/wasm32-unknown-unknown/release/akiva_kernel.wasm runtime/web/akiva-kernel.wasm

# הרמת שרת סטטי (ה-HAL הוא static-only ב-Phase 1)
python3 -m http.server 8765 --directory runtime/web
```

פתיחת טאבים נוספים על אותו URL מצטרפת אוטומטית כ-followers.

### 6.1 בדיקות ידניות

- **Leader roundtrip:** פתח טאב יחיד. Console: `role=leader`, `kernel: boot`, `display: primary surface ready`. הזז עכבר על הקנבס — רואה ריבוע כחלחל עוקב. לחץ — הריבוע הופך ורדרד.
- **Follower registration:** פתח טאב שני. Console שלו: `role=follower`. Console של הראשון: `follower display registered for tab ...`.
- **Failover:** סגור את הטאב הראשון. ב-console של השני: `role=leader (promoted)`, אחריו `kernel: boot`. מופיע הטקסט "Akiva Kernel" והריבוע עוקב העכבר.

## 7. מגבלות ידועות (MVP)

- **Follower canvas אינו מצויר.** קומפוזיציה cross-tab לא מיושמת; טאב 2 רואה רקע שחור של הדפדפן עד שהוא מקודם. תוקן ב-Phase 2 דרך frame deltas או ScreenSharing API.
- **State אינו נשמר בין leaders.** קידום מאפס kernel memory. Persistence אמיתי מגיע עם OPFS ב-Phase 2.
- **BroadcastChannel אינו מעביר MessagePorts.** תקשורת follower→leader נוגעת ב-channel הקהילתי — כל טאב רואה את כל ההודעות. למעבר ל-MessagePort לכל follower נעשה ב-Phase 2+ דרך SharedWorker feature-detection.
- **כל הטאבים חייבים להיות באותו origin.** Web Lock ו-BroadcastChannel scoped ל-origin. cross-origin העברה לא רלוונטית ל-MVP.

## 8. מסמכים קשורים

- [ARCHITECTURE.md §9](../ARCHITECTURE.md) — Single-instance architecture, רציונל למה Web Locks ולא SharedWorker.
- [ARCHITECTURE.md §10](../ARCHITECTURE.md) — RendererBackend trait (תוכנית ל-Phase 2).
- [docs/abi.md](abi.md) — ABI של האפליקציה (Component Model). לא קשור ל-HAL הפנימי שמתואר כאן.
- [kernel/src/host.rs](../kernel/src/host.rs) — רשימת ה-imports שה-kernel דורש מה-HAL.
