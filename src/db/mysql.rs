//! MySQL / MariaDB driver.

use anyhow::Result;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlQueryResult, MySqlRow};
use sqlx::{Executor, Row, TypeInfo, ValueRef};

use super::query::{self, Cell};
use super::{ConnectionConfig, POOL_SIZE, decode, quote_literal};

/// Schemas double as databases in MySQL; the server's own are hidden.
pub(crate) const DATABASES_SQL: &str = "SELECT schema_name FROM information_schema.schemata \
     WHERE schema_name NOT IN ('information_schema', 'performance_schema', 'mysql', 'sys') \
     ORDER BY schema_name";

pub(crate) const OBJECTS_SQL: &str = "SELECT table_schema, table_name, table_type \
     FROM information_schema.tables WHERE table_schema = DATABASE() ORDER BY table_name";

pub(crate) async fn connect(
    config: &ConnectionConfig,
    password: Option<&str>,
) -> Result<MySqlPool> {
    let mut options = MySqlConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .username(&config.username);

    if !config.database.is_empty() {
        options = options.database(&config.database);
    }
    if let Some(password) = password {
        options = options.password(password);
    }

    let mut pool_options = MySqlPoolOptions::new().max_connections(POOL_SIZE);

    // A read-only connection is read-only at the server too: the client-side
    // check in `Connection::refuse_write` only speaks for statements it can
    // recognise. MySQL has no connect option for it, so every connection the
    // pool opens is told as it comes up.
    if config.safety.is_read_only() {
        pool_options = pool_options.after_connect(|connection, _| {
            Box::pin(async move {
                connection
                    .execute("SET SESSION TRANSACTION READ ONLY")
                    .await?;
                Ok(())
            })
        });
    }

    let pool = pool_options.connect_with(options).await?;
    Ok(pool)
}

/// Primary key columns of a table in the current database, in key order.
pub(crate) fn primary_key_sql(table: &str) -> String {
    format!(
        "SELECT kcu.column_name FROM information_schema.table_constraints tc \
         JOIN information_schema.key_column_usage kcu \
         ON kcu.constraint_name = tc.constraint_name \
         AND kcu.table_schema = tc.table_schema \
         AND kcu.table_name = tc.table_name \
         WHERE tc.constraint_type = 'PRIMARY KEY' \
         AND tc.table_schema = DATABASE() AND tc.table_name = {} \
         ORDER BY kcu.ordinal_position",
        quote_literal(table)
    )
}

pub(crate) fn rows_affected(result: &MySqlQueryResult) -> u64 {
    result.rows_affected()
}

pub(crate) fn cell(row: &MySqlRow, index: usize) -> Cell {
    let Ok(raw) = row.try_get_raw(index) else {
        return None;
    };
    if raw.is_null() {
        return None;
    }
    let type_name = raw.type_info().name().to_string();

    let value = match type_name.as_str() {
        "BOOLEAN" | "TINYINT" => decode!(row, index, i8),
        "TINYINT UNSIGNED" => decode!(row, index, u8),
        "SMALLINT" => decode!(row, index, i16),
        "SMALLINT UNSIGNED" => decode!(row, index, u16),
        "INT" | "MEDIUMINT" => decode!(row, index, i32),
        "INT UNSIGNED" | "MEDIUMINT UNSIGNED" => decode!(row, index, u32),
        "BIGINT" => decode!(row, index, i64),
        "BIGINT UNSIGNED" => decode!(row, index, u64),
        "FLOAT" => decode!(row, index, f32),
        "DOUBLE" => decode!(row, index, f64),
        "DECIMAL" => decode!(row, index, sqlx::types::BigDecimal),
        "JSON" => decode!(row, index, sqlx::types::JsonValue),
        "DATE" => decode!(row, index, chrono::NaiveDate),
        "DATETIME" | "TIMESTAMP" => decode!(row, index, chrono::NaiveDateTime),
        // MySQL sets the wire protocol's BINARY column flag — which this
        // display name is derived from — for any `_bin` collation, not just
        // the true binary charset. So text columns using a `_bin` collation
        // (MySQL 8's information_schema identifier columns, for one) land
        // here too. sqlx's own `Type<MySql>::compatible` for `String`
        // already checks the real collation rather than this flag, so defer
        // to that: try a String decode first and only fall back to a
        // byte-count summary when it's genuinely not text.
        // https://github.com/launchbadge/sqlx/issues/3387
        "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" => {
            decode!(row, index, String).or_else(|| {
                row.try_get::<Vec<u8>, _>(index)
                    .ok()
                    .and_then(|bytes| query::blob(&bytes))
            })
        }
        _ => decode!(row, index, String),
    };

    value.or_else(|| query::unsupported(&type_name))
}
