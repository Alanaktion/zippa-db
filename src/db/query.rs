//! Result sets returned by a query.

use std::time::Duration;

/// A single cell. `None` is SQL `NULL`, which the grid renders differently from
/// an empty string.
pub type Cell = Option<String>;

/// One notice, warning, or note the server reported for a statement — the
/// things that are neither rows nor an error, so a run used to drop them.
/// `level` is the server's own word (`Warning`, `Note`, …) and `code` its
/// number, when it gives one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryMessage {
    pub level: String,
    pub code: Option<String>,
    pub text: String,
}

impl QueryMessage {
    /// `"Warning 1265: Data truncated for column 'x'"`, or `"Note: …"`
    /// when the server gave no number.
    pub fn display(&self) -> String {
        match &self.code {
            Some(code) => format!("{} {code}: {}", self.level, self.text),
            None => format!("{}: {}", self.level, self.text),
        }
    }
}

/// The outcome of running one statement.
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    /// Driver type name per column, e.g. `INT4`, `TEXT`, `BLOB`. Kept so
    /// generated writes can cast their parameters, and so the grid knows which
    /// cells hold values it could not read back.
    pub column_types: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    pub elapsed: Duration,
    /// Rows the statement changed, when the server said. A `select` reports
    /// nothing here; an `insert` or an `update` reports what it wrote.
    pub affected: Option<u64>,
    /// Notices and warnings the statement produced, in the order the server
    /// reported them. Empty unless the engine reports them on the
    /// connection the statement ran on: MySQL, read back with
    /// `SHOW WARNINGS` on the query tab's pinned connection. A pool read
    /// cannot carry them — the next statement on another connection would
    /// answer for a different session — and Postgres cannot carry them at
    /// all with this stack: sqlx 0.9 decodes a `NoticeResponse` only to
    /// log it (`sqlx-postgres`'s stream handler) and drops it, with no
    /// API to capture one per query, so a Postgres run's messages stay
    /// empty rather than being guessed at.
    pub messages: Vec<QueryMessage>,
}

impl QueryResult {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// `"12 rows in 4 ms"`, for the status bar.
    pub fn summary(&self) -> String {
        let milliseconds = self.elapsed.as_millis();

        // A statement that returned rows is described by them; one that only
        // changed rows is described by how many.
        if self.rows.is_empty()
            && let Some(affected) = self.affected
        {
            let unit = if affected == 1 { "row" } else { "rows" };
            return format!("{affected} {unit} affected in {milliseconds} ms");
        }

        let rows = self.row_count();
        let unit = if rows == 1 { "row" } else { "rows" };
        format!("{rows} {unit} in {milliseconds} ms")
    }
}

/// Placeholder shown for values no driver mapping covers yet.
pub fn unsupported(type_name: &str) -> Cell {
    Some(format!("<{type_name}>"))
}

/// `1` / `0`, which every engine reads back as a boolean and which matches how
/// MySQL and SQLite store one in the first place.
pub fn boolean(value: bool) -> String {
    if value { "1" } else { "0" }.to_string()
}

/// Binary columns are summarized rather than dumped into the grid.
pub fn blob(bytes: &[u8]) -> Cell {
    Some(format!("<{} bytes>", bytes.len()))
}

/// True for the `<3 bytes>` and `<TYPE>` stand-ins above.
///
/// The grid shows them, but they are a description of the value rather than
/// the value itself, so writing one back would corrupt the row.
///
/// Angle brackets alone are not enough to go on: a real value can be wrapped
/// in them too, `<a>hi</a>` from an `xml` column being the everyday case. A
/// stand-in holds one type name, or a byte count, and so never brackets
/// anything of its own — nor holds a parenthesis or a comma, which is what
/// tells a Postgres `circle`, `<(1,2),3>`, apart from one.
pub fn is_placeholder(cell: &Cell) -> bool {
    let Some(value) = cell else {
        return false;
    };
    let Some(inner) = value.strip_prefix('<').and_then(|v| v.strip_suffix('>')) else {
        return false;
    };
    if inner.contains(['<', '>', '(', ')', ',']) {
        return false;
    }
    match inner.split_once(' ') {
        Some((count, "bytes")) => count.chars().all(|c| c.is_ascii_digit()),
        // A type name is one word; `INT4[]` and `double precision` included.
        Some(_) | None => !inner.is_empty(),
    }
}

/// Column types whose values never survive a round trip through text.
pub fn is_binary_type(type_name: &str) -> bool {
    let name = type_name.trim_matches('"').to_ascii_uppercase();
    // An array of a binary type is no more readable than one value of it.
    let name = name.strip_suffix("[]").unwrap_or(&name);
    name == "BYTEA"
        || name.ends_with("BLOB")
        || name == "BINARY"
        || name == "VARBINARY"
        || name == "GEOMETRY"
}

/// Column types that hold raw bytes a file can replace, as a staged hex
/// string decoded on the way back in (see `sql::binary_placeholder`).
/// Narrower than [`is_binary_type`]: a `GEOMETRY` value is only meaningful
/// as WKB, and an array of a binary type is not one value, so neither is
/// offered a file to load — their dialogs stay read-only.
pub fn is_blob_type(type_name: &str) -> bool {
    let name = type_name.trim_matches('"').to_ascii_uppercase();
    name == "BYTEA" || name.ends_with("BLOB") || name == "BINARY" || name == "VARBINARY"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stand_ins_are_recognized() {
        assert!(is_placeholder(&blob(b"abc")));
        assert!(is_placeholder(&unsupported("XML")));
        assert!(is_placeholder(&unsupported("INT4[]")));
        assert!(!is_placeholder(&Some("plain".into())));
        assert!(!is_placeholder(&None));
        // An `xml` document is a value, not a description of one.
        assert!(!is_placeholder(&Some("<a>hi</a>".into())));
        assert!(!is_placeholder(&Some("<>".into())));
        // Nor is a Postgres `circle`.
        assert!(!is_placeholder(&Some("<(1,2),3>".into())));
    }

    #[test]
    fn blob_types_are_the_ones_a_file_can_replace() {
        assert!(is_blob_type("BYTEA"));
        assert!(is_blob_type("BLOB"));
        assert!(is_blob_type("LONGBLOB"));
        assert!(is_blob_type("BINARY"));
        assert!(is_blob_type("VARBINARY"));
        assert!(
            !is_blob_type("GEOMETRY"),
            "geometry bytes are WKB, not a file"
        );
        assert!(!is_blob_type("BYTEA[]"), "an array is not one value");
        assert!(!is_blob_type("TEXT"));
    }

    #[test]
    fn messages_say_their_level_and_code() {
        let warning = QueryMessage {
            level: "Warning".into(),
            code: Some("1265".into()),
            text: "Data truncated for column 'v'".into(),
        };
        assert_eq!(
            warning.display(),
            "Warning 1265: Data truncated for column 'v'"
        );
        let note = QueryMessage {
            level: "Note".into(),
            code: None,
            text: "Table does not exist".into(),
        };
        assert_eq!(note.display(), "Note: Table does not exist");
    }

    #[test]
    fn binary_types_are_recognized() {
        for name in ["BLOB", "bytea", "LONGBLOB", "VarBinary", "BYTEA[]"] {
            assert!(is_binary_type(name), "{name} should be binary");
        }
        for name in ["TEXT", "INT4", "VARCHAR"] {
            assert!(!is_binary_type(name), "{name} should not be binary");
        }
    }
}
