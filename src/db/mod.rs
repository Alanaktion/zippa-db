//! Database connectivity: connection configuration, pools, and query execution.
//!
//! Everything here is engine-agnostic; the per-engine modules own the sqlx
//! types and the mapping from SQL values to display strings.
//!
//! The split is by concern rather than by engine:
//!
//! * [`config`] — what the user saves about a connection, before it is opened.
//! * [`connection`] — the live pool, and the shared read and write paths.
//! * [`binary`] — sniffing and laying out the bytes behind a binary value.
//! * [`query_log`] — the bounded record of every statement a connection has
//!   sent, for the console pane.
//! * [`catalog`] — the whole schema once per session, and the search over it.
//! * [`schema`] — a table's own definition: columns, indexes, foreign keys.
//! * [`sql`] — quoting and placing bind parameters in generated statements.
//! * [`statement`] — splitting and classifying the user's own SQL.
//! * [`plan`] — reading an `EXPLAIN` into a tree.
//! * [`query`] — what a run comes back as.
//! * [`export`] — laying a result out as CSV, JSON, or SQL `INSERT`.
//! * [`import`] — reading a SQL dump back in as statements.
//! * [`runtime`] — the Tokio runtime every database call is submitted to.
//! * [`store`] — connection metadata on disk, passwords in the OS keychain.

pub mod binary;
pub mod catalog;
pub mod config;
pub mod connection;
pub mod export;
pub mod import;
pub mod mysql;
pub mod plan;
pub mod postgres;
pub mod query;
pub mod query_log;
pub mod runtime;
pub mod schema;
pub mod sql;
pub mod sqlite;
pub mod statement;
pub mod store;

#[cfg(test)]
pub(crate) mod tests;

pub use catalog::{Catalog, CatalogEntry, CatalogKind, Query};
pub(crate) use config::file_name;
pub use config::{ConnectionConfig, Engine, SafetyMode, TagColor, is_risky_auto_apply};
pub(crate) use connection::POOL_SIZE;
pub use connection::{
    Connection, DatabaseObject, ObjectKind, QueryDigest, RowKey, StoredKind, StoredObject,
};
pub use import::{ImportProgress, ImportRequest, ImportSummary, OnError};
pub use plan::{Explained, Plan, PlanNode};
pub use query_log::{LoggedQuery, QueryOutcome, QuerySource};
pub use schema::{
    ColumnDef, ForeignKeyDef, IndexDef, RebuildSource, ReferentialAction, TableSchema,
};
pub use sql::quote_identifier;
pub(crate) use sql::{keyword_literal, placeholder, quote_literal, text_type, typed_placeholder};
pub use sqlite::Maintenance;

/// Decode a column into a display string, or `None` when the decode fails.
macro_rules! decode {
    ($row:expr, $index:expr, $ty:ty) => {
        $row.try_get::<$ty, _>($index)
            .ok()
            .map(|value| value.to_string())
    };
}

pub(crate) use decode;
