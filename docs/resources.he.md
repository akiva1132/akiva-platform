# Resource System

## 1. מטרה

מערכת resources מפרידה בין **תוכן נוכחי של אפליקציה** (קובעי טקסט, צבעים, גודלים, אייקונים) לבין **קוד האפליקציה**. זה מאפשר:
- תרגום ללא שינוי קוד
- dark mode ללא שינוי קוד
- התאמות orientation/density ללא שינוי קוד
- קריאות ותחזוקה

## 2. מבנה תיקיות

```
apps/my-app/
└── res/
    ├── layout/            ← AKML layouts (default)
    ├── layout-land/       ← landscape variant
    ├── values/            ← default strings, colors, dimens, styles
    │   ├── strings.toml
    │   ├── colors.toml
    │   ├── dimens.toml
    │   └── styles.toml
    ├── values-he/         ← Hebrew translations
    │   └── strings.toml
    ├── values-night/      ← dark mode colors
    │   └── colors.toml
    ├── values-he-night/   ← combined
    │   └── (not required, falls back to values-night and values-he)
    └── drawable/          ← icons, images
        └── drawable-xhdpi/
```

## 3. Qualifiers נתמכים

ה-qualifiers נכתבים בסדר קבוע (המחשב מפרסר לפי הסדר הזה):

| # | Qualifier | ערכים | דוגמה |
|---|---|---|---|
| 1 | MCC/MNC | לא נתמך ב-MVP | — |
| 2 | Locale | ISO 639-1 code | `he`, `en`, `ar`, `he-IL` |
| 3 | Layout direction | `ldltr`, `ldrtl` | `ldrtl` |
| 4 | Smallest width | `sw<N>dp` | `sw600dp` (tablets) |
| 5 | Available width | `w<N>dp` | — |
| 6 | Available height | `h<N>dp` | — |
| 7 | Screen size | `small`, `normal`, `large`, `xlarge` | `large` |
| 8 | Screen orientation | `port`, `land` | `land` |
| 9 | UI mode | `night`, `notnight`, `car`, `tv` | `night` |
| 10 | Density | `mdpi`, `hdpi`, `xhdpi`, `xxhdpi`, `xxxhdpi` | `xhdpi` |

### דוגמאות של combinations חוקיים

```
values/
values-he/
values-land/
values-night/
values-he-land-night/
values-large-xhdpi/
```

## 4. Resolution Algorithm

כש-app מבקשת `@string/welcome`, ה-ResourceManager:

1. בודק את ה-config הנוכחי של המכשיר, למשל: `{ locale: he-IL, dir: rtl, orient: port, ui: night, density: xhdpi }`
2. מחפש את ה-resource directory עם הכי הרבה qualifier matches:
   - `values-he-rIL-night/` ← match 3, 9
   - `values-he-night/` ← match 2, 9
   - `values-he/` ← match 2
   - `values-night/` ← match 9
   - `values/` ← fallback
3. מחזיר את המציאה הכי ספציפית (הכי הרבה matches).
4. אם ה-key לא נמצא בתיקייה הנבחרת — יורד לפחות ספציפי עד שמוצא.

### Priority order (simplified)

Locale > Orientation > Dark mode > Density.

## 5. ResourceTable ב-boot

ה-ResourceManager בונה בטעינה ראשונית:

```rust
pub struct ResourceTable {
    // key = "@string/welcome", value = final resolved string (after qualifier pick)
    strings: HashMap<String, String>,
    colors: HashMap<String, Color>,
    dimens: HashMap<String, Dimen>,
    styles: HashMap<String, Style>,
    drawables: HashMap<String, Drawable>,
    layouts: HashMap<String, PathBuf>,  // AKML file path (inflated on demand)
}
```

Rebuild of the table happens on config change (orientation flip, language switch, dark mode toggle).

## 6. Values Files (TOML format)

### res/values/strings.toml

```toml
[strings]
app_name = "Calculator"
zero = "0"
clear = "C"
action_go = "Go"
welcome = "שלום %{name}"  # format arg
```

Named arguments (`%{name}`) להקשר במקום מיקום (`%1`).

### res/values/colors.toml

```toml
[colors]
primary = "#5B6CF9"
on_primary = "#FFFFFF"
surface = "#FFFFFF"
on_surface = "#0C0C10"
display_bg = "#F4F4F7"
num_button_bg = "#E8E8EC"
op_button_bg = "#FFA500"
```

### res/values-night/colors.toml

```toml
[colors]
primary = "#7B8CFF"
on_primary = "#FFFFFF"
surface = "#0C0C10"
on_surface = "#F4F4F7"
display_bg = "#1A1A1E"
num_button_bg = "#2A2A2E"
op_button_bg = "#FFA500"
```

### res/values/dimens.toml

```toml
[dimens]
spacing_small = "4dp"
spacing_medium = "8dp"
spacing_large = "16dp"
text_size_body = "16sp"
text_size_title = "24sp"
text_size_display = "48sp"
button_min_width = "64dp"
```

### res/values/styles.toml

```toml
[styles."num_button"]
background = "@color/num_button_bg"
textColor = "@color/on_surface"
textSize = "@dimen/text_size_title"
padding = "@dimen/spacing_large"

[styles."op_button"]
parent = "num_button"  # inheritance
background = "@color/op_button_bg"
textColor = "@color/on_primary"
```

Style inheritance: child overrides parent.

## 7. Localization Flow

```
User changes system language from EN to HE
     │
     ▼
Browser fires languagechange event
     │
     ▼
Tab HAL → Worker: "config change: locale=he"
     │
     ▼
Worker/ResourceManager: rebuild ResourceTable
     │
     ▼
ResourceManager notifies ActivityManager
     │
     ▼
Each active Activity gets onConfigChange
     │
     ▼
Default behavior: recreate activity
     │
     ▼
onSaveState → onDestroy → onCreate(savedState) → onRestoreState
     │
     ▼
Activity re-inflates AKML with new resources
     │
     ▼
Strings now shown in Hebrew, layout-ldrtl used if present
```

## 8. Drawables (icons/images)

### Formats נתמכים (MVP)

**PNG בלבד ב-MVP.** כן, אין SVG ואין JPG לעת עתה. הסיבה:
- SVG דורש parser + rasterizer משלו (resvg או equivalent, ~400KB wasm). בונים את זה אחרי ש-Compositor יציב.
- JPG דורש decoder; אין upside ל-app icons; ל-backgrounds נחזור ב-v0.2.
- PNG ניתן ל-decode מהירות דרך browser native API (`createImageBitmap(blob)`) שה-Worker HAL חושפת.

**Post-MVP (v0.2+):**
- SVG דרך `resvg` compiled ל-wasm, מעוקל פנימית ל-PNG per-density ב-install time, לא runtime.
- JPG דרך `createImageBitmap`.

### Decoding flow

```
App: resources.get-drawable("@drawable/icon")
  │
  ▼
ResourceManager picks variant by density bucket
  │
  ▼
Worker HAL: createImageBitmap(blob) → ImageBitmap
  │
  ▼
Uploaded to RendererBackend cache with ImageId
  │
  ▼
Returned to app as `drawable` resource handle
  │
  ▼
App: widget.set-image-drawable(image_widget, drawable_handle)
  │
  ▼
Compositor emits DrawImage(dst, image_id)
```

ה-PNG עצמה לא מגיעה ל-wasm של האפליקציה — רק handle.

### Density qualifiers for PNGs

```
res/drawable-mdpi/logo.png    ← 48x48 for 160dpi baseline
res/drawable-hdpi/logo.png    ← 72x72 for 240dpi
res/drawable-xhdpi/logo.png   ← 96x96 for 320dpi
res/drawable-xxhdpi/logo.png  ← 144x144 for 480dpi
res/drawable-xxxhdpi/logo.png ← 192x192 for 640dpi (optional)
```

אם ל-`@drawable/logo` יש רק `drawable-xhdpi/logo.png`, ResourceManager ישתמש בו בכל הצפיפויות (עם scaling). המלצה: ספק לפחות xhdpi + xxhdpi.

### האם אפשר רק `drawable/logo.png` בלי density?

כן — זה fallback לכל הצפיפויות. מופץ כ-xhdpi (density 2.0) באמונה. אופטימלי לאייקונים של MVP.

### מה עם density-independent אייקונים?

ב-MVP: PNG ברזולוציה הגבוהה שצפוי להתאים (xxhdpi = 144×144 ל-icon רגיל), ה-renderer יעשה scale down. איכות מקובלת. לטווח ארוך SVG יפתור זאת.

## 9. Device Config API

```rust
pub struct DeviceConfig {
    pub locale: Locale,           // "he-IL"
    pub layout_dir: LayoutDir,    // Rtl | Ltr
    pub orientation: Orientation, // Portrait | Landscape
    pub ui_mode: UiMode,          // Light | Dark
    pub density: Density,         // Xhdpi, etc.
    pub screen_width_dp: u32,
    pub screen_height_dp: u32,
}

pub struct ResourceManager {
    pub fn current_config(&self) -> &DeviceConfig;
    pub fn on_config_change(&mut self, new: DeviceConfig);
    pub fn get_string(&self, ref_: &str) -> Option<&str>;
    pub fn get_color(&self, ref_: &str) -> Option<Color>;
    pub fn get_dimen(&self, ref_: &str) -> Option<Dimen>;
    pub fn get_layout(&self, ref_: &str) -> Option<PathBuf>;
}
```

## 10. ל-app: איך לגשת ל-resources

ה-SDK חושף API פשוט:

```rust
use akiva_sdk::res;

let welcome = res::string("@string/welcome");
let primary = res::color("@color/primary");
let spacing = res::dimen("@dimen/spacing_large");

// עם format args
let greeting = res::string_fmt("@string/welcome", &[("name", "עקיבא")]);
```

מתחת למכסה ה-macro `res::string` מתרגם ל-ABI call.

## 11. Validation ב-build time

`akiva build` מריץ validator על ה-resources:
- כל `@string/foo` שמופיע ב-AKML **חייב** להיות מוגדר ב-strings.toml
- Locales שונות חייבות להכיל את אותן keys (אחרת fallback, עם warning)
- Types: `@color/x` חייב להיות color valid, `@dimen/y` חייב להיות dimen valid

שגיאות כאלה מונעות build.

## 12. מגבלות MVP

- אין `@array/` (arrays of strings) — נוסף ב-v0.2
- אין `@integer/`, `@boolean/` — לעת עתה מגדירים ישירות
- אין theme inheritance מחוץ ל-styles (themes מלאים ב-v0.2)
- אין transitions/animations עם resource refs
- Pluralization: רק `@string/foo` ברוב הגרסאות, אבל יש תמיכה בסיסית ב-`@plurals/foo { one, other }` ל-MVP.
