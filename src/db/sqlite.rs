//! SQLite driver (local file databases).

use anyhow::{Context, Result};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, TypeInfo, ValueRef};

use super::query::{self, Cell};
use super::{ConnectionConfig, POOL_SIZE, decode};

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
        .create_if_missing(false);

    let pool = SqlitePoolOptions::new()
        .max_connections(POOL_SIZE)
        .connect_with(options)
        .await
        .with_context(|| format!("could not open {}", config.database))?;
    Ok(pool)
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
        "BOOLEAN" => decode!(row, index, bool),
        "BLOB" => row
            .try_get::<Vec<u8>, _>(index)
            .ok()
            .and_then(|bytes| query::blob(&bytes)),
        _ => decode!(row, index, String),
    };

    value.or_else(|| query::unsupported(&type_name))
}
