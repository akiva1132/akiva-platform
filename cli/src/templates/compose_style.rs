//! `compose-style` template — declarative UI showcase.
//!
//! Where `default` builds the UI tree imperatively in `on_render`,
//! this template parses an AKML file at boot, inflates a node tree
//! per frame with a fresh `Bindings`, and renders that. The pattern
//! matches `apps/tic-tac-toe` and `apps/calculator`.
//!
//! The scaffolded app is a "greeter" — one click counter, one greeting
//! that cycles through a fixed list (`Hello / Hola / Bonjour / Shalom`),
//! and a reset button. Two resource files (`values/colors.toml` and
//! `values-night/colors.toml`) prove the dark-mode overlay works.

use std::io;
use std::path::Path;

use crate::new::Names;

pub(super) fn write(app_dir: &Path, n: &Names) -> io::Result<()> {
    std::fs::create_dir_all(app_dir.join("src"))?;
    std::fs::create_dir_all(app_dir.join("res/layout"))?;
    std::fs::create_dir_all(app_dir.join("res/values"))?;
    std::fs::create_dir_all(app_dir.join("res/values-night"))?;

    super::write_file(&app_dir.join("Cargo.toml"),    &super::render_cargo_toml(n, "Compose-style Akiva app."))?;
    super::write_file(&app_dir.join("manifest.toml"), &super::render_manifest(n, &["akiva.permission.STORAGE_SANDBOX"]))?;
    super::write_file(&app_dir.join("src/lib.rs"),    &render_lib_rs(n))?;
    super::write_file(&app_dir.join("res/layout/main.akml"),    LAYOUT)?;
    super::write_file(&app_dir.join("res/values/colors.toml"),  COLORS_LIGHT)?;
    super::write_file(&app_dir.join("res/values/strings.toml"), &render_strings(n))?;
    super::write_file(&app_dir.join("res/values-night/colors.toml"), COLORS_NIGHT)?;
    super::write_file(&app_dir.join(".gitignore"),    super::GITIGNORE)?;
    super::write_file(&app_dir.join("README.md"),     &render_readme(n))?;
    Ok(())
}

fn render_lib_rs(n: &Names) -> String {
    format!(
r#"//! {display} — scaffolded by `akiva new --template compose-style`.
//!
//! Declarative UI: `res/layout/main.akml` describes the tree; this
//! file rebuilds a `Bindings` every frame and inflates. Compare with
//! the `default` template, which builds the tree imperatively.

use akiva_sdk::prelude::*;
use akiva_sdk::akml::{{self, Bindings}};
use akiva_sdk::res;

const LAYOUT: &str = include_str!("../res/layout/main.akml");

const GREETINGS: &[&str] = &["Hello", "Hola", "Bonjour", "Shalom"];

#[derive(Default)]
pub struct {class} {{
    ui: UiState,
    /// Index into `GREETINGS`. Stored as `i32` so it round-trips
    /// through `SavedState::put_int` cleanly.
    greeting_idx: i32,
    count: i32,
}}

#[async_trait(?Send)]
impl Activity for {class} {{
    async fn on_create(&mut self, ctx: &mut Context<'_>) {{
        log_info!("{class} created ({{}}x{{}} @ {{:.1}}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density);
    }}

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {{
        self.ui.feed_pointer(ev);
    }}

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {{
        c.clear(res::color("bg_window"));

        let greeting = GREETINGS[(self.greeting_idx as usize) % GREETINGS.len()];
        let bindings = Bindings::new()
            .text("greeting", greeting.to_string())
            .text("count", format!("{{}}", self.count));

        let mut ui = Ui::new(&mut self.ui, ctx, c);
        let root = akml::inflate_str_with(&mut ui, LAYOUT, &bindings)
            .expect("layout is statically valid — inflate failure is a build bug");
        ui.present(root);

        if ui.was_triggered("greet") {{
            self.greeting_idx = (self.greeting_idx + 1) % (GREETINGS.len() as i32);
            self.count += 1;
        }}
        if ui.was_triggered("reset") {{
            self.greeting_idx = 0;
            self.count = 0;
        }}
    }}

    fn on_save_state(&self, state: &mut SavedState) {{
        state.put_int("greeting_idx", self.greeting_idx as i64);
        state.put_int("count", self.count as i64);
    }}
    fn on_restore_state(&mut self, state: &SavedState) {{
        if let Some(v) = state.get_int("greeting_idx") {{
            self.greeting_idx = v as i32;
        }}
        if let Some(v) = state.get_int("count") {{
            self.count = v as i32;
        }}
    }}
}}

akiva_sdk::akiva_app!({class});
"#,
        display = n.display_name,
        class = n.class,
    )
}

const LAYOUT: &str = r#"<?akml version="1.0"?>
<!--
    Compose-style scaffold layout.

    Bindings filled in per frame by src/lib.rs:
      - {greeting}  — cycling label (Hello / Hola / Bonjour / Shalom)
      - {count}     — click counter as a string

    Action names in `onClick` are the stable contract with Rust dispatch
    (`ui.was_triggered("greet")`); button `id`s are free for a11y.
-->
<VStack padding="24" gap="16" cross="stretch">
    <Text text="@string/app_name" size="28" color="@color/title_fg" />
    <Text text="{greeting}, world!" size="36" color="@color/greeting_fg" />
    <Text text="Clicks: {count}" size="18" color="@color/body_fg" />

    <HStack gap="12">
        <Button id="greet" label="@string/cta_greet" onClick="greet" flex="1" />
        <Button id="reset" label="@string/cta_reset" onClick="reset" />
    </HStack>
</VStack>
"#;

const COLORS_LIGHT: &str = r##"# Light theme — selected when the OS / browser reports
# `prefers-color-scheme: light` (the default). Keys here are looked up
# via `@color/<name>` in main.akml and `akiva_sdk::res::color("name")`
# in src/lib.rs.

bg_window    = "#fff5f7fa"
title_fg     = "#ff20242c"
greeting_fg  = "#ff1d4ed8"
body_fg      = "#ff48505c"
"##;

const COLORS_NIGHT: &str = r##"# Dark theme — selected when the OS reports
# `prefers-color-scheme: dark`. Keys must match values/colors.toml; the
# framework merges the night overlay on top of the light defaults at
# resolve time.

bg_window    = "#ff0f1420"
title_fg     = "#fff0f4fa"
greeting_fg  = "#ff60a5fa"
body_fg      = "#ffb0bccd"
"##;

fn render_strings(n: &Names) -> String {
    format!(
r#"# Static UI strings — looked up via `@string/<name>` in main.akml and
# `akiva_sdk::res::string("name")` in src/lib.rs. Adding `values-he/`
# (or any other locale qualifier) here mirrors the night-mode overlay
# pattern once the framework supports locale qualifiers.

app_name  = "{display}"
cta_greet = "Greet"
cta_reset = "Reset"
"#,
        display = n.display_name,
    )
}

fn render_readme(n: &Names) -> String {
    format!(
r#"# {display}

Akiva app scaffolded with `akiva new --template compose-style`.

## What this template demonstrates

* **Declarative layout.** `res/layout/main.akml` describes the UI tree;
  `src/lib.rs` inflates it per frame with a fresh `Bindings`. The Rust
  code stays small — it only wires data into the tree and dispatches
  click actions.
* **Resource theming.** Colors and strings live in `res/values/` so a
  designer can tune them without touching Rust. `@color/title_fg` /
  `@string/app_name` references in the AKML are resolved at inflate
  time.
* **Auto dark mode.** `res/values-night/colors.toml` overrides the
  light palette when the OS reports `prefers-color-scheme: dark`. Flip
  your system theme to see the colors update on the next frame —
  nothing in `src/lib.rs` had to change.
* **Saved state.** `on_save_state` / `on_restore_state` persist the
  click counter through process death (e.g. crash + relaunch).

## Build & run

```sh
akiva build {dir}
akiva run
```

## Where to look next

* `res/layout/main.akml` — change widgets, swap stacks for a Grid,
  add a Text or Button.
* `res/values/colors.toml` — tweak the palette; the same keys must
  exist in `values-night/` for dark mode to find an override.
* `src/lib.rs` — add a field to the struct, wire it into a binding,
  bump the saved state schema accordingly.
* `akiva docs` — opens the SDK reference.
"#,
        display = n.display_name,
        dir = n.dir,
    )
}
