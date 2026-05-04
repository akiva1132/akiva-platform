# Window Manager & Shell

## 1. מטרה

**WindowManager** הוא תת-המערכת שמחליטה **מה מוצג על איזה display בכל רגע נתון**. הוא הגשר בין ActivityManager (שיודע "מי אמור להיות Resumed") לבין Compositor (שיודע "איך לצייר").

**Shell** היא אפליקציית Akiva רגילה שממלאת תפקיד מיוחד — מסך הבית.

## 2. תפקידי ה-WindowManager

- **Display registry** — רשימת displays מחוברים (tabs)
- **Activity placement** — איזה activity על איזה display
- **Z-order** — מה מעל מה (system UI, dialogs, toasts)
- **Focus management** — לאיזו activity נשלחים input events
- **Transitions** — אנימציה של מעברים (ב-MVP: cut מיידי)
- **Chooser / Dialogs** — הצגת system dialogs (intent chooser, permission prompts)

## 3. Display Model

```rust
pub struct Display {
    pub id: DisplayId,
    pub width_dp: u32,
    pub height_dp: u32,
    pub density: f32,
    pub orientation: Orientation,
    pub message_port: MessagePort,  // link to Tab HAL
    pub state: DisplayState,
}

pub enum DisplayState {
    Active,     // tab visible, receiving events
    Idle,       // tab exists but not focused
    Disconnected, // tab closed
}
```

Displays באים והולכים:
- `register_display(info)` — כש-tab חדש מתחבר
- `unregister_display(id)` — כש-tab נסגר
- `update_display(id, info)` — resize, orientation change

## 4. Activity Placement

ActivityManager מבקש מ-WindowManager "הצג את activity X":

```rust
pub trait WindowManager {
    fn attach_activity(&mut self, activity: ActivityId, display: DisplayId);
    fn detach_activity(&mut self, activity: ActivityId);
    fn move_activity(&mut self, activity: ActivityId, target: DisplayId);
    fn render_frame(&mut self, display: DisplayId);
}
```

### Mapping ב-MVP

פשוט: activity אחד פעיל → מוצג ב-display שבו הוא היה אחרון. אם display לא קיים (tab נסגר בינתיים) → activity עובר ל-display הראשון הזמין.

### עתיד

- Multi-window: שתי activities side-by-side
- Picture-in-Picture: activity צפה ב-overlay
- Secondary display: activity פתוחה ב-tab 2 בלבד

## 5. Z-Order Layers

WindowManager מנהל layers בכל display:

```
┌─────────────────────────────────────┐  ← z=1000: System toast
├─────────────────────────────────────┤
├─────────────────────────────────────┤  ← z=900:  Dialogs (chooser, permission)
├─────────────────────────────────────┤
├─────────────────────────────────────┤  ← z=800:  System UI overlays (PIP)
├─────────────────────────────────────┤
├─────────────────────────────────────┤  ← z=100:  Status bar / nav bar
├─────────────────────────────────────┤
├─────────────────────────────────────┤  ← z=0:    Current activity
└─────────────────────────────────────┘
```

Compositor מציג כל layer בסדר, top-down. Input events מגיעים קודם לשכבה העליונה שכוללת את הנקודה.

## 6. Input Routing

```
Tab HAL receives pointer_down(x, y)
  │
  ▼
MessagePort → Worker
  │
  ▼
EventQueue (kernel) receives event
  │
  ▼
Dispatcher looks at display_id from event
  │
  ▼
WindowManager.hit_test(display_id, x, y) → widget_id
  │
  ├─ check top layer first (dialogs, toasts)
  ├─ if no hit, next layer down
  └─ finally, activity layer
  │
  ▼
Activity receives akiva_on_event(widget_id, event_type, payload)
  │
  ▼
Callback fired, state updated
  │
  ▼
Next frame: re-layout, re-compose, new draw commands
```

## 7. Focus Management

בכל רגע, בכל display, יש **exactly one activity** ב-Resumed state (המצב היחיד שמקבל input):

- Activity חדש נפתח → הישן → Paused, החדש → Resumed.
- User לוחץ במקום ריק → focus נשאר (בניגוד ל-web, אין focus-on-click).
- Input widget focus (typing): focus פנימי של ה-activity.

### Keyboard events

- רק activity ב-Resumed מקבלת keyboard events.
- אם יש input widget בעל focus — הטקסט הולך אליו.
- אחרת, האפליקציה מקבלת את ה-raw key event ל-callback `on_key_event`.

## 8. Shell App

### הגדרה

Shell היא **אפליקציית Akiva**, עם mods מסוימים:
- Pre-installed, לא ניתן להסיר
- מקבלת permission מיוחד: `akiva.permission.LAUNCH_ANY_APP`
- מטפלת ב-intent `category=HOME`
- נטענת אוטומטית ב-boot

### תפקידים

- **Home screen** — רשימת apps מותקנות, launcher
- **Task switcher** — רשימת activities רצות, מעבר ביניהן
- **System UI host** — הצגת status bar (בעתיד)
- **Default fallback** — אם activity נסגרת ואין עוד בstack, חזרה ל-Shell

### Shell ב-MVP (פשוט)

```xml
<!-- apps/shell/res/layout/home.akml -->
<VStack padding="16dp" background="@color/shell_bg" spacing="16dp">
    <Text text="@string/akiva_os" textSize="32sp" textColor="@color/on_bg" />

    <Grid columns="4" spacing="8dp" flex="1">
        <!-- אייקונים מוכנסים דינמית ע"י Shell code -->
    </Grid>
</VStack>
```

```rust
// apps/shell/src/lib.rs
#[akiva::app]
struct Shell {
    installed: Vec<PackageInfo>,
}

impl App for Shell {
    fn on_create(&mut self, ctx: &ActivityContext) {
        self.installed = ctx.package_manager().installed_apps();
        ctx.set_content_view("@layout/home");
        self.render_app_grid(ctx);
    }

    fn on_resume(&mut self, ctx: &ActivityContext) {
        // Refresh in case apps were installed/uninstalled
        let current = ctx.package_manager().installed_apps();
        if current.len() != self.installed.len() {
            self.installed = current;
            self.render_app_grid(ctx);
        }
    }
}

impl Shell {
    fn render_app_grid(&self, ctx: &ActivityContext) {
        let grid = ctx.find_view("@+id/app_grid");
        grid.clear_children();
        for app in &self.installed {
            let icon = ctx.inflate("@layout/app_icon"); // mini layout for each icon
            icon.set_text("@+id/label", &app.name);
            icon.set_tag(&app.id);
            icon.set_click_handler("on_app_click");
            grid.add_child(icon);
        }
    }
}

#[akiva::callback]
fn on_app_click(app: &mut Shell, ctx: &CallbackContext) {
    let package_id = ctx.tag();
    let intent = Intent::new("akiva.intent.action.MAIN")
        .with_category("akiva.intent.category.LAUNCHER")
        .with_package(package_id);
    ctx.activity().start_activity(intent);
}
```

## 9. Back Navigation

כל display מנהל task stack משלו (כרגע — ב-MVP שני displays משותפים את אותו stack).

**Back button** (hardware, software, או browser):
1. Tab HAL שולח event `back_pressed`
2. WindowManager → ActivityManager.on_back()
3. ActivityManager:
   - אם יש dialog פתוח → סגור dialog
   - אחרת אם ל-activity current יש `on_back_pressed` callback → קרא לו, משתמש יכול לבטל
   - אחרת → pop from stack
   - אם stack ריק → exit ל-Shell

### המקרה של browser Back button

הדפדפן יש לו היסטוריה משלו. WindowManager יכול לסנכרן:
- כל pushActivity → `history.pushState()`
- browser back → האר תעוצר ונקבל event
- ה-event מופנה ל-ActivityManager.on_back()

זה נותן ל-browser back button את המשמעות הצפויה של "חזור" בתוך Akiva.

## 10. Transition Animations

**MVP:** אין. החלפת activities = "cut" מיידי.

**V0.2:** תוספות:
- Fade (opacity 0→1 / 1→0)
- Slide (מאוחד עם direction)
- Shared element (widget "ממשיך" מ-activity אחת לאחרת)

WindowManager יהיה אחראי על ה-animation — ActivityManager רק יגיד "transition from X to Y" והרכיב יטפל.

## 11. Dialogs & Toasts

### Dialogs

מוצגים כ-activity modal (בשכבה 900). משתמש לא יכול ללחוץ על activity מתחתם.

דוגמאות: intent chooser, permission prompt, custom app dialogs.

### Toasts

הודעות קצרות ב-bottom, מופיעות ל-2-3 שניות ונעלמות. שכבה 1000. לא בלוקים input.

```rust
ctx.show_toast("Saved!");
ctx.show_toast_long("This will take a moment...");
```

## 12. System UI (v0.2+)

Status bar (למעלה), navigation bar (למטה) יוצגו בעתיד כ-overlays של Shell. עכשיו — כל activity מילוי המסך המלא.

## 13. מגבלות MVP

- Display אחד בפועל (tab ראשון); tabs נוספים מציגים "Akiva is running in another tab"  — או משקפים את אותה activity.
- אין transitions, אין animations.
- אין PIP, אין split-screen.
- Dialogs פשוטים (center, fixed size).
- Shell מינימלי — רק grid של apps, אין task switcher, אין search.
