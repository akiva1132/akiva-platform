# Observability & Debugging

## 1. מטרה

אפליקציה ב-wasm רצה בתוך sandbox. אם אין מנגנון observability מכוון, debugging שלה ב-Akiva יהיה גיהנום. המסמך הזה קובע מה mandatory מ-v0.1.

## 2. עקרונות

1. **כל log של app מגיע למפתח בלי מאמץ.** `println!`/`log::info!` ב-Rust → console של ה-browser.
2. **Traces מובנים, לא רק strings.** `span-begin`/`span-end` יוצרים events אפשריים ל-import ל-Chrome DevTools / Tempo / Jaeger.
3. **ה-runtime עצמה מלוגגת באותו format.** מפתח רואה את ה-app log, kernel log, ו-framework log בזרם מאוחד.

## 3. ABI (ראה `abi.md §4.7`)

```wit
interface log {
    enum level { debug, info, warn, error }
    write: func(level: level, message: string);
    span-begin: func(name: string) -> u32;
    span-end:   func(id: u32);
}
```

`log` **תמיד** מיובא לכל app — אין capability עליו, זה נחשב ל-core.

## 4. Sinks

| Sink | Audience | איך מחובר |
|---|---|---|
| Browser console | Developer | `log.write` at info+ → `console.{info,warn,error}`. debug מודפס רק אם `?akiva_debug=1` ב-URL |
| In-app Log Viewer | End user / QA | Shell תוסיף ב-post-MVP: "Developer → Logs" view, שיקרא את ה-ring buffer של ה-Worker |
| Structured trace | Host tooling | `span-begin` / `span-end` → Chrome DevTools Performance panel דרך `performance.mark` |
| File sink | CI | ב-dev server mode, logs נשמרים ל-`.akiva/logs/session.jsonl` |

## 5. Format

Log line format ב-JSON (מתאים ל-jq/grep):

```json
{
    "ts": 1763890240123,
    "level": "info",
    "src": "com.akiva.calculator",
    "msg": "user tapped digit 7",
    "span_id": 42
}
```

Kernel/framework מלוגגים עם `src` של `"kernel"`, `"framework.activitymanager"`, וכו'.

## 6. Developer flags

ב-URL של ה-host:
- `?akiva_debug=1` — כל log level כולל debug.
- `?akiva_trace=1` — מפעיל tracing spans בפועל (אחרת span-begin/end no-op להוזיל overhead).
- `?akiva_devtools=1` — מוסיף overlay עם FPS, memory per app, active activities.

## 7. Debug panics

אפליקציה ב-wasm שעושה `panic!`:
1. wasm module trap → host שומע.
2. Kernel רושם `{level: error, src: <app_id>, msg: "<panic message>", stack: <wasm stack trace>}`.
3. ActivityManager מציין activity = CRASHED (ראה `activity-model.md §7`).
4. WindowManager מציג Toast "Calculator crashed" עם "Restart" button.

## 8. Logging ל-SDK (Rust)

```rust
use akiva_sdk::log;

log::debug!("counter = {}", self.counter);
log::info!("user tapped {}", tag);
log::warn!("unexpected state");
log::error!("failed to parse: {err}");

// Tracing
let _span = log::span!("recalculate");
self.recalculate();
// Span ends when _span drops.
```

ה-macros האלה ממפים ל-`log.write` / `log.span-begin` / `log.span-end` דרך ה-WIT bindings.

## 9. Performance monitoring

ב-MVP:
- FPS counter overlay כש-`akiva_devtools=1`.
- `log::info!` בכל frame שחורג מ-16.6ms budget (kernel עושה את זה אוטומטית).

ב-v0.2+:
- מדידה per-app של: memory use, time in `on_event`, time in `on_frame`.
- Export ל-Chrome DevTools Performance.

## 10. מה *לא* ב-MVP

- Remote logging (שליחת logs לשרת).
- Advanced profiling דרך perf tools.
- Replay of event stream.
- Persistent crash reports.

כל אלו נחמדים, אף אחד לא חובה כדי לפתח Akiva.
