# Akiva Manifest

## 1. מטרה

כל אפליקציית Akiva מגיעה עם `manifest.toml` בשורש. ה-manifest מצהיר על זהות האפליקציה, ה-activities שלה, הרשאות, intent filters, ומטא-דאטה. ה-PackageManager מפרסר את המניפסט ברגע ה-install ובכל boot.

## 2. למה TOML

- קריא, לא דורש quotes על כל שדה
- נפוץ באקוסיסטם Rust (Cargo, pyproject, pipfile)
- מתרחק מ-JSON/XML של ה-web
- תומך ב-hierarchical ובמבני מערכים

## 3. סכמה מלאה

```toml
# ============ זהות האפליקציה ============
[app]
id = "com.akiva.calculator"         # reverse domain, ייחודי במערכת
name = "Calculator"                  # שם תצוגה (או @string/app_name)
version = "0.1.0"                    # semver
version_code = 1                     # מספר אינטגר עולה
authors = ["Akiva Team"]
description = "מחשבון בסיסי עם ארבע פעולות"
icon = "@drawable/icon"              # reference
category = "utilities"               # utilities | games | productivity | social | ...
min_abi_version = "0.1.0"            # SemVer; runtime must host ≥ this

# ============ Runtime ============
[runtime]
wasm = "calculator.wasm"             # נתיב יחסי ל-wasm של ה-app
min_memory_mb = 1
max_memory_mb = 16

# ============ Activities ============
[[activities]]
id = "main"                          # ייחודי באפליקציה
class = "CalculatorMain"             # לצורכי log/debug
entry = true                         # activity שנפתחת כברירת מחדל
launch_mode = "standard"             # standard | singleTop | singleTask | singleInstance
task_affinity = "com.akiva.calculator" # ברירת מחדל = package_id
exported = true                      # האם apps אחרות יכולות להריץ אותה
config_changes = ["orientation", "locale", "ui_mode"]
# ^ אם משתנה config אחד מאלה, activity תקבל onConfigChange במקום recreate

[[activities.intent_filters]]
actions = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]

# activity שנייה (settings)
[[activities]]
id = "settings"
class = "CalculatorSettings"
launch_mode = "singleTop"
exported = false                     # רק calculator עצמו יכול להריץ

[[activities.intent_filters]]
actions = ["com.akiva.calculator.intent.SETTINGS"]
categories = ["akiva.intent.category.DEFAULT"]

# ============ Permissions ============
[permissions]
required = [
    # פועלות אוטומטית; app לא יתקין בלי המשתמש מאשר אותן
]
optional = [
    # ניתן לבקש runtime; app יכול לעבוד גם בלעדיהן
]

# declaration של permissions שהאפליקציה מגדירה בעצמה (לאפליקציות מתקדמות)
[[permissions.declarations]]
name = "com.akiva.calculator.permission.READ_HISTORY"
description = "Read calculation history"
protection_level = "normal"

# ============ Display ============
[display]
default_width_dp = 320
default_height_dp = 520
resizable = true
orientation = "any"                  # portrait | landscape | any | sensor

# ============ Intents שהאפליקציה צורכת (optional) ============
[dependencies]
# אם מגבילים ל-apps שמספקות intent מסוים
# לא ב-MVP

# ============ Resources manifest (אופציונלי) ============
[resources]
supported_locales = ["en", "he"]
default_locale = "en"
```

## 4. שדות חובה

| שדה | סיבה |
|---|---|
| `app.id` | זהות ייחודית |
| `app.name` | הצגה ב-Shell |
| `app.version` | version management |
| `app.version_code` | compare versions |
| `app.min_abi_version` | compatibility check |
| `runtime.wasm` | איפה ה-wasm |
| לפחות `[[activities]]` אחד עם `entry = true` | יש launch point |

## 5. שדות אופציונליים עם defaults

| שדה | Default |
|---|---|
| `app.authors` | `[]` |
| `app.description` | `""` |
| `app.icon` | icon default של Akiva |
| `app.category` | `"other"` |
| `runtime.min_memory_mb` | `1` |
| `runtime.max_memory_mb` | `32` |
| `activity.launch_mode` | `"standard"` |
| `activity.task_affinity` | ערך של `app.id` |
| `activity.exported` | `true` אם יש intent_filter, אחרת `false` |
| `display.default_width_dp` | `360` |
| `display.default_height_dp` | `640` |
| `display.resizable` | `true` |
| `display.orientation` | `"any"` |
| `permissions.required` | `[]` |
| `permissions.optional` | `[]` |
| `resources.default_locale` | `"en"` |

## 6. Validation (ב-install / ב-build)

ה-PackageManager מסרב לטעון אפליקציה אם:

- ❌ TOML syntax שגוי
- ❌ שדה חובה חסר
- ❌ `app.id` לא reverse-domain legal
- ❌ `app.version` לא semver
- ❌ `min_abi_version` > runtime ABI
- ❌ `runtime.wasm` לא מצביע לקובץ קיים
- ❌ ה-wasm לא מייצא lifecycle hooks חובה
- ❌ אין activity אחד עם `entry = true`
- ❌ `id` של activity לא ייחודי באפליקציה
- ❌ reference ל-`@string/...`, `@drawable/...` לא קיים
- ❌ intent_filter עם action ריק
- ❌ permission declared שלא קיים בסיסטם

## 7. מבנה חבילה

```
calculator/
├── manifest.toml
├── calculator.wasm
├── signature.sig           ← (future: code signing)
└── res/
    ├── layout/
    │   └── main.akml
    ├── values/
    │   ├── strings.toml
    │   ├── colors.toml
    │   └── dimens.toml
    └── drawable/
        └── icon.svg
```

בעתיד — package זה ייארז כ-`.akiva` archive (ZIP with manifest).

## 8. דוגמה: Shell manifest

```toml
[app]
id = "com.akiva.shell"
name = "@string/app_name"
version = "0.1.0"
version_code = 1
icon = "@drawable/icon"
category = "system"
min_abi_version = "0.1.0"

[runtime]
wasm = "shell.wasm"
max_memory_mb = 32

[[activities]]
id = "home"
entry = true
launch_mode = "singleTask"          # תמיד יש רק instance אחד של Shell

[[activities.intent_filters]]
actions = ["akiva.intent.action.MAIN"]
categories = [
    "akiva.intent.category.HOME",
    "akiva.intent.category.DEFAULT",
    "akiva.intent.category.LAUNCHER",
]

[permissions]
required = [
    "akiva.permission.LAUNCH_ANY_APP",
    "akiva.permission.QUERY_ALL_PACKAGES",
]
```

## 9. עתיד

- Code signing — `signature.sig` חתום ע"י publisher key
- Manifest inheritance (common manifest fragments)
- Bundled assets variants per locale/density
- Feature requirements (`<uses-feature>` באנדרואיד) — דרישה של WebGPU, לדוגמה
- Split APKs (code splitting) — נוסף ב-v0.3+
