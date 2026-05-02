//! `default` template — minimal counter using the programmatic `Ui`
//! builder. Identical in spirit to `apps/hello`: paint a title, a
//! counter, and an increment button. Three callbacks are present
//! (`on_create`, `on_pointer`, `on_render`) plus `on_save_state` /
//! `on_restore_state` so the first meaningful edit doesn't require
//! learning the rest of the API at once.

use std::io;
use std::path::Path;

use crate::new::Names;

pub(super) fn write(app_dir: &Path, n: &Names) -> io::Result<()> {
    std::fs::create_dir_all(app_dir.join("src"))?;
    super::write_file(&app_dir.join("Cargo.toml"),    &super::render_cargo_toml(n, "Akiva app."))?;
    super::write_file(&app_dir.join("manifest.toml"), &super::render_manifest(n, &[]))?;
    super::write_file(&app_dir.join("src/lib.rs"),    &render_lib_rs(n))?;
    super::write_file(&app_dir.join(".gitignore"),    super::GITIGNORE)?;
    super::write_file(&app_dir.join("README.md"),     &render_readme(n))?;
    Ok(())
}

fn render_lib_rs(n: &Names) -> String {
    format!(
r#"//! {display} — scaffolded by `akiva new`.
//!
//! Activity is your contract with the runtime. Minimum override is
//! `on_render`; everything else has sensible defaults.

use akiva_sdk::prelude::*;

#[derive(Default)]
pub struct {class} {{
    ui: UiState,
    count: i32,
}}

#[async_trait(?Send)]
impl Activity for {class} {{
    async fn on_create(&mut self, ctx: &mut Context<'_>) {{
        log_info!("{class} created ({{}}x{{}} @ {{:.1}}x)",
            ctx.metrics.width_px, ctx.metrics.height_px, ctx.metrics.density);
    }}

    async fn on_pointer(&mut self, _ctx: &mut Context<'_>, ev: PointerEvent) {{
        // Feed the UI state so it can correlate pointer events with
        // widgets laid out during the next `present()`.
        self.ui.feed_pointer(ev);
    }}

    async fn on_render(&mut self, ctx: &mut Context<'_>, c: &mut Canvas<'_>) {{
        c.clear(0xff0f1420);
        let mut ui = Ui::new(&mut self.ui, ctx, c);

        let title = ui.text("{display}").size(28.0).color(0xfff0f4fa).build();
        let counter = ui.text(&format!("count = {{}}", self.count))
            .size(20.0).color(0xffe0e6f0).build();
        let inc = ui.button("inc", "+1");

        let root = ui.v_stack(&[title, counter, inc])
            .gap(16.0).padding(24.0).build();
        ui.present(root);

        if ui.was_clicked("inc") {{ self.count += 1; }}
    }}

    fn on_save_state(&self, state: &mut SavedState) {{
        state.put_int("count", self.count as i64);
    }}
    fn on_restore_state(&mut self, state: &SavedState) {{
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

fn render_readme(n: &Names) -> String {
    format!(
r#"# {display}

Akiva app scaffolded with `akiva new --template default`.

## Build & run

```sh
akiva build {dir}     # compile this app to wasm
akiva run             # build everything + serve at http://localhost:8080
```

The first run installs the app into `runtime/web/dev-apps/{package_id}/`.
Subsequent edits to `src/lib.rs` need a fresh `akiva build {dir}` and a
browser refresh.

## Layout

* `src/lib.rs` — Activity implementation. The `on_render` callback
  builds the UI tree every frame using `akiva_sdk::ui::Ui`.
* `manifest.toml` — App identity, runtime quotas, intent filter that
  registers this activity as a launcher entry.
* `Cargo.toml` — Standalone crate (apps/ is excluded from the workspace).

## Next steps

* Open `src/lib.rs` and rename `count` to whatever your app actually
  tracks. Add fields to the struct; the `on_save_state` /
  `on_restore_state` plumbing is already wired.
* Try the `compose-style` template (`akiva new mynotes --template
  compose-style`) for an example with declarative AKML layouts and
  resource theming.
* `akiva docs` opens the SDK reference in your browser.
"#,
        display = n.display_name,
        dir = n.dir,
        package_id = n.package_id,
    )
}
