# AKML Language Support

Local VS Code extension for Akiva Markup Language (`.akml`).

## What it adds

- syntax highlighting for tags, attributes, strings, bindings, and resource refs
- block comments (`<!-- ... -->`)
- bracket/autoclose support
- a few starter snippets

## Install locally

From this directory:

```bash
npx @vscode/vsce package
```

Then in VS Code:

1. Open Extensions
2. Click the `...` menu
3. Choose `Install from VSIX...`
4. Select the generated `.vsix`

## Notes

This is intentionally lightweight:

- no formatter yet
- no validation/linting yet
- no completion for widget names/resources yet

Those can be added later once the AKML surface is more stable.
