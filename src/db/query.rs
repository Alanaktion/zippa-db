//! Result sets returned by a query.

use std::time::Duration;

/// A single cell. `None` is SQL `NULL`, which the grid renders differently from
/// an empty string.
pub type Cell = Option<String>;

/// The outcome of running one statement.
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    pub elapsed: Duration,
}

impl QueryResult {
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    /// `"12 rows in 4 ms"`, for the status bar.
    pub fn summary(&self) -> String {
        let rows = self.row_count();
        let unit = if rows == 1 { "row" } else { "rows" };
        format!("{rows} {unit} in {} ms", self.elapsed.as_millis())
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
