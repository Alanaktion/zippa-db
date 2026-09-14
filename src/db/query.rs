//! Result sets returned by a query.

use std::time::Duration;

/// A single cell. `None` is SQL `NULL`, which the grid renders differently from
/// an empty string.
pub type Cell = Option<String>;

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
/// anything of its own.
pub fn is_placeholder(cell: &Cell) -> bool {
    let Some(value) = cell else {
        return false;
    };
    let Some(inner) = value.strip_prefix('<').and_then(|v| v.strip_suffix('>')) else {
        return false;
    };
    if inner.contains(['<', '>']) {
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
