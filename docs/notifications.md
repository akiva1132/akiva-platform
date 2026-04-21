# Notification System

## 1. מטרה

מערכת Notifications של Akiva מאפשרת לאפליקציה להציג הודעה למשתמש **מחוץ לחלון של ה-activity שלה**. זה כולל:
- Toasts — הודעה קצרה בתחתית, ללא interaction (ב-MVP דרך `system.show-toast`, לא דורש Notification subsystem).
- Persistent notifications — הודעה ש"יושבת" במגש, עם title/body/icon, עם אפשרות actions.
- Foreground notifications — hint למערכת ש-process שם ב-background חי וחשוב (למשל: "הורדה בתהליך").

## 2. החלטה ל-MVP

**ב-MVP אין subsystem מלא של notifications.** מה שיש:
- `system.show-toast(message, duration)` — מופיע ב-WindowManager layer 1000 (ראה `window-manager.md §5`). זה *עובד*, אבל limited — לא נוחת במגש, לא חי אחרי סגירת ה-activity, לא actions.

זה מספיק למחשבון ולאפליקציות ה-hello. Notifications אמיתיים נדחים ל-v0.2.

## 3. ארכיטקטורה מתוכננת (post-MVP)

### הרעיון

הרכיב `NotificationManager` חי במסגרת ה-framework. אפליקציה שולחת intent-like request, המסגרת מציגה ב-Shell (או ב-system UI overlay).

```
App  → NotificationManager.notify(id, NotificationSpec)
        │
        ▼
NotificationManager persist to storage
        │
        ▼
NotificationManager broadcasts to Shell
        │
        ▼
Shell renders notification tray / badge
```

### WIT (draft, post-MVP)

```wit
interface notifications {
    record notification-spec {
        title: string,
        body: string,
        icon: option<string>,      // @drawable/...
        priority: priority,
        category: string,          // for grouping
        ongoing: bool,             // persistent (not dismissable)
        actions: list<action>,
        tap-intent: option<intent>,
    }
    enum priority { low, default, high, urgent }
    record action {
        label: string,
        action-intent: intent,
    }

    notify: func(id: string, spec: notification-spec);
    cancel: func(id: string);
    cancel-all: func();
}
```

### Capability

שידרוג עתיד ל-manifest: `akiva.permission.NOTIFICATIONS` = required.

### Delivery to Shell

ב-post-MVP: Shell תממש intent filter `akiva.intent.action.NOTIFICATION_POSTED` + broadcast mechanism. NotificationManager ישדר בכל notify. Shell תרנדר tray משלה.

## 4. מגבלות מכוונות

- **אין browser push**: אם המשתמש סגר את כל ה-tabs — אין notifications. אין Service Worker שמאזין ל-push, כי ה-OS לא רץ שם.
- **בתוך session**: כל ה-notifications הם in-app. זה מתאים לקונספט של Akiva כ-OS על דפדפן — אם אתה לא "בתוך", אין מערכת הפעלה פעילה.

## 5. תלות

לפני שממשים notifications:
- `StorageManager` חייב לתמוך ב-cross-app read (Shell קוראת notifications של apps אחרים).
- `Shell` חייב לתמוך ב-tray / drawer UI.
- Broadcast intents (ראה `intents.md §13`) חייבים להיות זמינים.

לכן notifications הם v0.3+, לא v0.2.
