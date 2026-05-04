# `.akv` Archive Format (AKPK v1)

> **Implementation status (May 2026):** Phase 8.1 — format,
> encode/decode, and unit tests are all live in
> [`cli/src/archive.rs`](../cli/src/archive.rs). `akiva package` and
> `akiva install` (Phases 8.2–8.3) ride on top. The runtime still
> sources dev apps from `runtime/web/dev-apps/` rather than from
> installed `.akv` archives, so the on-disk format is real but the
> production ingestion path stays gated until the loader closes that
> loop.

## 1. Why a custom format and not ZIP

The Akiva CLI is intentionally stdlib-only — the dev-server is a
hand-written HTTP server, the manifest parser in `build.rs` reads
TOML line-by-line without a parser dependency. Pulling in a `zip`
crate just to bundle three files in a package would break that
discipline.

ZIP's drawbacks for our case:

- It is large — a complete ZIP parser handles EOCD records, the
  central directory, multi-disk volumes, data descriptors,
  encryption, the `0x05054b50` marker, and ZIP64.
- Its strengths (random access, ubiquitous tooling) do not buy us
  much: Akiva always loads the whole archive into RAM at install
  time; `.akv` is not meant to be hand-edited.

AKPK is ~200 lines of Rust including tests, with zero dependencies.
If we ever want compression or random access we bump the version
byte and switch — the format leaves room.

## 2. Wire layout (little-endian)

```
offset  size       field       description
──────  ────────   ─────────   ──────────────────────────────────────
0       4          magic       "AKPK" (0x4B50_4B41 LE bytes 'A','K','P','K')
4       1          version     1
5       4          count       u32 — number of entries
9       *          entries     entries laid out back-to-back

Entry:
0       2          path_len    u16 — length of the path in bytes (UTF-8)
2       path_len   path        UTF-8 bytes; separator is always '/'
        4          data_len    u32 — length of the payload in bytes
        data_len   data        raw bytes
```

The format **forbids any trailing bytes after the last entry**. A
file longer than expected → `TrailingBytes`. This makes corruption
detectable immediately.

## 3. Required content

In v1, every archive must contain at least these two entries:

- `manifest.toml` — the [`manifest.toml`](manifest.md) of the app.
- `app.wasm` — the compiled wasm module.

`decode()` rejects an archive missing either of them with
`MissingRequired("…")`.

## 4. Reserved optional entries

The format itself is content-agnostic — anything that obeys the path
rules below is accepted. By convention these paths are reserved and
consumed by the `PackageManager` at install time:

- `res/values/strings.toml`
- `res/values/colors.toml`
- `res/layout/*.akml` (planned for v0.3 with qualifier directories)
- `res/drawable/*` (planned for v0.3+)

## 5. Path rules

Every path must:

- be valid UTF-8
- be non-empty
- not start with `/`
- not end with `/`
- contain no empty segment (`foo//bar` → invalid)
- contain no `.` or `..` segment (no traversal)
- use `/` exclusively as a separator (no `\`)
- not contain a NUL byte (`\0`)

`foo/bar.txt` is valid. `foo/`, `/foo`, `foo//bar`, `foo/../x`, `..`
— all invalid.

**Uniqueness:** two entries with the same path → `DuplicatePath` at
encode time. There is no "override" semantics — the encoder must
know what it is putting in.

## 6. Size limits

- `path_len` — up to 65 535 UTF-8 bytes (`u16`).
- `data_len` — up to 4 GB (`u32`). A wasm file larger than 4 GB is
  a sign that something far more fundamental has gone wrong.
- `count` — up to 4 294 967 295 entries. Negligible in practice; an
  MVP-typical app has 3–10 entries.
- The total archive size is unbounded; the limit comes from the
  installer's available memory.

## 7. Versioning

Byte at offset 4 is `version = 1`. Any **incompatible** change
(layout shuffle, compression, field-order change) requires a bump.
A decoder seeing an unknown version returns `UnsupportedVersion(n)`
rather than guessing.

Compatible changes (e.g. adding optional reserved entries) do not
require a bump — the decoder simply ignores entries it does not
recognise.

## 8. What v1 deliberately does not include

| feature                         | why not                                                                                                                      | when                                          |
|---------------------------------|------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------|
| Compression                     | wasm is already optimised (`opt-level="z"`, `strip=true`); deflate margin is small. TOML files are tiny.                      | v2, if app sizes start crossing several MB.   |
| Signatures (`signature.sig`)    | [`docs/manifest.md §9`](manifest.md) flags this as post-MVP.                                                                  | v0.3+ (code signing).                         |
| Per-entry CRC32                 | TCP and OPFS already deliver clean reads; in-flight corruption surfaces as `Truncated` / `BadMagic`.                          | v2, if silent corruption ever shows up.       |
| Random access                   | The whole archive is loaded into RAM anyway.                                                                                  | not planned.                                  |
| Directory entries               | The PackageManager creates paths from entries; no need to represent an empty directory.                                       | not planned.                                  |
| Symlinks, mode bits             | irrelevant for wasm packages.                                                                                                  | not planned.                                  |

## 9. Example: calculator archive

Logical contents:

```
manifest.toml             742 bytes
app.wasm               169,279 bytes
res/values/colors.toml    378 bytes
```

Binary layout (schematic):

```
"AKPK"                               # magic
0x01                                 # version
0x03 0x00 0x00 0x00                  # count = 3

0x0d 0x00                            # path_len = 13
"manifest.toml"                      # path (13 bytes)
0xe6 0x02 0x00 0x00                  # data_len = 742
<manifest bytes, 742>

0x08 0x00                            # path_len = 8
"app.wasm"                           # path
0x7f 0x95 0x02 0x00                  # data_len = 169279
<wasm bytes, 169279>

0x16 0x00                            # path_len = 22
"res/values/colors.toml"             # path
0x7a 0x01 0x00 0x00                  # data_len = 378
<colors.toml bytes, 378>
```

Total: `4 + 1 + 4 + (2 + 13 + 4 + 742) + (2 + 8 + 4 + 169279) +
(2 + 22 + 4 + 378) ≈ 170 KB`.

## 10. Decode errors

The decoder returns `ArchiveError`. Each variant says exactly what
is wrong; `PackageManager::install` reads it and produces a clear
message for the user:

| variant                          | meaning                                                                |
|----------------------------------|------------------------------------------------------------------------|
| `BadMagic`                       | First four bytes are not `AKPK` — the file is not an `.akv`.          |
| `UnsupportedVersion(n)`          | Version byte is not 1 — the archive is too new or corrupt.            |
| `Truncated`                      | A header promises more data but the buffer ends.                       |
| `TrailingBytes`                  | Bytes appear after the last entry — corrupt or padded.                |
| `NonUtf8Path`                    | An entry's path is not valid UTF-8.                                    |
| `InvalidPath(String)`            | A path violates the §5 rules.                                          |
| `DuplicatePath(String)`          | Two entries share a path.                                              |
| `MissingRequired(&'static str)`  | `manifest.toml` or `app.wasm` is missing.                              |
| `PathTooLong(String)`            | Path exceeds 65 535 bytes.                                             |
| `FileTooLarge(String)`           | Entry exceeds 4 GB.                                                    |

## 11. Magic bytes vs known formats

`AKPK` at offset 0 does not collide with any well-known signature:

- ZIP: `PK\x03\x04` (`50 4B 03 04`)
- tar: `"ustar"` at offset 257
- gzip: `1F 8B`
- wasm: `\0asm` (`00 61 73 6D`)
- Android APK: ZIP magic

The standard `file(1)` magic database does not currently identify
the format; an entry can be added to a local `/etc/magic.d/` if it
ever becomes useful.

## 12. Extraction safety (mandatory reading for extractors)

Zip Slip is a class of vulnerability where a malicious archive entry
with a path like `../../etc/passwd` tricks the extractor into
writing outside the target directory. CVEs have been filed against
tar, jar, war, cpio, apk, and 7z extractors. Akiva applies two
layers of defence — both must be implemented.

**Layer 1: the format itself rejects malicious paths.** §5 lists
every rule the decoder enforces: no absolute paths, no `..`
component, no backslash, no NUL, no empty components.
`archive::decode()` returns `InvalidPath(...)` before the extractor
ever sees the entries.

**Layer 2: the extractor must canonicalize and enforce
containment.** Even with layer 1 sound, an extractor that relies
solely on the format becomes unsafe if (a) a future version of AKPK
relaxes the §5 rules, or (b) an implementation bug slips. The
standard pattern:

```rust
fn extract_entry(target_dir: &Path, entry: &Entry) -> io::Result<PathBuf> {
    let out_path = target_dir.join(&entry.path);

    // Canonicalize and enforce containment — same pattern Android's
    // docs recommend for ZIP extraction (see 'Zip Path Traversal').
    let canon_target = target_dir.canonicalize()?;
    let canon_out = out_path.parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad path"))?
        .canonicalize()?;
    if !canon_out.starts_with(&canon_target) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("entry escapes target: {:?}", entry.path),
        ));
    }
    // …write data…
    Ok(out_path)
}
```

In Akiva today the only extractor is `PackageManager::install`
(Phase 8.2+), which does not write to the filesystem at all — it
writes to the kernel's KV store under `packages/{id}/…`. The same
rule applies there: an entry's path must be validated *again* before
it is concatenated into a storage key, because the kernel storage
namespace is shared.

## 13. Forward-looking: Component Model and Bindle

The WebAssembly Component Model is moving fast in 2026
([WASI 0.3](https://eunomia.dev/blog/2025/02/16/wasi-and-the-webassembly-component-model-current-status/),
[Bytecode Alliance `wkg`](https://component-model.bytecodealliance.org/)).
[Bindle](https://github.com/deislabs/bindle) is a manifest format
designed specifically for distributing components plus their
dependencies.

Akiva defers Component Model adoption to v0.3+ (see
[`docs/research-component-model.md`](research-component-model.md)).
Until then our apps are core wasm modules
(`wasm32-unknown-unknown`) rather than components, and AKPK matches
that shape exactly: 3–5 files per package, install-from-disk, no
inter-package dependencies.

When we do migrate, two doors are open:

1. **AKPK v2** — extend the format (bump the version byte) for
   components plus a manifest of dependencies. Preserves the
   "one archive installs one app" mental model.
2. **Bindle adoption** — adopt the community-standard format for
   future registry compatibility.

The decision is not now. Both are reachable because the version
byte = 1 today and `min_abi_version` in the manifest already handles
app-level compatibility decisions.
