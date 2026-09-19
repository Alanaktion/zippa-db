//! Database connectivity: connection configuration, pools, and query execution.
//!
//! Everything here is engine-agnostic; the per-engine modules own the sqlx
//! types and the mapping from SQL values to display strings.
//!
//! The split is by concern rather than by engine:
//!
//! * [`config`] — what the user saves about a connection, before it is opened.
//! * [`connection`] — the live pool, and the shared read and write paths.
//! * [`schema`] — a table's own definition: columns, indexes, foreign keys.
//! * [`sql`] — quoting and placing bind parameters in generated statements.
//! * [`statement`] — splitting and classifying the user's own SQL.
//! * [`plan`] — reading an `EXPLAIN` into a tree.
//! * [`query`] — what a run comes back as.
//! * [`runtime`] — the Tokio runtime every database call is submitted to.
//! * [`store`] — connection metadata on disk, passwords in the OS keychain.

pub mod config;
pub mod connection;
pub mod mysql;
pub mod plan;
pub mod postgres;
pub mod query;
pub mod runtime;
pub mod schema;
pub mod sql;
pub mod sqlite;
pub mod statement;
pub mod store;

#[cfg(test)]
pub(crate) mod tests;

pub(crate) use config::file_name;
pub use config::{ConnectionConfig, Engine, SafetyMode, TagColor};
pub(crate) use connection::POOL_SIZE;
pub use connection::{Connection, DatabaseObject, ObjectKind, RowKey, StoredKind, StoredObject};
pub use plan::{Explained, Plan, PlanNode};
pub use schema::{ColumnDef, ForeignKeyDef, IndexDef, ReferentialAction, TableSchema};
pub use sql::quote_identifier;
pub(crate) use sql::{keyword_literal, placeholder, quote_literal, text_type, typed_placeholder};

/// Decode a column into a display string, or `None` when the decode fails.
macro_rules! decode {
    ($row:expr, $index:expr, $ty:ty) => {
        $row.try_get::<$ty, _>($index)
            .ok()
            .map(|value| value.to_string())
    };
}

pub(crate) use decode;
