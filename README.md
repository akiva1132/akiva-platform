# Akiva Platform

**מערכת הפעלה אפליקטיבית** שרצה על הדפדפן כ-runtime. אפליקציות נכתבות בשפות native (Rust כרגע, עתיד: כל שפה שמתקמפלת ל-WebAssembly) ורצות בתוך OS מלא — עם Activity lifecycle, Resource system, Intent-based IPC, Package Manager ו-Window Manager.

> **לא web framework. לא מחולל אתרים. לא Flutter.** זו מערכת הפעלה שלמה שלוקחת את הדפדפן כ-kernel (כמו שאנדרואיד לוקחת את Linux kernel) ובונה מעליו אקוסיסטם אפליקציות native.

---

## מסמכי תכנון

### נקודות התחלה
- [vision.md](vision.md) — **החזון ברמה המושגית** (גרסה 0.2)
- [ARCHITECTURE.md](ARCHITECTURE.md) — **ארכיטקטורה טכנית** של כל המערכת
- [ROADMAP.md](ROADMAP.md) — **תוכנית פיתוח** שלב-אחר-שלב

### מפרטי תתי-המערכות (עמוק)
- [docs/activity-model.md](docs/activity-model.md) — מודל Activity, lifecycle, task stack, launch modes, state layering
- [docs/akml.md](docs/akml.md) — שפת ה-layout הדקלרטיבית (AKML)
- [docs/resources.md](docs/resources.md) — מערכת resources עם qualifiers (`-he`, `-night`, `-land`)
- [docs/intents.md](docs/intents.md) — Intent system + PackageManager + PendingIntent + Broadcasts (post-MVP)
- [docs/window-manager.md](docs/window-manager.md) — WindowManager + Shell
- [docs/manifest.md](docs/manifest.md) — פורמט ה-`manifest.toml`
- [docs/abi.md](docs/abi.md) — חוזה ה-ABI (WIT / Component Model)
- [docs/sdk-rust.md](docs/sdk-rust.md) — SDK לפיתוח אפליקציות ב-Rust
- [docs/notifications.md](docs/notifications.md) — Notifications (MVP-thin)
- [docs/testing.md](docs/testing.md) — אסטרטגיית בדיקות
- [docs/observability.md](docs/observability.md) — Logging, tracing, debugging

---

## מבנה הפרויקט (מתוכנן)

```
akiva-platform/
├── runtime/
│   ├── web/                  # JS HAL (Tab + Worker)
│   │   ├── index.html
│   │   ├── tab.js            # Tab HAL: canvas + input
│   │   └── worker.js         # Shared Worker HAL
│   ├── kernel/               # Kernel (Rust → wasm)
│   │   └── src/
│   │       ├── memory.rs
│   │       ├── scheduler.rs
│   │       ├── event_queue.rs
│   │       ├── storage.rs
│   │       ├── wasm_loader.rs
│   │       └── display_manager.rs
│   └── framework/            # Framework layer (Rust → wasm)
│       └── src/
│           ├── activity_manager/
│           ├── package_manager/
│           ├── resource_manager/
│           ├── window_manager/
│           ├── permissions/
│           ├── intent/
│           ├── akml/         # parser + inflater
│           ├── widgets/
│           ├── layout/
│           └── compositor/
│
├── sdk-rust/                 # Rust SDK לאפליקציות
│   └── src/
│       ├── lib.rs
│       ├── app.rs            # App trait, #[akiva::app]
│       ├── activity.rs
│       ├── callbacks.rs
│       ├── res.rs
│       ├── intent.rs
│       ├── storage.rs
│       └── log.rs
│
├── apps/
│   ├── shell/                # מסך הבית
│   ├── hello/                # אפליקציית בדיקה
│   └── calculator/           # אפליקציית הדגל הראשונה
│
├── cli/                      # akiva CLI
│   └── src/
│       └── main.rs
│
└── docs/                     # מסמכי עיצוב (ראה למעלה)
```

---

## מצב נוכחי

**גרסה 0.0.1 — תכנון מלא**

כל תתי-המערכות תוכננו ברמת design. הצעד הבא: יישום של [שלב 1 ב-ROADMAP](ROADMAP.md#שלב-1--kernel-skeleton--hal) — Kernel Skeleton + HAL.

---

## עקרונות יסוד

1. **OS אמיתית, לא סימולציה.** כל תת-מערכת קיימת, גם אם רזה. אין shortcuts שידרשו שכתוב אחר כך.
2. **אפליקציות native, לא JS.** Rust → wasm. אפשר להרחיב לשפות אחרות מאוחר יותר דרך אותו ABI.
3. **Single OS instance לכל משתמש.** מספר tabs = מספר displays, לא מספר מערכות. (ראה [ARCHITECTURE.md §9](ARCHITECTURE.md#9-single-instance-architecture-tabs--displays-not-devices))
4. **בלי DOM.** רנדור ישיר על canvas עם layout engine אמיתי. מערכת resources, מערכת styles.
5. **AOSP-inspired.** לוקחים את המושגים שהוכחו 20 שנה באנדרואיד ומיישמים אותם מחדש ב-Rust/wasm.
