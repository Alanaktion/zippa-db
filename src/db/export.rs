//! Turning a result into text: CSV, TSV, JSON, Markdown, and SQL `INSERT`,
//! plus the single-column shapes the clipboard wants.
//!
//! Pure formatting — no UI and no database. The table view, the grid, and the
//! file saver hand it the columns, the driver's type names, and the rows, and
//! it hands back the text to write or copy.
//!
//! A cell the driver could not read back (`query::is_placeholder`) is a
//! description of a value rather than the value itself, so every format writes
//! `NULL` there and [`Rendered::skipped`] says how many did so.

use super::config::Engine;
use super::query::{self, Cell};
use super::sql::{quote_identifier, quote_literal_for};

/// The layout a result is written or copied in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Tsv,
    Csv,
    Json,
    Markdown,
    Sql,
}

impl Format {
    /// The formats a file export offers, in the order a menu shows them.
    pub const FILE: [Format; 3] = [Format::Csv, Format::Json, Format::Sql];

    /// The file extension a saved export gets.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Tsv => "tsv",
            Format::Csv => "csv",
            Format::Json => "json",
            Format::Markdown => "md",
            Format::Sql => "sql",
        }
    }

    /// The format's short name, for a menu.
    pub fn label(self) -> &'static str {
        match self {
            Format::Tsv => "TSV",
            Format::Csv => "CSV",
            Format::Json => "JSON",
            Format::Markdown => "Markdown",
            Format::Sql => "SQL INSERT",
        }
    }
}

/// What came out: the text to write, and how many cells were written as `NULL`
/// because the driver had only a description of their value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub skipped: usize,
}

/// Lay `rows` out in `format`, under `columns` and their driver type names.
pub fn render(
    format: Format,
    engine: Engine,
    table: &str,
    columns: &[String],
    types: &[String],
    rows: &[Vec<Cell>],
) -> Rendered {
    match format {
        Format::Tsv => render_tsv(columns, rows),
        Format::Csv => render_csv(columns, rows),
        Format::Json => render_json(columns, types, rows),
        Format::Markdown => render_markdown(columns, rows),
        Format::Sql => render_sql(engine, table, columns, types, rows),
    }
}

/// A header line, then one line per row, columns separated by tabs. Tabs and
/// line breaks inside a value become spaces so one row stays one line, which is
/// lossy and why the menu says so.
fn render_tsv(columns: &[String], rows: &[Vec<Cell>]) -> Rendered {
    let mut skipped = 0;
    let mut text = String::new();

    text.push_str(
        &columns
            .iter()
            .map(|name| tsv_field(name))
            .collect::<Vec<String>>()
            .join("\t"),
    );
    text.push('\n');

    for row in rows {
        let fields: Vec<String> = (0..columns.len())
            .map(|index| match row.get(index) {
                Some(cell) if query::is_placeholder(cell) => {
                    skipped += 1;
                    String::new()
                }
                Some(Some(value)) => tsv_field(value),
                _ => String::new(),
            })
            .collect();
        text.push_str(&fields.join("\t"));
        text.push('\n');
    }

    Rendered { text, skipped }
}

/// Tabs and line breaks become spaces so one row stays one line.
fn tsv_field(value: &str) -> String {
    value.replace(['\t', '\n', '\r'], " ")
}

/// A header row, then one line per row, with `NULL` and placeholders empty.
fn render_csv(columns: &[String], rows: &[Vec<Cell>]) -> Rendered {
    let mut skipped = 0;
    let mut text = String::new();

    text.push_str(
        &columns
            .iter()
            .map(|name| csv_field(name))
            .collect::<Vec<String>>()
            .join(","),
    );
    text.push('\n');

    for row in rows {
        let fields: Vec<String> = (0..columns.len())
            .map(|index| match row.get(index) {
                Some(cell) if query::is_placeholder(cell) => {
                    skipped += 1;
                    String::new()
                }
                Some(Some(value)) => csv_field(value),
                _ => String::new(),
            })
            .collect();
        text.push_str(&fields.join(","));
        text.push('\n');
    }

    Rendered { text, skipped }
}

/// Quote a CSV field that holds a comma, a quote, or a line break.
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

/// A Markdown table: a header row, a dash separator, then one row per result,
/// every cell padded to its column's width so the source reads in plain text.
fn render_markdown(columns: &[String], rows: &[Vec<Cell>]) -> Rendered {
    let mut skipped = 0;
    if columns.is_empty() {
        return Rendered {
            text: String::new(),
            skipped,
        };
    }

    let header: Vec<String> = columns.iter().map(|name| markdown_field(name)).collect();
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..columns.len())
                .map(|index| match row.get(index) {
                    Some(cell) if query::is_placeholder(cell) => {
                        skipped += 1;
                        String::new()
                    }
                    Some(Some(value)) => markdown_field(value),
                    _ => String::new(),
                })
                .collect()
        })
        .collect();

    let widths: Vec<usize> = (0..columns.len())
        .map(|index| {
            let mut width = display_width(&header[index]);
            for row in &cells {
                width = width.max(display_width(&row[index]));
            }
            // A Markdown table wants at least three dashes under each header.
            width.max(3)
        })
        .collect();

    let mut text = String::new();
    text.push_str(&markdown_line(
        header
            .iter()
            .zip(&widths)
            .map(|(cell, width)| pad(cell, *width)),
    ));
    text.push('\n');
    text.push_str(&markdown_line(
        widths.iter().map(|width| "-".repeat(*width)),
    ));
    text.push('\n');
    for row in &cells {
        text.push_str(&markdown_line(
            row.iter()
                .zip(&widths)
                .map(|(cell, width)| pad(cell, *width)),
        ));
        text.push('\n');
    }

    Rendered { text, skipped }
}

/// Escape what a table cell cannot hold as written, and turn a line break into
/// the `<br>` a table row can carry.
fn markdown_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], "<br>")
}

fn markdown_line(cells: impl Iterator<Item = String>) -> String {
    format!("| {} |", cells.collect::<Vec<String>>().join(" | "))
}

/// `value` padded with spaces to `width` characters.
fn pad(value: &str, width: usize) -> String {
    let mut padded = value.to_string();
    for _ in display_width(value)..width {
        padded.push(' ');
    }
    padded
}

/// Characters, not bytes: a cell is padded to line up in the text, and a CJK
/// glyph or an emoji is one column even though it is several bytes.
fn display_width(value: &str) -> usize {
    value.chars().count()
}

/// An array of objects, one value per line, in column order.
fn render_json(columns: &[String], types: &[String], rows: &[Vec<Cell>]) -> Rendered {
    let mut skipped = 0;
    if rows.is_empty() {
        return Rendered {
            text: "[]".to_string(),
            skipped,
        };
    }

    let mut text = String::from("[");
    for (position, row) in rows.iter().enumerate() {
        if position > 0 {
            text.push(',');
        }
        text.push_str("\n  {");
        for (index, name) in columns.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str("\n    ");
            text.push_str(&json_string(name));
            text.push_str(": ");

            let cell = row.get(index);
            if cell.is_some_and(query::is_placeholder) {
                skipped += 1;
                text.push_str("null");
                continue;
            }

            let type_name = types.get(index).map(String::as_str).unwrap_or_default();
            text.push_str(&json_value(cell.and_then(Option::as_deref), type_name));
        }
        text.push_str("\n  }");
    }
    text.push_str("\n]");

    Rendered { text, skipped }
}

/// One cell as JSON: `null` for `NULL`, a bare number or boolean where the
/// column's type says so, an embedded value for a JSON column, and a string
/// otherwise.
fn json_value(value: Option<&str>, type_name: &str) -> String {
    let Some(value) = value else {
        return "null".to_string();
    };

    let base = base_type(type_name);
    if is_boolean(&base) {
        let truthy = matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "t" | "true" | "y" | "yes"
        );
        return truthy.to_string();
    }
    if is_numeric(&base) && as_json_number(value).is_some() {
        // A JavaScript consumer reads a JSON number as a float and loses digits
        // past 2^53, so an integer that large goes in as a string instead.
        if beyond_safe_integer(value) {
            return json_string(value);
        }
        return value.to_string();
    }
    if matches!(base.as_str(), "JSON" | "JSONB")
        && let Ok(embedded) = serde_json::from_str::<serde_json::Value>(value)
    {
        return embedded.to_string();
    }
    json_string(value)
}

/// One value per line, for a single column: what a column copy puts on the
/// clipboard. `NULL` and placeholders are empty lines.
pub fn values(cells: &[Cell]) -> Rendered {
    let mut skipped = 0;
    let lines: Vec<String> = cells
        .iter()
        .map(|cell| {
            if query::is_placeholder(cell) {
                skipped += 1;
                return String::new();
            }
            cell.clone().unwrap_or_default()
        })
        .collect();

    Rendered {
        text: lines.join("\n"),
        skipped,
    }
}

/// One column as a SQL `IN` list: `('a', 'b', 3)`.
///
/// A numeric or boolean column goes in bare and everything else is quoted for
/// `NULL` — and a placeholder, which is `NULL` in every format — is
/// left out, but the order and the duplicates are kept: this is meant to be
/// pasted into a `WHERE` beside the values the user picked. If nothing is left,
/// a single `NULL` keeps the list valid rather than emitting `()`.
pub fn in_list(engine: Engine, type_name: &str, cells: &[Cell]) -> Rendered {
    let mut skipped = 0;
    let mut values = Vec::new();

    for cell in cells {
        if query::is_placeholder(cell) {
            skipped += 1;
            continue;
        }
        let Some(value) = cell.as_deref() else {
            continue;
        };

        let base = base_type(type_name);
        if is_boolean(&base) {
            let truthy = matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "t" | "true" | "y" | "yes"
            );
            values.push(truthy.to_string());
        } else if is_number(type_name, value) {
            values.push(value.to_string());
        } else {
            values.push(quote_literal_for(engine, value));
        }
    }

    // An `IN` list cannot be empty — `in ()` is a syntax error — so a column
    // whose values were all NULL (or all stand-ins) keeps one: comparing
    // against NULL is unknown, which is what the dropped values meant anyway.
    let text = if values.is_empty() {
        "(NULL)".to_string()
    } else {
        format!("({})", values.join(", "))
    };

    Rendered { text, skipped }
}

/// One row per `INSERT`, with numbers bare and everything else quoted.
fn render_sql(
    engine: Engine,
    table: &str,
    columns: &[String],
    types: &[String],
    rows: &[Vec<Cell>],
) -> Rendered {
    let mut skipped = 0;
    if columns.is_empty() {
        return Rendered {
            text: String::new(),
            skipped,
        };
    }

    let target = quote_identifier(table, engine);
    let names = columns
        .iter()
        .map(|name| quote_identifier(name, engine))
        .collect::<Vec<String>>()
        .join(", ");

    let mut text = String::new();
    for row in rows {
        let values: Vec<String> = (0..columns.len())
            .map(|index| {
                let cell = row.get(index);
                if cell.is_some_and(query::is_placeholder) {
                    skipped += 1;
                    return "NULL".to_string();
                }
                let type_name = types.get(index).map(String::as_str).unwrap_or_default();
                match cell.and_then(Option::as_deref) {
                    None => "NULL".to_string(),
                    Some(value) if is_number(type_name, value) => value.to_string(),
                    Some(value) => quote_literal_for(engine, value),
                }
            })
            .collect();

        text.push_str(&format!(
            "insert into {target} ({names}) values ({});\n",
            values.join(", ")
        ));
    }

    Rendered { text, skipped }
}

/// Whether a `type_name` value can go into the statement bare.
///
/// A text column holding `42` has to stay quoted: Postgres will not cast an
/// integer literal to text on the way into an `INSERT`.
fn is_number(type_name: &str, value: &str) -> bool {
    let base = base_type(type_name);
    is_numeric(&base) && as_json_number(value).is_some()
}

/// A driver type name without its array suffix or its width: `INT4[]` becomes
/// `INT4`, `numeric(10,2)` becomes `NUMERIC`.
fn base_type(type_name: &str) -> String {
    let name = type_name.trim_matches('"').trim().to_ascii_uppercase();
    let name = name.strip_suffix("[]").unwrap_or(&name);
    name.split(['(', ' ']).next().unwrap_or(name).to_string()
}

/// Column types whose values go into JSON as numbers.
fn is_numeric(name: &str) -> bool {
    matches!(
        name,
        "INT"
            | "INTEGER"
            | "INT2"
            | "INT4"
            | "INT8"
            | "TINYINT"
            | "SMALLINT"
            | "MEDIUMINT"
            | "BIGINT"
            | "FLOAT"
            | "FLOAT4"
            | "FLOAT8"
            | "REAL"
            | "DOUBLE"
            | "MONEY"
            | "NUMERIC"
            | "DECIMAL"
            | "DEC"
            | "OID"
            | "YEAR"
            | "SERIAL"
            | "SMALLSERIAL"
            | "BIGSERIAL"
    )
}

/// Column types whose values go into JSON as `true`/`false`.
fn is_boolean(name: &str) -> bool {
    matches!(name, "BOOL" | "BOOLEAN")
}

/// `value` when JSON can spell it as a number, so a numeric column's `007` or
/// `NaN` — which JSON has no way to write — stays a string instead.
fn as_json_number(value: &str) -> Option<&str> {
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(serde_json::Value::Number(_)) => Some(value),
        _ => None,
    }
}

/// The largest integer a JavaScript `Number` holds exactly.
const JSON_SAFE_INTEGER: i128 = 1 << 53;

/// Whether a numeric string is an integer a JavaScript consumer would read
/// imprecisely. Only an integer spelling is at risk: a float has already lost
/// the digits it was going to lose, and `1e20` is written the way JSON spells
/// it.
fn beyond_safe_integer(value: &str) -> bool {
    if value.contains(['.', 'e', 'E']) {
        return false;
    }
    value
        .parse::<i128>()
        .is_ok_and(|number| !(-JSON_SAFE_INTEGER..=JSON_SAFE_INTEGER).contains(&number))
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec!["id".to_string(), "name".to_string()]
    }

    #[test]
    fn csv_quotes_only_the_fields_that_need_it() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let rows = vec![
            vec![Some("plain".to_string()), Some("a,b".to_string())],
            vec![Some("\"q\"".to_string()), Some("line\nbreak".to_string())],
        ];

        let rendered = render(Format::Csv, Engine::Sqlite, "t", &columns, &[], &rows);
        assert_eq!(
            rendered.text,
            "a,b\nplain,\"a,b\"\n\"\"\"q\"\"\",\"line\nbreak\"\n"
        );
        assert_eq!(rendered.skipped, 0);
    }

    #[test]
    fn csv_writes_null_and_placeholders_empty() {
        let rows = vec![
            vec![Some("1".to_string()), None],
            vec![Some("2".to_string()), query::blob(b"abc")],
        ];

        let rendered = render(Format::Csv, Engine::Sqlite, "t", &names(), &[], &rows);
        assert_eq!(rendered.text, "id,name\n1,\n2,\n");
        assert_eq!(rendered.skipped, 1, "the blob stand-in is not real data");
    }

    #[test]
    fn json_takes_its_types_from_the_column() {
        let columns = vec![
            "n".to_string(),
            "t".to_string(),
            "b".to_string(),
            "j".to_string(),
        ];
        let types = vec![
            "INT4".to_string(),
            "TEXT".to_string(),
            "BOOL".to_string(),
            "JSONB".to_string(),
        ];
        let rows = vec![vec![
            Some("42".to_string()),
            Some("42".to_string()),
            Some("1".to_string()),
            Some("{\"a\":1}".to_string()),
        ]];

        let rendered = render(Format::Json, Engine::Postgres, "t", &columns, &types, &rows);
        assert_eq!(
            rendered.text,
            "[\n  {\n    \"n\": 42,\n    \"t\": \"42\",\n    \"b\": true,\n    \"j\": {\"a\":1}\n  }\n]"
        );
        assert_eq!(rendered.skipped, 0);
    }

    #[test]
    fn json_writes_placeholders_as_null() {
        let rows = vec![vec![Some("1".to_string()), query::blob(b"abc")]];
        let rendered = render(
            Format::Json,
            Engine::Sqlite,
            "t",
            &names(),
            &["INTEGER".to_string(), "BLOB".to_string()],
            &rows,
        );
        assert!(rendered.text.ends_with("\"name\": null\n  }\n]"));
        assert_eq!(rendered.skipped, 1);
    }

    #[test]
    fn sql_quotes_text_and_mysql_backslashes() {
        let columns = vec!["name".to_string()];
        let types = vec!["TEXT".to_string()];
        let rows = vec![vec![Some("O'Brien".to_string())]];

        let rendered = render(Format::Sql, Engine::Sqlite, "t", &columns, &types, &rows);
        assert_eq!(rendered.text, "insert into t (name) values ('O''Brien');\n");

        let rows = vec![vec![Some("C:\\tmp".to_string())]];
        let rendered = render(Format::Sql, Engine::MySql, "t", &columns, &types, &rows);
        assert_eq!(
            rendered.text,
            "insert into t (name) values ('C:\\\\tmp');\n"
        );
    }

    #[test]
    fn sql_leaves_numbers_bare_and_writes_placeholders_null() {
        let columns = vec!["id".to_string(), "payload".to_string()];
        let types = vec!["INT4".to_string(), "BYTEA".to_string()];
        let rows = vec![vec![Some("7".to_string()), query::blob(b"abc")]];

        let rendered = render(Format::Sql, Engine::Postgres, "t", &columns, &types, &rows);
        assert_eq!(
            rendered.text,
            "insert into t (id, payload) values (7, NULL);\n"
        );
        assert_eq!(rendered.skipped, 1);
    }

    #[test]
    fn an_empty_result_is_still_a_valid_document() {
        let empty = render(Format::Json, Engine::Sqlite, "t", &names(), &[], &[]);
        assert_eq!(empty.text, "[]");
        let csv = render(Format::Csv, Engine::Sqlite, "t", &names(), &[], &[]);
        assert_eq!(csv.text, "id,name\n");
    }

    #[test]
    fn tsv_flattens_tabs_and_line_breaks() {
        let rows = vec![
            vec![Some("a\tb".to_string()), Some("line\nbreak".to_string())],
            vec![Some("1".to_string()), None],
        ];

        let rendered = render(Format::Tsv, Engine::Sqlite, "t", &names(), &[], &rows);
        assert_eq!(rendered.text, "id\tname\na b\tline break\n1\t\n");
        assert_eq!(rendered.skipped, 0);
    }

    #[test]
    fn tsv_writes_placeholders_empty() {
        let rows = vec![vec![Some("1".to_string()), query::blob(b"abc")]];
        let rendered = render(Format::Tsv, Engine::Sqlite, "t", &names(), &[], &rows);
        assert_eq!(rendered.text, "id\tname\n1\t\n");
        assert_eq!(rendered.skipped, 1);
    }

    #[test]
    fn markdown_pads_every_column_to_one_width() {
        let columns = vec!["a".to_string(), "b".to_string()];
        let rows = vec![
            vec![Some("1".to_string()), Some("x|y".to_string())],
            vec![Some("long".to_string()), Some("b".to_string())],
        ];

        let rendered = render(Format::Markdown, Engine::Sqlite, "t", &columns, &[], &rows);
        assert_eq!(
            rendered.text,
            concat!(
                "| a    | b    |\n",
                "| ---- | ---- |\n",
                "| 1    | x\\|y |\n",
                "| long | b    |\n",
            )
        );
        assert_eq!(rendered.skipped, 0);
    }

    #[test]
    fn markdown_escapes_line_breaks_and_backslashes() {
        let columns = vec!["v".to_string()];
        let rows = vec![
            vec![Some("line\nbreak".to_string())],
            vec![Some("back\\slash".to_string())],
        ];

        let rendered = render(Format::Markdown, Engine::Sqlite, "t", &columns, &[], &rows);
        assert!(rendered.text.contains("line<br>break"), "{}", rendered.text);
        assert!(rendered.text.contains("back\\\\slash"), "{}", rendered.text);
    }

    #[test]
    fn markdown_writes_placeholders_empty() {
        let rows = vec![vec![Some("1".to_string()), query::blob(b"abc")]];
        let rendered = render(Format::Markdown, Engine::Sqlite, "t", &names(), &[], &rows);
        assert_eq!(
            rendered.text,
            concat!("| id  | name |\n", "| --- | ---- |\n", "| 1   |      |\n",)
        );
        assert_eq!(rendered.skipped, 1);
    }

    #[test]
    fn json_quotes_integers_a_javascript_number_cannot_hold() {
        let columns = vec!["big".to_string(), "small".to_string(), "float".to_string()];
        let types = vec!["INT8".to_string(), "INT8".to_string(), "FLOAT8".to_string()];
        let rows = vec![vec![
            Some("9007199254740993".to_string()),
            Some("42".to_string()),
            Some("1e20".to_string()),
        ]];

        let rendered = render(Format::Json, Engine::Postgres, "t", &columns, &types, &rows);
        assert!(rendered.text.contains("\"big\": \"9007199254740993\""));
        assert!(rendered.text.contains("\"small\": 42"));
        assert!(rendered.text.contains("\"float\": 1e20"));
    }

    #[test]
    fn column_values_are_one_per_line() {
        let cells = vec![Some("a".to_string()), None, query::blob(b"abc")];
        let rendered = values(&cells);
        assert_eq!(rendered.text, "a\n\n");
        assert_eq!(rendered.skipped, 1);
    }

    #[test]
    fn an_in_list_is_typed_and_drops_nulls() {
        let cells = vec![Some("a".to_string()), Some("b".to_string())];
        let rendered = in_list(Engine::Sqlite, "TEXT", &cells);
        assert_eq!(rendered.text, "('a', 'b')");
        assert_eq!(rendered.skipped, 0);

        let cells = vec![Some("3".to_string()), None, Some("5".to_string())];
        let rendered = in_list(Engine::Postgres, "INT4", &cells);
        assert_eq!(rendered.text, "(3, 5)", "a NULL is left out");

        let cells = vec![Some("1".to_string()), Some("0".to_string())];
        let rendered = in_list(Engine::Postgres, "BOOL", &cells);
        assert_eq!(rendered.text, "(true, false)");
    }

    #[test]
    fn an_in_list_escapes_for_the_engine_and_counts_placeholders() {
        let cells = vec![Some("O'Brien".to_string()), query::blob(b"abc")];
        let rendered = in_list(Engine::Sqlite, "TEXT", &cells);
        assert_eq!(rendered.text, "('O''Brien')");
        assert_eq!(rendered.skipped, 1, "the blob stand-in is not a value");

        let cells = vec![Some("C:\\tmp".to_string())];
        let rendered = in_list(Engine::MySql, "TEXT", &cells);
        assert_eq!(rendered.text, "('C:\\\\tmp')");

        let empty = in_list(Engine::Sqlite, "TEXT", &[]);
        assert_eq!(empty.text, "(NULL)", "an empty list is still valid SQL");

        // A column of only NULLs keeps a NULL rather than emitting `()`, which
        // no server parses.
        let all_null = in_list(Engine::Sqlite, "TEXT", &[None, None]);
        assert_eq!(all_null.text, "(NULL)");
    }
}
