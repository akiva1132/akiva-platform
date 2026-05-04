# `.akv` Archive Format (AKPK v1)

> **סטטוס מימוש:** Phase 8.1 — פורמט + encode/decode + unit tests פעילים ב-[cli/src/archive.rs](../cli/src/archive.rs). `akiva package` + `akiva install` (Phase 8.2–8.3) יושבים על גבי זה.

## 1. למה פורמט מותאם, לא ZIP

ה-CLI של Akiva הוא stdlib-only בכוונה — ה-devserver הוא שרת HTTP שכתבנו בעצמנו, ה-manifest parser ב-`build.rs` סורק שורה-שורה בלי TOML parser. להביא crate כמו `zip` רק כדי לארוז שלושה קבצים בחבילה מפרה את העיקרון הזה.

חסרונות של ZIP לטובת המקרה שלנו:
- הוא גדול — ZIP parser מלא טומן טיפול ב-EOCD record, central directory, multi-disk, data descriptors, encryption, 0x05054b50 ו-ZIP64.
- יתרונותיו (random access, tools מוכרים) לא קריטיים: Akiva תמיד טוענת את כל ה-archive ל-RAM ב-install; ה-`.akv` לא נועד לעריכה ידנית.

AKPK הוא ~200 שורות Rust כולל tests, ו-0 תלויות. אם יום אחד נרצה compression או random access נעבור ל-ZIP דרך bump של version byte — הפורמט תומך בזה.

## 2. מבנה ה-wire (little-endian)

```
offset  size       field       description
──────  ────────   ─────────   ──────────────────────────────────────
0       4          magic       "AKPK" (0x4B50_4B41 LE bytes 'A','K','P','K')
4       1          version     1
5       4          count       u32 — מספר entries
9       *          entries     רצף של entries ברצף

Entry:
0       2          path_len    u16 — אורך ה-path בבתים (UTF-8)
2       path_len   path        UTF-8 bytes, separator תמיד '/'
        4          data_len    u32 — אורך התוכן בבתים
        data_len   data        raw bytes
```

הפורמט **לא מתיר bytes נוספים אחרי ה-entry האחרון**. קובץ ארוך יותר מהצפוי → `TrailingBytes`. זה מאפשר לאתר שחיתות מיידית.

## 3. תוכן נדרש

ב-v1 חובה שיכילו שני entries:

- `manifest.toml` — תוכן ה-[`manifest.toml`](manifest.md) של האפליקציה
- `app.wasm` — ה-wasm module הקומפלציוני של האפליקציה

ה-decode דוחה archive שאין בו את שניהם עם `MissingRequired("…")`.

## 4. Entries אופציונליים מוכרים

הפורמט עצמו agnostic — כל מה שעומד בכללי ה-path חוקי. לפי convention, הנתיבים הבאים שמורים ומוזנים ע"י ה-PackageManager בזמן install:

- `res/values/strings.toml`
- `res/values/colors.toml`
- `res/layout/*.akml` (עתידי, v0.3 qualifiers)
- `res/drawable/*` (עתידי, v0.3+)

## 5. כללי path

כל path חייב:

- להיות UTF-8 תקין
- לא ריק
- לא להתחיל ב-`/`
- לא להסתיים ב-`/`
- לא להכיל רכיב ריק (`foo//bar` → פסול)
- לא להכיל רכיב `.` או `..` (שום traversal)
- להשתמש ב-`/` בלבד כמפריד (לא `\`)
- לא להכיל NUL (`\0`)

Path `foo/bar.txt` חוקי. `foo/` / `/foo` / `foo//bar` / `foo/../x` / `..` — כולם פסולים.

**Uniqueness:** שני entries עם אותו path בדיוק → `DuplicatePath` ב-encode. לא נקראים "override" — מי שבונה את ה-archive חייב לדעת מה הוא מכניס.

## 6. הגבלות גודל

- `path_len` — עד 65 535 בתים UTF-8 (u16)
- `data_len` — עד 4 GB (u32). קובץ wasm גדול מ-4 GB הוא סימן שמשהו הרבה יותר יסודי לא בסדר.
- `count` — עד 4 294 967 295 entries. זניח בפועל; MVP אפליקציה טיפוסית תכיל 3-10 entries.
- סך ה-archive לא חסום — limit בא מהזיכרון הזמין של המתקין.

## 7. Versioning

ה-byte בעמדה 4 הוא `version = 1`. כל שינוי **לא-תואם** (פריסה שונה, compression, סדר שדות) דורש בump. Decoder שרואה version לא מוכר מחזיר `UnsupportedVersion(n)` ולא מנחש.

תואמות-תואם (למשל, הוספת entries אופציונליים) לא דורשת bump — ה-decoder רק מתעלם מ-entries שלא מוכרים לו.

## 8. מה **לא** ב-v1

| תכונה | למה לא | מתי |
|---|---|---|
| Compression | ה-wasm כבר עבר optimize (`opt-level="z"`, `strip=true`), רווח deflate עליו שולי. קבצי TOML זניחים. | אם אפליקציות יגיעו לגדלים שבהם חבילה מעל כמה MB — v2. |
| Signatures (`signature.sig`) | [`docs/manifest.md §9`](manifest.md) מציין post-MVP | v0.3+ (code signing) |
| Checksums פנימיים (CRC32 פר-entry) | התחייבות TCP ו-OPFS כבר מספקת טעינה נקייה; שחיתות ב-flight → `Truncated` / `BadMagic` מזהים | אם יתגלו שחיתות שקטות — v2 |
| Random access | כל ה-archive נטען ל-RAM בכל אופן | לא מתוכנן |
| Directory entries | ה-PackageManager יוצר נתיבים לפי ה-entries — אין צורך לייצג תיקייה ריקה | לא מתוכנן |
| Symlinks, mode bits | לא רלוונטי ל-wasm packages | לא מתוכנן |

## 9. דוגמה: archive של calculator

מבנה לוגי:

```
manifest.toml             742 bytes
app.wasm               169,279 bytes
res/values/colors.toml    378 bytes
```

Binary layout (schematically):

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

סה"כ: `4 + 1 + 4 + (2 + 13 + 4 + 742) + (2 + 8 + 4 + 169279) + (2 + 22 + 4 + 378) = ~170 KB`.

## 10. שגיאות decode

ה-API מחזיר `ArchiveError`. כל variant מציין בדיוק מה לא בסדר — הקוד קורא ב-`PackageManager::install` ועונה הודעה בהירה למשתמש:

| Variant | משמעות |
|---|---|
| `BadMagic` | ארבעת הבתים הראשונים לא `AKPK` — הקובץ אינו `.akv` |
| `UnsupportedVersion(n)` | Version byte לא 1 — archive חדש מדי או פגום |
| `Truncated` | ה-header מציין עוד נתונים אבל הגענו לסוף הבאפר |
| `TrailingBytes` | בתים אחרי ה-entry האחרון — פגום |
| `NonUtf8Path` | ה-path של entry מסוים אינו UTF-8 |
| `InvalidPath(String)` | ה-path מפר את כללי §5 |
| `DuplicatePath(String)` | שני entries עם אותו path |
| `MissingRequired(&'static str)` | `manifest.toml` או `app.wasm` חסרים |
| `PathTooLong(String)` | path עולה על 65 535 בתים |
| `FileTooLarge(String)` | entry עולה על 4 GB |

## 11. Magic bytes במערכות שזיהוי קבצים

`AKPK` במיקום 0 לא מתנגש עם חתימות מוכרות:

- ZIP: `PK\x03\x04` (50 4B 03 04)
- tar: "ustar" ב-offset 257
- gzip: `1F 8B`
- wasm: `\0asm` (00 61 73 6D)
- אנדרואיד APK: ZIP magic

כלי `file(1)` עם ה-database המוגדרת לא יזהה את הפורמט כיום — ב-v0.3 נוסיף entry ל-`/etc/magic.d/` מקומי אם נצטרך.

## 12. Extraction safety (קריאה חובה ל-extractors)

Zip Slip הוא מחלקת-חולשות בפורמטי archive שבה entry מזיק עם path כגון `../../etc/passwd` מפתה את ה-extractor לכתוב מחוץ לספריית היעד. CVEs רבים נמצאו ב-tar, jar, war, cpio, apk, 7z. ב-Akiva יש שתי שכבות הגנה — וחובה ליישם את שתיהן.

**שכבה 1: הפורמט עצמו דוחה paths מזיקים.** §5 מגדיר את כל הכללים שה-decoder אוכף: אין absolute paths, אין `..` כרכיב, אין backslash, אין NUL, אין רכיבים ריקים. `archive::decode` מחזיר `InvalidPath(…)` לפני שה-extractor בכלל רואה את ה-entries.

**שכבה 2: ה-extractor חייב לעשות canonicalization + containment check.** גם עם שכבה 1 מוצלחת, extractor שמסתמך רק על הפורמט עובר ללא הגנה אם (א) גרסה עתידית של AKPK תקל על כללי §5, או (ב) באג במימוש הפורמט. הפרקטיקה הסטנדרטית:

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

ב-Akiva ה-extractor היחיד היום הוא `PackageManager::install` (Phase 8.2+), שלא כותב ל-filesystem בכלל — כותב לכ-KV store של ה-kernel תחת `packages/{id}/…`. גם שם אותו כלל חל: path של entry חייב להיות validated *פעם נוספת* לפני שמשתלב ל-storage key, כי האוכל של ה-kernel storage הוא namespace משותף.

## 13. עתיד: Component Model ו-Bindle

WebAssembly Component Model נמצא באימוץ מואץ ב-2026 ([WASI 0.3](https://eunomia.dev/blog/2025/02/16/wasi-and-the-webassembly-component-model-current-status/), [Bytecode Alliance `wkg`](https://component-model.bytecodealliance.org/)). [Bindle](https://github.com/deislabs/bindle) הוא פורמט manifest המיועד ספציפית ל-distribution של components + dependencies.

Akiva דוחה אימוץ Component Model ל-Phase 4 של v0.3+ (ראה [`ROADMAP.md`](../ROADMAP.md) "Phase 4 (ABI)"). עד אז, האפליקציות שלנו הן core wasm modules (`wasm32-unknown-unknown`) ולא components, ו-AKPK מתאים להן בדיוק: שלושה-חמישה קבצים פר-package, התקנה מ-disk, אין dependencies בין חבילות.

כשנעבור ל-Component Model, שתי אפשרויות פתוחות:
1. **AKPK v2** — להרחיב את הפורמט (בamp של version byte) לתמיכה ב-components + manifest של dependencies. משמר את הקריאה הפשוטה "archive אחד מתקין אפליקציה".
2. **Bindle adoption** — לאמץ את הפורמט הסטנדרטי של הקהילה, לקבל תואמות עם registries עתידיים.

ההחלטה לא עכשיו. שתי האפשרויות נתמכות כי version byte = 1 משאיר את כל המרחב ל-future versions, ו-`min_abi_version` במניפסט מטפל בהחלטות תאימות ברמת האפליקציה.
