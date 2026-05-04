# AKML — Akiva Markup Language

## 1. מטרה

AKML היא שפת תיאור דקלרטיבית לממשק משתמש באפליקציות Akiva. כמו `XML layouts` באנדרואיד — UI נכתב בקובץ חיצוני, מפורש בזמן ריצה ע"י `LayoutInflater`, והאפליקציה (קוד Rust/wasm) רק מגיבה ל-events.

## 2. עקרונות

1. **דקלרטיבי**: מתאר "מה צריך להיות", לא "מה לעשות".
2. **נפרד מהקוד**: UI לא נכתב ב-Rust, הוא נכתב ב-AKML.
3. **Resource-aware**: `@string/`, `@color/`, `@dimen/` — הפניות אוטומטיות.
4. **Qualifier-aware**: `res/layout-land/main.akml` נטען אוטומטית במצב אופקי.
5. **Type-checked ב-inflate time**: AKML לא תקין → שגיאה לפני שהאפליקציה רצה.

## 3. תחביר

AKML מבוסס XML. הבחירה ב-XML:
- הוכח במשך 20 שנה באנדרואיד
- Tooling מסיבי (שגיאה/לינטינג/syntax highlighting)
- Schema validation פשוט
- מוכר למפתחי mobile

### מבנה בסיסי

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

כל element הוא widget. ה-root חייב להיות widget אחד בלבד.

**Containers:**
- `<VStack>` — vertical stack
- `<HStack>` — horizontal stack
- `<Grid columns="N" rows="M">`
- `<ScrollView>` — יכול לגלול (נוסף ב-v0.2)
- `<Frame>` — stacking (z-order)

**Leaf widgets:**
- `<Text>`
- `<Button>`
- `<Image>`
- `<Input>` (v0.2)
- `<Spacer>`

## 4. Attributes

### Universal attributes

| Attribute | Type | דוגמה | פירוש |
|---|---|---|---|
| `id` | string | `@+id/name` | מזהה ייחודי ב-tree (רק אם צריך) |
| `width` | size | `match_parent`, `wrap_content`, `100dp`, `50%` | רוחב |
| `height` | size | כנ"ל | גובה |
| `padding` | spacing | `16dp`, `8dp 16dp`, `8dp 16dp 8dp 16dp` | top right bottom left |
| `margin` | spacing | כנ"ל | רווח חיצוני |
| `background` | color | `@color/surface`, `#FF0000` | צבע רקע |
| `flex` | float | `1.0`, `0.5` | משקל ב-stack |
| `tag` | string | `"9"`, `"op_mul"` | מידע שמועבר ל-callback |
| `visible` | bool | `true` \| `false` \| `gone` | הצגה (`gone` = לא תופס מקום) |

### Text-specific

| Attribute | Type | פירוש |
|---|---|---|
| `text` | string / ref | תוכן |
| `textSize` | size | גודל פונט |
| `textColor` | color | צבע טקסט |
| `textAlign` | enum | `start`, `center`, `end`, `justify` |
| `fontWeight` | enum | `normal`, `bold` |
| `maxLines` | int | מספר שורות מקסימלי |
| `ellipsize` | enum | `none`, `end` |

### Button-specific

| Attribute | פירוש |
|---|---|
| `onClick` | שם callback ב-app |
| `onLongClick` | שם callback |
| `enabled` | bool |

### Stack-specific

| Attribute | פירוש |
|---|---|
| `spacing` | רווח בין ילדים |
| `alignItems` | `start` \| `center` \| `end` \| `stretch` |
| `justifyContent` | `start` \| `center` \| `end` \| `space_between` \| `space_around` |

### Grid-specific

| Attribute | פירוש |
|---|---|
| `columns` | מספר עמודות |
| `rows` | מספר שורות (אופציונלי — auto אם לא ניתן) |
| `spacing` | רווח אחיד |
| `columnSpacing` / `rowSpacing` | רווחים נפרדים |

## 5. יחידות (Units)

- `dp` — density-independent pixel. `1dp = 1/160 של אינץ'` ≈ 1 פיקסל במסך 160dpi.
- `sp` — scale-independent pixel. כמו dp אבל מוכפל ב-user font scale.
- `px` — פיקסל גולמי. **לא מומלץ** — לא מתאים להחלפת צפיפויות.
- `%` — אחוז מה-parent (על `width`, `height`, `padding`).
- `match_parent` — מילוי כל המקום הזמין.
- `wrap_content` — רק את הגודל הנדרש לתוכן.

## 6. Resource References

### Types נתמכים

| Prefix | דוגמה | משמעות |
|---|---|---|
| `@string/` | `@string/welcome` | מ-`res/values/strings.toml` |
| `@color/` | `@color/primary` | מ-`res/values/colors.toml` |
| `@dimen/` | `@dimen/spacing_medium` | מ-`res/values/dimens.toml` |
| `@drawable/` | `@drawable/icon` | מ-`res/drawable/icon.*` |
| `@style/` | `@style/op_button` | מ-`res/values/styles.toml` |
| `@layout/` | `@layout/list_item` | מ-`res/layout/list_item.akml` (לא ב-AKML עצמה, רק ב-API) |
| `@+id/foo` | `@+id/display` | **יצירת** id חדש |
| `@id/foo` | `@id/display` | **שימוש** ב-id קיים (אופציונלי) |

### Literal values

אם לא משתמשים ב-reference, אפשר literal:
- צבעים: `#RRGGBB` או `#AARRGGBB` או `rgb(R,G,B)`
- strings: טקסט רגיל בתוך attribute
- מספרים: `"42"`, `"3.14"`

**המלצה:** תמיד להשתמש ב-references, גם אם זה נראה מיותר. זה מה שנותן תמיכה ב-i18n, dark mode, וכו'.

## 7. Styles

כמו ב-Android — סט של attributes שיכולים לעבור בירושה.

### הגדרה ב-`res/values/styles.toml`

```toml
[styles."op_button"]
background = "@color/op_bg"
textColor = "@color/on_op"
padding = "16dp"
textSize = "20sp"

[styles."number_button"]
background = "@color/number_bg"
textColor = "@color/on_number"
padding = "16dp"
textSize = "20sp"
fontWeight = "bold"
```

### שימוש ב-AKML

```xml
<Button text="7" style="@style/number_button" onClick="on_digit" tag="7" />
<Button text="+" style="@style/op_button" onClick="on_op" tag="add" />
```

Attributes שמוגדרים ישירות ב-element מנצחים את ה-style.

## 8. Callbacks

attribute של `onClick` (וכדומה) מכיל שם של פונקציה באפליקציה:

```xml
<Button text="Go" onClick="on_go_clicked" tag="primary" />
```

ב-Rust:

```rust
#[akiva::callback]
fn on_go_clicked(app: &mut MyApp, tag: &str) {
    // tag = "primary"
}
```

ה-SDK רושם את ה-callback ב-registry פנימי. בזמן inflate, ה-LayoutInflater מחבר widget → callback ID ומעביר ל-runtime.

## 9. דוגמה מלאה — Calculator

### res/layout/main.akml

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

    <Grid columns="4" spacing="8dp" flex="1">
        <Button text="@string/clear" onClick="on_clear" tag="C" style="@style/special_button" />
        <Button text="±" onClick="on_sign" style="@style/special_button" />
        <Button text="%" onClick="on_percent" style="@style/special_button" />
        <Button text="÷" onClick="on_op" tag="div" style="@style/op_button" />

        <Button text="7" onClick="on_digit" tag="7" style="@style/num_button" />
        <Button text="8" onClick="on_digit" tag="8" style="@style/num_button" />
        <Button text="9" onClick="on_digit" tag="9" style="@style/num_button" />
        <Button text="×" onClick="on_op" tag="mul" style="@style/op_button" />

        <Button text="4" onClick="on_digit" tag="4" style="@style/num_button" />
        <Button text="5" onClick="on_digit" tag="5" style="@style/num_button" />
        <Button text="6" onClick="on_digit" tag="6" style="@style/num_button" />
        <Button text="−" onClick="on_op" tag="sub" style="@style/op_button" />

        <Button text="1" onClick="on_digit" tag="1" style="@style/num_button" />
        <Button text="2" onClick="on_digit" tag="2" style="@style/num_button" />
        <Button text="3" onClick="on_digit" tag="3" style="@style/num_button" />
        <Button text="+" onClick="on_op" tag="add" style="@style/op_button" />

        <Button text="0" onClick="on_digit" tag="0" style="@style/num_button" />
        <Button text="." onClick="on_dot" style="@style/num_button" />
        <Button text="=" onClick="on_equals" style="@style/equals_button" />
    </Grid>
</VStack>
```

ה-Rust code שמלווה:

```rust
use akiva_sdk::prelude::*;

#[akiva::app]
struct Calculator {
    display: String,
    operand: f64,
    op: Option<Operation>,
}

impl App for Calculator {
    fn new() -> Self {
        Self {
            display: "0".to_string(),
            operand: 0.0,
            op: None,
        }
    }

    fn on_create(&mut self, ctx: &ActivityContext) {
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

// ... שאר ה-callbacks
```

## 10. Parser & Inflation

### שלבי ה-inflater

```
1. Read akml bytes
2. XML tokenizer → events (StartElement, EndElement, Text, Attribute)
3. AST builder → tree of AkmlNode
4. Validator:
   - Check element names are known
   - Check required attributes
   - Check attribute types (size, color, ref)
5. Resolver:
   - Resolve @string/foo, @color/bar, @dimen/baz
   - Apply @style/* (merge attrs from style, element overrides win)
   - Apply qualifiers if layout itself has qualifiers
6. Builder:
   - For each node, call runtime API to create widget
   - Register callbacks in app's callback registry
   - Store @+id/* references in IdMap
7. Return WidgetTreeRoot + IdMap
```

### Error reporting

ה-parser מחזיק מיקום (שורה, עמודה) לכל token. שגיאות יש להן לוקציה:

```
Error in main.akml:15:8
  Unknown attribute 'textBold' on Text widget
  Did you mean 'fontWeight'?
```

## 11. Schema Version

```xml
<?akml version="1.0"?>
```

מסמל את גרסת ה-schema. ה-runtime מקבל v1.0 בלבד כרגע. גרסאות עתידיות יהיו backward compatible בתוספת בלבד.

## 12. מגבלות MVP

- אין conditional rendering (אין `<if>`, `<switch>`). האפליקציה משנה state ואז `invalidate` / `set_content_view` מחדש.
- אין loops (אין `<for>`). רשימות תידרשנה `<List>` widget (ב-v0.2).
- אין imports בין קבצי AKML — כל קובץ standalone.
- אין animations declarative (ב-v0.3).
- אין custom widgets ב-AKML (רק built-ins של ה-framework).

## 13. כלי פיתוח

- **Validator**: `akiva validate res/layout/main.akml`
- **Formatter**: `akiva fmt` — מיישר indentation
- **VS Code extension** (עתידי): syntax highlighting, autocomplete
