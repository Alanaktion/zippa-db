//! Database connectivity: connection configuration, pools, and query execution.
//!
//! Everything here is engine-agnostic; the per-engine modules own the sqlx
//! types and the mapping from SQL values to display strings.
//!
//! The split is by concern rather than by engine:
//!
//! * [`config`] — what the user saves about a connection, before it is opened.
//! * [`connection`] — the live pool, and the shared read and write paths.
//! * [`health`] — telling a dropped or unreachable server apart from a SQL
//!   error, and saying so in words.
//! * [`binary`] — sniffing and laying out the bytes behind a binary value.
//! * [`query_log`] — the bounded record of every statement a connection has
//!   sent, for the console pane.
//! * [`catalog`] — the whole schema once per session, and the search over it.
//! * [`completion`] — what the SQL editor offers for the word under the caret.
//! * [`schema`] — a table's own definition: columns, indexes, foreign keys.
//! * [`sql`] — quoting and placing bind parameters in generated statements.
//! * [`statement`] — splitting and classifying the user's own SQL.
//! * [`plan`] — reading an `EXPLAIN` into a tree.
//! * [`query`] — what a run comes back as.
//! * [`export`] — laying a result out as CSV, JSON, or SQL `INSERT`.
//! * [`import`] — reading a SQL dump back in as statements.
//! * [`script`] — running a whole query buffer, with or without a
//!   transaction, pausing on a failure for the user to answer.
//! * [`dedicated`] — one connection held for the length of an import or a
//!   script.
//! * [`runtime`] — the Tokio runtime every database call is submitted to.
//! * [`store`] — connection metadata on disk, passwords in the OS keychain.

pub mod binary;
pub mod catalog;
pub mod completion;
pub mod config;
pub mod connection;
pub(crate) mod dedicated;
pub mod export;
pub mod health;
pub mod import;
pub mod mysql;
pub mod plan;
pub mod postgres;
pub mod query;
pub mod query_log;
pub mod runtime;
pub mod schema;
pub mod script;
pub mod sql;
pub mod sqlite;
pub mod statement;
pub mod store;
pub(crate) mod tunnel;

#[cfg(test)]
pub(crate) mod tests;

pub use catalog::{Catalog, CatalogEntry, CatalogKind, Query};
pub(crate) use config::file_name;
pub use config::{
    ConnectionConfig, Engine, SafetyMode, SshAuth, SshConfig, SslConfig, SslMode, TagColor,
    is_risky_auto_apply,
};
pub(crate) use connection::pool_options;
pub use connection::{
    Connection, Credentials, DatabaseObject, ObjectKind, QueryDigest, RowKey, StoredKind,
    StoredObject,
};
pub use import::{ImportProgress, ImportRequest, ImportSummary, OnError};
pub use plan::{Explained, Plan, PlanNode};
pub use query_log::{LoggedQuery, QueryOutcome, QuerySource};
pub use schema::{
    ColumnDef, ForeignKeyDef, IndexDef, RebuildSource, ReferentialAction, TableSchema,
};
pub use script::{
    Blocker, Decision, OnFailure, ScriptFailure, ScriptMode, ScriptOutcome, ScriptRun, Step,
    transaction_blocker,
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
