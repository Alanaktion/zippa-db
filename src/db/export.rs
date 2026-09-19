//! Turning a result into a file: CSV, JSON, and SQL `INSERT`.
//!
//! Pure formatting — no UI and no database. The table view and the grid hand it
//! the columns, the driver's type names, and the rows, and it hands back the
//! text to write.
//!
//! A cell the driver could not read back (`query::is_placeholder`) is a
//! description of a value rather than the value itself, so every format writes
//! `NULL` there and [`Rendered::skipped`] says how many did so.

use super::config::Engine;
use super::query::{self, Cell};
use super::sql::{quote_identifier, quote_literal_for};

/// The file format an export is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Json,
    Sql,
}

impl Format {
    /// Every format, in the order a menu offers them.
    pub const ALL: [Format; 3] = [Format::Csv, Format::Json, Format::Sql];

    /// The file extension a saved export gets.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Json => "json",
            Format::Sql => "sql",
        }
    }

    /// The format's short name, for a menu.
    pub fn label(self) -> &'static str {
        match self {
            Format::Csv => "CSV",
            Format::Json => "JSON",
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
        Format::Csv => render_csv(columns, rows),
        Format::Json => render_json(columns, types, rows),
        Format::Sql => render_sql(engine, table, columns, types, rows),
    }
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
        return value.to_string();
    }
    if matches!(base.as_str(), "JSON" | "JSONB")
        && let Ok(embedded) = serde_json::from_str::<serde_json::Value>(value)
    {
        return embedded.to_string();
    }
    json_string(value)
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
}
