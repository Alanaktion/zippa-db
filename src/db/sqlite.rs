//! SQLite driver (local file databases).

use anyhow::{Context, Result};
use sqlx::sqlite::{
    SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteQueryResult, SqliteRow,
};
use sqlx::{Row, TypeInfo, ValueRef};

use super::query::{self, Cell};
use super::{ConnectionConfig, POOL_SIZE, decode, quote_literal};

/// A SQLite connection has one main database plus any attached ones.
pub(crate) const DATABASES_SQL: &str = "SELECT name FROM pragma_database_list ORDER BY seq";

pub(crate) const OBJECTS_SQL: &str = "SELECT NULL AS table_schema, name, \
     CASE type WHEN 'view' THEN 'VIEW' ELSE 'BASE TABLE' END AS table_type \
     FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' \
     ORDER BY name";

pub(crate) async fn connect(config: &ConnectionConfig) -> Result<SqlitePool> {
    if config.database.trim().is_empty() {
        anyhow::bail!("no database file selected");
    }

    // Opening a missing file would silently create an empty database.
    let options = SqliteConnectOptions::new()
        .filename(&config.database)
        .create_if_missing(false)
        // A read-only connection is read-only at the file too, so nothing
        // that slips past the client-side check can write.
        .read_only(config.safety.is_read_only());

    let pool = SqlitePoolOptions::new()
        .max_connections(POOL_SIZE)
        .connect_with(options)
        .await
        .with_context(|| format!("could not open {}", config.database))?;
    Ok(pool)
}

/// Primary key columns of a table, in key order.
///
/// A `WITHOUT ROWID` table always has a primary key, so this answering with
/// rows is what keeps the `rowid` fallback away from one.
pub(crate) fn primary_key_sql(table: &str) -> String {
    format!(
        "SELECT name FROM pragma_table_info({}) WHERE pk > 0 ORDER BY pk",
        quote_literal(table)
    )
}

/// Columns of a table: name, declared type, nullability, default.
pub(crate) fn columns_sql(table: &str) -> String {
    format!(
        "SELECT name, type, (\"notnull\" = 0), dflt_value \
         FROM pragma_table_info({}) ORDER BY cid",
        quote_literal(table)
    )
}

/// One row per index: name, its columns in index order, unique?, primary key?
///
/// `pragma_index_info` already returns a column's rows in index order, so
/// nothing here has to ask it to sort itself.
pub(crate) fn indexes_sql(table: &str) -> String {
    format!(
        "SELECT il.name, \
         (SELECT group_concat(ii.name, ',') FROM pragma_index_info(il.name) ii), \
         il.\"unique\", (il.origin = 'pk') \
         FROM pragma_index_list({}) il \
         ORDER BY il.seq",
        quote_literal(table)
    )
}

/// One row per foreign key: no name (SQLite does not have one, so the caller
/// synthesizes one), local columns, no schema, referenced table/columns
/// (matching order), `ON DELETE`/`ON UPDATE` — already spelled the way every
/// other engine reports them (`CASCADE`, `SET NULL`, ...).
pub(crate) fn foreign_keys_sql(table: &str) -> String {
    format!(
        "SELECT NULL, group_concat(\"from\", ','), NULL, \"table\", \
         group_concat(\"to\", ','), on_delete, on_update \
         FROM pragma_foreign_key_list({}) \
         GROUP BY id ORDER BY id",
        quote_literal(table)
    )
}

pub(crate) fn rows_affected(result: &SqliteQueryResult) -> u64 {
    result.rows_affected()
}

pub(crate) fn cell(row: &SqliteRow, index: usize) -> Cell {
    let Ok(raw) = row.try_get_raw(index) else {
        return None;
    };
    if raw.is_null() {
        return None;
    }
    let type_name = raw.type_info().name().to_string();

    let value = match type_name.as_str() {
        "INTEGER" => decode!(row, index, i64),
        "REAL" => decode!(row, index, f64),
        "BOOLEAN" => row.try_get::<bool, _>(index).ok().map(query::boolean),
        "BLOB" => row
            .try_get::<Vec<u8>, _>(index)
            .ok()
            .and_then(|bytes| query::blob(&bytes)),
        _ => decode!(row, index, String),
    };

    value.or_else(|| query::unsupported(&type_name))
}
