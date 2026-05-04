# Resource System

> **Implementation status (May 2026):** strings + colors are live
> with one qualifier dimension — **`night`** (Stone 11.6.9 #3,
> light vs. dark). Drawables ship as PNG bitmaps (`icon.png`) keyed
> by package, registered through the cross-package drawable cache
> in [`runtime/web/canvas2d.js`](../runtime/web/renderer/canvas2d.js).
> Locale, density, orientation, layout-direction, screen-size, and
> styles are **not yet implemented** — they are reserved in the
> design but the resource manager currently has no fields for them.
> See [§13](#13-current-coverage-vs-design) for the live-vs-design
> matrix.

## 1. Purpose

The resource system separates an app's **content** (text, colours,
sizes, icons) from its **code**. This enables:

- translation without code changes;
- dark mode without code changes;
- orientation / density adaptations without code changes;
- readability and maintainability.

## 2. Directory layout

```
apps/my-app/
└── res/
    ├── layout/                ← AKML layouts (default)
    ├── layout-land/           ← landscape variant            (planned)
    ├── values/                ← default strings, colors, dimens, styles
    │   ├── strings.toml
    │   ├── colors.toml
    │   ├── dimens.toml
    │   └── styles.toml        (planned)
    ├── values-he/             ← Hebrew translations          (planned)
    │   └── strings.toml
    ├── values-night/          ← dark-mode colours            (LIVE)
    │   └── colors.toml
    ├── values-he-night/       ← combined override            (planned)
    │   └── (not required; falls back to values-night and values-he)
    └── drawable/              ← icons, images
        └── drawable-xhdpi/    ← density bucket               (planned)
```

## 3. Supported qualifiers (target list)

Qualifiers are read in a fixed order — the parser picks them off in
this sequence:

| # | qualifier            | values                                                          | example          | status         |
|---|----------------------|-----------------------------------------------------------------|------------------|----------------|
| 1 | MCC / MNC            | not in scope                                                    | —                | n/a            |
| 2 | Locale               | ISO 639-1 code                                                  | `he`, `en`, `ar` | planned        |
| 3 | Layout direction     | `ldltr`, `ldrtl`                                                | `ldrtl`          | planned        |
| 4 | Smallest width       | `sw<N>dp`                                                       | `sw600dp`        | planned        |
| 5 | Available width      | `w<N>dp`                                                        | —                | planned        |
| 6 | Available height     | `h<N>dp`                                                        | —                | planned        |
| 7 | Screen size          | `small`, `normal`, `large`, `xlarge`                            | `large`          | planned        |
| 8 | Orientation          | `port`, `land`                                                  | `land`           | planned        |
| 9 | UI mode              | `night`, `notnight`, `car`, `tv`                                | `night`          | **live**       |
| 10| Density              | `mdpi`, `hdpi`, `xhdpi`, `xxhdpi`, `xxxhdpi`                    | `xhdpi`          | planned        |

### Examples of legal combinations (target)

```
values/
values-he/
values-land/
values-night/
values-he-land-night/
values-large-xhdpi/
```

## 4. Resolution algorithm

When an app requests `@string/welcome`, the `ResourceManager`:

1. reads the device config — e.g. `{ locale: he-IL, dir: rtl,
   orient: port, ui: night, density: xhdpi }`;
2. searches resource directories for the most-qualifier-matched
   set:
   - `values-he-rIL-night/` — matches 3, 9
   - `values-he-night/`     — matches 2, 9
   - `values-he/`           — matches 2
   - `values-night/`        — matches 9
   - `values/`              — fallback;
3. returns the most specific match;
4. if the key isn't present in the chosen directory, falls back
   level-by-level until found.

### Priority order (simplified)

Locale > Orientation > Dark mode > Density.

> Today only step "9. night" is implemented. The active dimension
> is fed by `DeviceConfig.night: bool`; `ResourceTable::string` /
> `color` look in the `night` overlay first if `cfg.night` is on,
> falling back to `default`.

## 5. `ResourceTable` in memory

The `ResourceManager` holds, per package:

```rust
pub struct ValueSet {
    strings: HashMap<String, String>,
    colors:  HashMap<String, u32>,
}

pub struct ResourceTable {
    default: ValueSet,
    night:   Option<ValueSet>,
    // future fields: locale: HashMap<Locale, ValueSet>, …
}
```

Lookups consult the active overlay first, then the default. The
active overlay is selected by the current `DeviceConfig`. Future
qualifier dimensions slot in as additional fields.

A rebuild happens on config change today: night-mode toggle calls
`ResourceManager::set_night_mode(bool)`, which switches the active
overlay; the next lookup returns the new value. Activities that
care about the change recompose on the following frame.

## 6. Values files (TOML)

### `res/values/strings.toml`

```toml
[strings]
app_name   = "Calculator"
zero       = "0"
clear      = "C"
action_go  = "Go"
welcome    = "Hello %{name}"   # named format arg
```

Named arguments (`%{name}`) are used instead of positional (`%1`).
The format function is provided by the SDK helper `res::string_fmt`.

### `res/values/colors.toml`

```toml
[colors]
primary       = "#5B6CF9"
on_primary    = "#FFFFFF"
surface       = "#FFFFFF"
on_surface    = "#0C0C10"
display_bg    = "#F4F4F7"
num_button_bg = "#E8E8EC"
op_button_bg  = "#FFA500"
```

### `res/values-night/colors.toml`

```toml
[colors]
primary       = "#7B8CFF"
on_primary    = "#FFFFFF"
surface       = "#0C0C10"
on_surface    = "#F4F4F7"
display_bg    = "#1A1A1E"
num_button_bg = "#2A2A2E"
op_button_bg  = "#FFA500"
```

### `res/values/dimens.toml` *(planned — not parsed today)*

```toml
[dimens]
spacing_small      = "4dp"
spacing_medium     = "8dp"
spacing_large      = "16dp"
text_size_body     = "16sp"
text_size_title    = "24sp"
text_size_display  = "48sp"
button_min_width   = "64dp"
```

### `res/values/styles.toml` *(planned)*

```toml
[styles."num_button"]
background = "@color/num_button_bg"
textColor  = "@color/on_surface"
textSize   = "@dimen/text_size_title"
padding    = "@dimen/spacing_large"

[styles."op_button"]
parent     = "num_button"   # inheritance
background = "@color/op_button_bg"
textColor  = "@color/on_primary"
```

Style inheritance: child overrides parent.

## 7. Localisation flow (target)

```
User changes the system language from EN to HE
     │
     ▼
Browser fires the languagechange event
     │
     ▼
Tab HAL → Worker: "config change: locale=he"
     │
     ▼
Worker / ResourceManager: rebuild ResourceTable
     │
     ▼
ResourceManager notifies ActivityManager
     │
     ▼
Each active activity gets on_config_change
     │
     ▼
Default behaviour: recreate the activity
     │
     ▼
on_save_state → on_destroy → on_create(savedState) → on_restore_state
     │
     ▼
The activity re-inflates its AKML against the new resources
     │
     ▼
Strings render in Hebrew; layout-ldrtl is used if present
```

The plumbing for the language flow does not exist yet (locale is
not a qualifier dimension). The night-mode flow does work and
follows the same shape.

## 8. Drawables (icons, images)

### Formats supported (today)

**PNG only.** SVG and JPG are deferred:

- SVG would require its own parser + rasteriser (e.g. `resvg`,
  ~400 KB of wasm). Build it after the Compositor is stable.
- JPG decoder is unnecessary for icons; for backgrounds we'll
  revisit.
- PNG decodes quickly via the browser's native `createImageBitmap`
  exposed by the worker HAL.

**Planned (post-MVP):**

- SVG via `resvg` compiled to wasm, rasterised at install time
  per density bucket, not at runtime.
- JPG via `createImageBitmap`.

### Decoding flow (today)

```
Calculator launches → its app worker reads pkgs/<pkg>/res/drawable/icon.png from OPFS
  │
  ▼
createImageBitmap(bytes) → ImageBitmap
  │
  ▼
postMessage to the kernel worker — bitmap registered in the
foreign-drawable cache (Map<package_id, Map<name, ImageBitmap>>)
  │
  ▼
Shell's render code calls c.draw_foreign_image(&app.id, &app.icon, …)
  │
  ▼
canvas2d backend resolves the ImageBitmap and paints
```

The PNG bytes never reach the calling app's wasm — only a
package-id + drawable-name reference.

### Density qualifiers for PNGs *(planned — not used today)*

```
res/drawable-mdpi/logo.png    ← 48×48  for 160 dpi baseline
res/drawable-hdpi/logo.png    ← 72×72  for 240 dpi
res/drawable-xhdpi/logo.png   ← 96×96  for 320 dpi
res/drawable-xxhdpi/logo.png  ← 144×144 for 480 dpi
res/drawable-xxxhdpi/logo.png ← 192×192 for 640 dpi (optional)
```

If `@drawable/logo` exists only at `drawable-xhdpi/`, the
ResourceManager would use it everywhere with scaling. Recommended
floor when this lands: ship `xhdpi` + `xxhdpi`.

### Can I just have `drawable/logo.png` without a density?

Yes — that is the path the runtime uses today. Fallback for every
density. Treated as `xhdpi` (density 2.0) in spirit.

### What about density-independent icons?

For now, ship a PNG at the highest density that's likely to fit
(`xxhdpi` = 144 × 144 for a normal icon); the renderer scales down.
Quality is acceptable for MVP. SVG closes this long-term.

## 9. Device config (in tree today)

```rust
pub struct DeviceConfig {
    pub night: bool,
    // Reserved for future qualifier dimensions:
    // pub locale: Locale,
    // pub layout_dir: LayoutDir,    // Rtl | Ltr
    // pub orientation: Orientation, // Portrait | Landscape
    // pub density: Density,
    // pub screen_width_dp: u32,
    // pub screen_height_dp: u32,
}
```

Today, only `night` is consulted. Adding fields here + a matching
`ValueSet` overlay slot in `ResourceTable` is how each new
dimension lands.

## 10. App access to resources

The SDK exposes a small API:

```rust
use akiva_sdk::res;

let welcome = res::string("@string/welcome");
let primary = res::color("@color/primary");
let spacing = res::dimen("@dimen/spacing_large");

// With format args:
let greeting = res::string_fmt("@string/welcome", &[("name", "Akiva")]);
```

Underneath, the macros expand into ABI calls
(`akiva_host_resource_string` / `_color`, with a planned `_dimen`
once dimens land).

## 11. Build-time validation (planned)

`akiva build` will run a validator over the resources:

- every `@string/foo` referenced from AKML must be defined in
  `strings.toml`;
- every locale must contain the same key set (otherwise fall back
  with a warning);
- types: `@color/x` must be a valid colour, `@dimen/y` must be a
  valid dimen.

These rules will block the build. The current CLI does not
implement them yet.

## 12. MVP limitations

- No `@array/` (string arrays) — planned for v0.2+.
- No `@integer/`, `@boolean/` — define inline for now.
- No theme inheritance outside of styles (full themes in v0.2+).
- No transitions / animations bound to resource refs.
- Pluralisation: `@string/foo` only; basic `@plurals/foo { one,
  other }` is reserved but not parsed.

## 13. Current coverage vs. design

| capability                                                 | status                |
|------------------------------------------------------------|-----------------------|
| `@string/...` lookup                                       | ✅ live               |
| `@color/...` lookup                                        | ✅ live               |
| `night` qualifier (dark / light)                           | ✅ live (Stone 11.6.9 #3) |
| Cross-package drawables (PNG, no density bucket)            | ✅ live               |
| `@dimen/...` lookup                                        | ❌ planned            |
| `@drawable/...` density buckets                            | ❌ planned            |
| Locale (translation) qualifier                              | ❌ planned            |
| Orientation / layout-direction qualifiers                   | ❌ planned            |
| Screen-size / smallest-width qualifiers                     | ❌ planned            |
| Styles (`@style/...`) + inheritance                         | ❌ planned            |
| `@plurals/...`                                              | ❌ planned            |
| Build-time validator                                        | ❌ planned            |
