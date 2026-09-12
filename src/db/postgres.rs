//! PostgreSQL driver (also covers CockroachDB / Redshift).

use anyhow::Result;
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgQueryResult, PgRow};
use sqlx::{Row, TypeInfo, ValueRef};

use super::query::{self, Cell};
use super::{ConnectionConfig, POOL_SIZE, decode, quote_literal};

/// Databases on this server the user can connect to.
pub(crate) const DATABASES_SQL: &str = "SELECT datname FROM pg_database \
     WHERE datistemplate = false AND datallowconn ORDER BY datname";

/// Tables and views outside the system schemas.
pub(crate) const OBJECTS_SQL: &str = "SELECT table_schema, table_name, table_type \
     FROM information_schema.tables \
     WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
     ORDER BY table_schema, table_name";

pub(crate) async fn connect(config: &ConnectionConfig, password: Option<&str>) -> Result<PgPool> {
    let mut options = PgConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .username(&config.username);

    if !config.database.is_empty() {
        options = options.database(&config.database);
    }
    if let Some(password) = password {
        options = options.password(password);
    }

    let pool = PgPoolOptions::new()
        .max_connections(POOL_SIZE)
        .connect_with(options)
        .await?;
    Ok(pool)
}

/// Primary key columns of a table, in key order.
pub(crate) fn primary_key_sql(schema: &str, table: &str) -> String {
    format!(
        "SELECT kcu.column_name FROM information_schema.table_constraints tc \
         JOIN information_schema.key_column_usage kcu \
         ON kcu.constraint_name = tc.constraint_name \
         AND kcu.constraint_schema = tc.constraint_schema \
         AND kcu.table_name = tc.table_name \
         WHERE tc.constraint_type = 'PRIMARY KEY' \
         AND tc.table_schema = {} AND tc.table_name = {} \
         ORDER BY kcu.ordinal_position",
        quote_literal(schema),
        quote_literal(table)
    )
}

pub(crate) fn rows_affected(result: &PgQueryResult) -> u64 {
    result.rows_affected()
}

pub(crate) fn cell(row: &PgRow, index: usize) -> Cell {
    let Ok(raw) = row.try_get_raw(index) else {
        return None;
    };
    if raw.is_null() {
        return None;
    }
    let type_name = raw.type_info().name().to_string();

    let value = match type_name.as_str() {
        "BOOL" => decode!(row, index, bool),
        "INT2" => decode!(row, index, i16),
        "INT4" => decode!(row, index, i32),
        "INT8" => decode!(row, index, i64),
        "FLOAT4" => decode!(row, index, f32),
        "FLOAT8" => decode!(row, index, f64),
        "NUMERIC" => decode!(row, index, sqlx::types::BigDecimal),
        "UUID" => decode!(row, index, sqlx::types::Uuid),
        "JSON" | "JSONB" => decode!(row, index, sqlx::types::JsonValue),
        "DATE" => decode!(row, index, chrono::NaiveDate),
        "TIME" => decode!(row, index, chrono::NaiveTime),
        "TIMESTAMP" => decode!(row, index, chrono::NaiveDateTime),
        "TIMESTAMPTZ" => decode!(row, index, chrono::DateTime<chrono::Utc>),
        "BYTEA" => row
            .try_get::<Vec<u8>, _>(index)
            .ok()
            .and_then(|bytes| query::blob(&bytes)),
        _ => decode!(row, index, String),
    };

    value.or_else(|| query::unsupported(&type_name))
}
