# AKML — Akiva Markup Language

> **Implementation status (May 2026):** parser + inflater live in
> [`sdk-rust/src/akml/`](../sdk-rust/src/akml/). Supported widget
> tags: **`VStack`**, **`HStack`**, **`Frame`**, **`Grid`**,
> **`Text`**, **`Button`**, **`Rect`**, **`Spacer`**, **`Image`**.
> `Image` resolves `@drawable/...` refs through the cross-package
> drawable cache. `<Input>`, `<ScrollView>`, and `<List>` are
> design targets, not yet implemented. Style references
> (`@style/...`) and dimension references (`@dimen/...`) are
> reserved in the syntax but the resource manager does not parse
> them yet (see [`docs/resources.md §13`](resources.md)).

## 1. Purpose

AKML is the declarative language for Akiva user interfaces. Like
XML layouts on Android, the UI is written in an external file,
parsed at runtime by the inflater, and the app code only reacts to
events.

## 2. Principles

1. **Declarative.** Describes "what should exist", not "what to
   do".
2. **Decoupled from code.** UI is not in Rust — it's in AKML.
3. **Resource-aware.** `@string/`, `@color/`, `@dimen/`,
   `@drawable/` resolve automatically.
4. **Qualifier-aware.** `res/layout-land/main.akml` would load
   automatically in landscape *(the qualifier flow is the planned
   shape; today only `night` is wired in `res/values/`).*
5. **Type-checked at inflate time.** A malformed AKML fails before
   the app runs — the inflater rejects unknown tags, malformed
   refs, and bad attribute types with positional errors.

## 3. Syntax

AKML is XML-based. Why XML:

- 20+ years of proof on Android.
- Massive tooling (linting, syntax highlighting).
- Schema validation is straightforward.
- Familiar to mobile developers.

### Basic structure

```xml
<?akml version="1.0"?>
<VStack
    id="@+id/root"
    padding="16dp"
    background="@color/surface">

    <Text
        id="@+id/title"
        text="@string/app_name"
        textSize="24sp"
        textColor="@color/primary" />

    <Button
        text="@string/action_go"
        onClick="on_go_clicked" />
</VStack>
```

### Elements

Every element is a widget. The root must be exactly one widget.

**Containers** (live):

- `<VStack>` — vertical stack
- `<HStack>` — horizontal stack
- `<Grid cols="N" rows="M">` — uniform grid
- `<Frame>` — children stack on the z axis at the same origin

**Containers** (planned):

- `<ScrollView>` — scrollable wrapper
- `<List>` — virtualised list of items

**Leaf widgets** (live):

- `<Text>`
- `<Button>`
- `<Rect>` — coloured rectangle (used as a primitive painter)
- `<Spacer>` — pure layout filler
- `<Image src="@drawable/...">` — bitmap from the cross-package
  drawable cache

**Leaf widgets** (planned):

- `<Input>` — editable text field

## 4. Attributes

### Universal

| attribute     | type      | example                                       | meaning                                                    |
|---------------|-----------|-----------------------------------------------|------------------------------------------------------------|
| `id`          | string    | `@+id/name`                                   | unique id within the tree (only when needed)               |
| `width`       | size      | `match_parent`, `wrap_content`, `100dp`, `50%`| width                                                      |
| `height`      | size      | (same)                                        | height                                                     |
| `padding`     | spacing   | `16dp`, `8dp 16dp`, `8dp 16dp 8dp 16dp`       | top, right, bottom, left                                   |
| `margin`      | spacing   | (same)                                        | outer spacing                                              |
| `background`  | color     | `@color/surface`, `#FF0000`                   | background colour                                          |
| `flex`        | float     | `1.0`, `0.5`                                  | weight inside a stack                                      |
| `tag`         | string    | `"9"`, `"op_mul"`                             | data passed back to the click callback                     |
| `visible`     | bool      | `true` / `false` / `gone`                     | visibility (`gone` removes from layout)                    |

### `Text`-specific

| attribute     | type      | meaning                                                            |
|---------------|-----------|--------------------------------------------------------------------|
| `text`        | string / ref | content                                                          |
| `textSize`    | size      | font size                                                          |
| `textColor`   | color     | text colour                                                        |
| `textAlign`   | enum      | `start`, `center`, `end`, `justify`                                |
| `fontWeight`  | enum      | `normal`, `bold`                                                   |
| `maxLines`    | int       | maximum number of rendered lines                                   |
| `ellipsize`   | enum      | `none`, `end`                                                      |

### `Button`-specific

| attribute       | meaning                          |
|-----------------|----------------------------------|
| `onClick`       | name of an app-side callback     |
| `onLongClick`   | name of an app-side callback     |
| `enabled`       | bool                             |

### Stack-specific (`VStack`, `HStack`)

| attribute        | meaning                                                                          |
|------------------|----------------------------------------------------------------------------------|
| `spacing`        | gap between children                                                             |
| `alignItems`     | `start` / `center` / `end` / `stretch`                                           |
| `justifyContent` | `start` / `center` / `end` / `space_between` / `space_around`                    |

### `Grid`-specific

| attribute                          | meaning                                  |
|------------------------------------|------------------------------------------|
| `cols`                             | number of columns (required)             |
| `rows`                             | number of rows (required)                |
| `spacing`                          | uniform gap                              |
| `columnSpacing` / `rowSpacing`     | distinct horizontal / vertical gaps      |

### `Image`-specific

| attribute   | meaning                                                                       |
|-------------|-------------------------------------------------------------------------------|
| `src`       | required; must start with `@drawable/`. Resolves through the package's drawable cache, or — when allowed — another package's. |
| `width`     | size                                                                          |
| `height`    | size                                                                          |

## 5. Units

- `dp` — density-independent pixel. `1 dp = 1/160 in` ≈ 1 pixel
  on a 160 dpi screen.
- `sp` — scale-independent pixel. Like `dp` but multiplied by the
  user's font scale.
- `px` — raw pixel. **Discouraged** — does not adapt to density.
- `%` — percent of the parent (on `width`, `height`, `padding`).
- `match_parent` — fill all available space.
- `wrap_content` — only the size needed by the content.

## 6. Resource references

### Supported types

| prefix         | example                | meaning                                                      |
|----------------|------------------------|--------------------------------------------------------------|
| `@string/`     | `@string/welcome`      | from `res/values/strings.toml`                               |
| `@color/`      | `@color/primary`       | from `res/values/colors.toml`                                |
| `@dimen/`      | `@dimen/spacing_medium`| from `res/values/dimens.toml` *(reserved — not parsed today)* |
| `@drawable/`   | `@drawable/icon`       | from `res/drawable/icon.*`                                   |
| `@style/`      | `@style/op_button`     | from `res/values/styles.toml` *(reserved)*                   |
| `@layout/`     | `@layout/list_item`    | from `res/layout/list_item.akml` (used in API, not in AKML) |
| `@+id/foo`     | `@+id/display`         | **define** a new id                                          |
| `@id/foo`      | `@id/display`          | **use** an existing id                                       |

### Literal values

If you don't use a reference, a literal works:

- Colours: `#RRGGBB`, `#AARRGGBB`, or `rgb(R, G, B)`.
- Strings: regular text inside the attribute.
- Numbers: `"42"`, `"3.14"`.

**Recommendation.** Prefer references even when they look
unnecessary — they are what makes i18n, dark mode, and other
qualifier dimensions Just Work.

## 7. Styles *(planned)*

Like Android — a set of attributes shared and inheritable.

### Definition in `res/values/styles.toml`

```toml
[styles."op_button"]
background = "@color/op_bg"
textColor  = "@color/on_op"
padding    = "16dp"
textSize   = "20sp"

[styles."number_button"]
background = "@color/number_bg"
textColor  = "@color/on_number"
padding    = "16dp"
textSize   = "20sp"
fontWeight = "bold"
```

### Use in AKML

```xml
<Button text="7" style="@style/number_button" onClick="on_digit" tag="7" />
<Button text="+" style="@style/op_button"     onClick="on_op"    tag="add" />
```

Attributes set directly on the element override style attributes.
**Status:** the `style="@style/..."` attribute parses, but the
resource manager does not yet load `styles.toml`, so references
resolve to nothing. Use direct attributes for now.

## 8. Callbacks

A `onClick` (and friends) attribute holds the name of a function
in the app:

```xml
<Button text="Go" onClick="on_go_clicked" tag="primary" />
```

In Rust:

```rust
#[akiva::callback]
fn on_go_clicked(app: &mut MyApp, ctx: &CallbackContext) {
    let tag = ctx.tag();   // "primary"
}
```

The SDK records the callback in an internal registry. At inflate
time the inflater wires `widget → callback id` and hands the map
to the runtime.

## 9. Full example — Calculator

### `res/layout/main.akml`

```xml
<?akml version="1.0"?>
<VStack
    padding="16dp"
    background="@color/surface"
    spacing="8dp">

    <Text
        id="@+id/display"
        text="@string/zero"
        textSize="48sp"
        textColor="@color/on_surface"
        textAlign="end"
        padding="16dp"
        background="@color/display_bg"
        flex="0" />

    <Grid cols="4" rows="5" spacing="8dp" flex="1">
        <Button text="@string/clear" onClick="on_clear"   tag="C"   />
        <Button text="±"             onClick="on_sign"               />
        <Button text="%"             onClick="on_percent"            />
        <Button text="÷"             onClick="on_op"      tag="div" />

        <Button text="7" onClick="on_digit" tag="7" />
        <Button text="8" onClick="on_digit" tag="8" />
        <Button text="9" onClick="on_digit" tag="9" />
        <Button text="×" onClick="on_op"    tag="mul" />

        <Button text="4" onClick="on_digit" tag="4" />
        <Button text="5" onClick="on_digit" tag="5" />
        <Button text="6" onClick="on_digit" tag="6" />
        <Button text="−" onClick="on_op"    tag="sub" />

        <Button text="1" onClick="on_digit" tag="1" />
        <Button text="2" onClick="on_digit" tag="2" />
        <Button text="3" onClick="on_digit" tag="3" />
        <Button text="+" onClick="on_op"    tag="add" />

        <Button text="0" onClick="on_digit" tag="0" />
        <Button text="." onClick="on_dot"            />
        <Button text="=" onClick="on_equals"         />
    </Grid>
</VStack>
```

The Rust side that goes with it:

```rust
use akiva_sdk::prelude::*;

#[akiva::app]
struct Calculator {
    display: String,
    operand: f64,
    op: Option<Operation>,
}

impl Activity for Calculator {
    async fn on_create(&mut self, ctx: &mut Context<'_>) {
        ctx.set_content_view("@layout/main");
    }
}

#[akiva::callback]
fn on_digit(app: &mut Calculator, ctx: &CallbackContext) {
    let digit = ctx.tag();
    if app.display == "0" {
        app.display = digit.to_string();
    } else {
        app.display.push_str(digit);
    }
    ctx.update_text("@id/display", &app.display);
}
```

## 10. Parser & inflation

### Stages

```
1. Read AKML bytes.
2. XML tokenizer → events (StartElement, EndElement, Text, Attribute).
3. AST builder → tree of AkmlNode.
4. Validator:
   - Element names known (VStack/HStack/Frame/Grid/Text/Button/Rect/Spacer/Image).
   - Required attributes present (`Grid` needs `cols` + `rows`,
     `Image` needs `src`, `Button` needs `label`, …).
   - Attribute types valid (size, color, ref, …).
5. Resolver:
   - Resolve @string/foo, @color/bar, @drawable/baz.
   - Apply @style/* (planned) — merge attrs from style; element overrides win.
   - Apply qualifiers (today: `night` only) when the layout itself has them.
6. Builder:
   - For each node, call the runtime API to create the widget.
   - Register callbacks in the app's callback registry.
   - Store @+id/* references in IdMap.
7. Return a WidgetTreeRoot + IdMap.
```

### Error reporting

The parser tracks (line, column) for every token. Errors carry a
location:

```
Error in main.akml:15:8
  Unknown attribute 'textBold' on Text widget
  Did you mean 'fontWeight'?
```

## 11. Schema version

```xml
<?akml version="1.0"?>
```

Identifies the schema version. The runtime accepts `1.0` only.
Future versions will be backward-compatible (additive only).

## 12. MVP limits

- No conditional rendering (`<if>`, `<switch>`). The app updates
  state and then `invalidate` / `set_content_view` re-runs.
- No loops (`<for>`). Lists need a `<List>` widget *(planned)*.
- No imports between AKML files — every file stands alone.
- No declarative animations *(planned)*.
- No custom widgets — only the built-in tags above.

## 13. Tooling

- **Validator:** `akiva validate res/layout/main.akml` *(planned —
  `akiva` CLI does not have this subcommand yet).*
- **Formatter:** `akiva fmt` — indentation pass *(planned).*
- **VS Code extension:** [`tools/vscode-akml/`](../tools/vscode-akml/) —
  syntax highlighting, brackets, comments, starter snippets.
  Validation / autocomplete are not in yet.
