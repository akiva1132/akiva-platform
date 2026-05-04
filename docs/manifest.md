# Akiva Manifest

> **Implementation status (May 2026, ABI 0.12.0):** parser +
> validation are complete; install / uninstall / rehydrate from OPFS
> are live. Permission enforcement is wired (Stone 10.2 —
> `package_manager::install` rejects any manifest whose
> `permissions.required` names a permission not declared by the
> system; `permissions::declare_mvp_permissions` declares the catalog
> at boot). The `.akv` archive format and the CLI commands `akiva
> package` / `install` / `uninstall` ship in [`cli/src/`](../cli/src/);
> the runtime still sources dev apps from `DEV_APPS` rather than from
> installed archives, so the on-disk archive is real but the
> production install path stays gated until the dev-server / loader
> closes that loop.

## 1. Purpose

Every Akiva app ships with a `manifest.toml` at its root. The
manifest declares the app's identity, its activities, the
permissions it needs, intent filters, and metadata. The
`PackageManager` parses the manifest at install time and on every
boot.

## 2. Why TOML

- Readable; no quoting on every field.
- Native to the Rust ecosystem (Cargo, pyproject, pipfile).
- Distinct from JSON / XML / web-stack formats.
- Supports hierarchical and array-of-tables structures.

## 3. Full schema

```toml
# ============ App identity ============
[app]
id = "com.akiva.calculator"          # reverse-domain, system-unique
name = "Calculator"                  # display name (or @string/app_name)
version = "0.1.0"                    # semver
version_code = 1                     # monotonic integer
authors = ["Akiva Team"]
description = "Basic four-function calculator"
icon = "@drawable/icon"              # resource ref
category = "utilities"               # utilities | games | productivity | social | …
min_abi_version = "0.1.0"            # SemVer; runtime must host >= this

# ============ Runtime ============
[runtime]
wasm = "calculator.wasm"             # path relative to the package root
min_memory_mb = 1
max_memory_mb = 16

# ============ Activities ============
[[activities]]
id = "main"                          # unique within the app
class = "CalculatorMain"             # log/debug label
entry = true                         # the default-launched activity
launch_mode = "standard"             # standard | singleTop | singleTask | singleInstance
task_affinity = "com.akiva.calculator" # default = package_id
exported = true                      # may other apps launch it?
config_changes = ["orientation", "locale", "ui_mode"]
# ^ if one of these config dimensions changes, the activity gets
#   on_config_change instead of being recreated.

[[activities.intent_filters]]
actions = ["akiva.intent.action.MAIN"]
categories = ["akiva.intent.category.LAUNCHER"]

# A second activity (settings)
[[activities]]
id = "settings"
class = "CalculatorSettings"
launch_mode = "singleTop"
exported = false                     # only the calculator itself can launch it

[[activities.intent_filters]]
actions = ["com.akiva.calculator.intent.SETTINGS"]
categories = ["akiva.intent.category.DEFAULT"]

# ============ Permissions ============
[permissions]
required = [
    # auto-granted at install time once user has consented;
    # the app refuses to install if any are not declared.
]
optional = [
    # may be requested at runtime; the app must work without them.
]

# Declarations of permissions the app *defines itself* (advanced).
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

# ============ Inter-app dependencies (optional) ============
[dependencies]
# Restrict to apps providing a given intent.
# Not used in MVP.

# ============ Resources manifest (optional) ============
[resources]
supported_locales = ["en", "he"]
default_locale = "en"
```

## 4. Required fields

| field                                                 | reason                  |
|-------------------------------------------------------|-------------------------|
| `app.id`                                              | unique identity         |
| `app.name`                                            | shown in Shell          |
| `app.version`                                         | version management      |
| `app.version_code`                                    | version comparison      |
| `app.min_abi_version`                                 | compatibility check     |
| `runtime.wasm`                                        | the wasm binary path    |
| at least one `[[activities]]` with `entry = true`     | a launch point          |

## 5. Optional fields with defaults

| field                              | default                                     |
|------------------------------------|---------------------------------------------|
| `app.authors`                      | `[]`                                        |
| `app.description`                  | `""`                                        |
| `app.icon`                         | Akiva's default icon                        |
| `app.category`                     | `"other"`                                   |
| `runtime.min_memory_mb`            | `1`                                         |
| `runtime.max_memory_mb`            | `32`                                        |
| `activity.launch_mode`             | `"standard"`                                |
| `activity.task_affinity`           | `app.id`                                    |
| `activity.exported`                | `true` if any `intent_filter` exists; else `false` |
| `display.default_width_dp`         | `360`                                       |
| `display.default_height_dp`        | `640`                                       |
| `display.resizable`                | `true`                                      |
| `display.orientation`              | `"any"`                                     |
| `permissions.required`             | `[]`                                        |
| `permissions.optional`             | `[]`                                        |
| `resources.default_locale`         | `"en"`                                      |

## 6. Validation (at install / build)

The `PackageManager` rejects an app if any of these is true:

- ❌ TOML syntax invalid
- ❌ a required field is missing
- ❌ `app.id` is not a legal reverse-domain (`[a-z0-9_-]`, at least
  one `.`, no leading digit)
- ❌ `app.version` or `min_abi_version` is not semver
- ❌ `min_abi_version` exceeds the runtime ABI
- ❌ `runtime.wasm` does not point at an existing file in the package
- ❌ the wasm does not export the required lifecycle hooks
- ❌ no activity has `entry = true`
- ❌ activity `id`s are not unique within the app
- ❌ a resource ref (`@string/...`, `@drawable/...`) is unresolved
- ❌ an `intent_filter` has an empty `actions` list
- ❌ `permissions.required` names a permission the system has not
  declared

### What is verified, where

The implementation lives in
[`framework/src/manifest.rs`](../framework/src/manifest.rs) and
[`framework/src/package_manager.rs`](../framework/src/package_manager.rs):

| check                                                                                                | status        | location                                                                                                                  |
|------------------------------------------------------------------------------------------------------|---------------|---------------------------------------------------------------------------------------------------------------------------|
| TOML syntax                                                                                          | ✅            | `toml` crate                                                                                                              |
| required fields (`app.id`, `app.name`, `app.version`, `app.version_code`, `app.min_abi_version`, `runtime.wasm`) | ✅ | `normalize()` + serde                                                                                                     |
| `app.id` reverse-domain legality                                                                     | ✅            | `validate_app_id`                                                                                                         |
| `app.version` / `min_abi_version` semver shape                                                       | ✅            | `validate_semver`                                                                                                         |
| `min_abi_version` ≤ runtime ABI                                                                      | ✅            | `package_manager::check_abi` (same-major check today)                                                                     |
| entry activity exists                                                                                | ✅            | `normalize()`                                                                                                             |
| activity `id` unique                                                                                 | ✅            | `normalize_activities`                                                                                                    |
| `launch_mode` valid                                                                                  | ✅            | `normalize_activities`                                                                                                    |
| `intent_filter.actions` not empty                                                                    | ✅            | `normalize_activities`                                                                                                    |
| `runtime.wasm` exports the required hooks                                                            | ✅            | [`runtime/web/app_registry.js::REQUIRED_EXPORTS`](../runtime/web/app_registry.js) verifies 14 exports including `memory` |
| `permissions.required` declared by the system                                                        | ✅ (Stone 10.2) | `package_manager::install` — installs are rejected with `InstallError::UnknownPermission` if any required name is unknown  |
| Resource refs resolve at install time                                                                | ⏳            | resolution at *render* time works; install-time `@ref → key exists` lookup not yet wired                                  |

Defaults from §5 are applied in `normalize()`: `exported` is computed
from the presence of `intent_filters`; `task_affinity` defaults to
`app.id`; `launch_mode` defaults to `"standard"`; etc.

## 7. Package layout

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

The on-disk packaged form is a `.akv` archive (AKPK format —
[archive-format.md](archive-format.md)). The CLI handles the
encode / decode side; the runtime currently bypasses the archive for
dev apps and sources from `runtime/web/dev-apps/` directly.

## 8. Example: Shell manifest

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
launch_mode = "singleTask"           # Shell is always single-instance

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
    "akiva.permission.QUERY_PACKAGES",
]
```

## 9. Forward-looking

These are tracked but not in the v0.2 surface:

- **Code signing** — `signature.sig` signed by a publisher key.
- **Manifest inheritance** — common fragments composed at install time.
- **Bundled asset variants** per locale / density.
- **Feature requirements** (Android's `<uses-feature>`) — e.g. asking
  for a WebGPU-capable host.
- **Split packages** (code splitting) — v0.3+.
- **Task-management attributes** — `allow_task_reparenting`,
  `clear_task_on_launch`. Android-parity, niche; depends on the
  multi-task orchestration scheduled for Phase 11+. Not currently
  reserved in the namespace.
