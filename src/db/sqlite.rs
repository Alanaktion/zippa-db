//! SQLite driver (local file databases).

use anyhow::{Context, Result};
use sqlx::sqlite::{
    SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteQueryResult, SqliteRow,
};
use sqlx::{AssertSqlSafe, Row, TypeInfo, ValueRef};

use super::query::{self, Cell};
use super::{ConnectionConfig, POOL_SIZE, decode, quote_literal};

/// A SQLite connection has one main database plus any attached ones.
pub(crate) const DATABASES_SQL: &str = "SELECT name FROM pragma_database_list ORDER BY seq";

pub(crate) const OBJECTS_SQL: &str = "SELECT NULL AS table_schema, name, \
     CASE type WHEN 'view' THEN 'VIEW' ELSE 'BASE TABLE' END AS table_type \
     FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' \
     ORDER BY name";

/// Every column in every user table and view, for the catalog search: owning
/// table, whether it is a view, column name, and declared type.
///
/// `pragma_table_info` is a table-valued function, and SQLite lets its
/// argument be a column from the table to its left, so one row per column of
/// every table comes back in a single statement. There are no schemas, so the
/// first field is a stand-in `NULL`.
pub(crate) const CATALOG_COLUMNS_SQL: &str = "SELECT NULL, m.name, m.type, ti.name, ti.type \
     FROM sqlite_master m JOIN pragma_table_info(m.name) ti \
     WHERE m.type IN ('table', 'view') AND m.name NOT LIKE 'sqlite_%' \
     ORDER BY m.name, ti.cid";

/// Every explicitly created index: owning table, index name, and its columns
/// in index order. `origin = 'c'` skips the auto-indexes behind a `UNIQUE` or
/// `PRIMARY KEY`, whose generated names are noise in a search.
pub(crate) const CATALOG_INDEXES_SQL: &str = "SELECT NULL, m.name, m.type, il.name, \
     (SELECT group_concat(ii.name, ', ') FROM pragma_index_info(il.name) ii) \
     FROM sqlite_master m JOIN pragma_index_list(m.name) il \
     WHERE m.type IN ('table', 'view') AND m.name NOT LIKE 'sqlite_%' \
       AND il.origin = 'c' \
     ORDER BY m.name, il.name";

/// Every trigger: the table or view it is on, its name, and that object's
/// kind. SQLite keeps a trigger's timing and event only inside its `CREATE
/// TRIGGER` text, so the detail line stays empty rather than being guessed at.
pub(crate) const CATALOG_TRIGGERS_SQL: &str = "SELECT NULL, tr.tbl_name, COALESCE(o.type, 'table'), tr.name, '' \
     FROM sqlite_master tr \
     LEFT JOIN sqlite_master o ON o.name = tr.tbl_name AND o.type IN ('table', 'view') \
     WHERE tr.type = 'trigger' \
     ORDER BY tr.tbl_name, tr.name";

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

/// The stored SQL a table rebuild reads before it starts: the table's own
/// definition, its explicit indexes and triggers, and every view (the caller
/// keeps the ones that mention the table). Auto-indexes behind a `UNIQUE` or
/// `PRIMARY KEY` constraint have no SQL of their own, so they are left out and
/// rebuilt from the table.
pub(crate) fn master_sql(table: &str) -> String {
    format!(
        "SELECT type, name, sql FROM sqlite_master \
         WHERE sql IS NOT NULL AND (tbl_name = {} OR type = 'view') \
         ORDER BY type, name",
        quote_literal(table)
    )
}

/// Run the body of a table rebuild as one atomic change.
///
/// The procedure cannot be a plain list of statements: foreign keys have to be
/// off before the transaction starts — SQLite ignores the pragma inside one —
/// the whole rebuild has to run on a single connection, and the foreign keys
/// are re-checked before it commits, so a rebuild that breaks a relationship
/// rolls itself back rather than leaving a half-moved table.
///
/// The connection is closed rather than returned to the pool, so a rebuild
/// cancelled halfway cannot hand back a connection with a transaction still
/// open or foreign keys still off.
pub(crate) async fn rebuild(pool: &SqlitePool, statements: &[String]) -> Result<()> {
    let mut connection = pool
        .acquire()
        .await
        .context("could not take a connection for the table rebuild")?;
    connection.close_on_drop();

    // `PRAGMA foreign_keys` is a no-op inside a transaction, so it is set
    // before one is opened and read first so a connection that never asked for
    // the enforcement does not suddenly turn it on.
    let foreign_keys: bool = sqlx::query_scalar::<_, i64>("PRAGMA foreign_keys")
        .fetch_one(&mut *connection)
        .await
        .map(|value| value != 0)
        .unwrap_or(false);
    if foreign_keys {
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await
            .context("could not turn foreign keys off for the table rebuild")?;
    }

    // Modern SQLite rewrites every reference to the renamed table, and refuses
    // to when a view that reads the old table has been left dangling by the
    // `DROP TABLE` — which is exactly the state a rebuild passes through. This
    // connection is discarded afterwards, so the setting cannot leak, and the
    // procedure's own recreation of the indexes, triggers, and views is what
    // puts the references back.
    sqlx::query("PRAGMA legacy_alter_table = ON")
        .execute(&mut *connection)
        .await
        .context("could not prepare the table rebuild")?;

    let mut transaction = sqlx::Connection::begin(&mut *connection)
        .await
        .context("could not start the table rebuild's transaction")?;

    for (position, statement) in statements.iter().enumerate() {
        sqlx::query(AssertSqlSafe(statement.clone()))
            .execute(&mut *transaction)
            .await
            .with_context(|| format!("statement {} of {}", position + 1, statements.len()))?;
    }

    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&mut *transaction)
        .await
        .context("could not check foreign keys after the table rebuild")?;
    if !violations.is_empty() {
        transaction.rollback().await.ok();
        anyhow::bail!(
            "the rebuild would leave {} foreign key relationship{} broken, so it was rolled back",
            violations.len(),
            if violations.len() == 1 { "" } else { "s" }
        );
    }

    transaction
        .commit()
        .await
        .context("could not commit the table rebuild")?;

    if foreign_keys {
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await
            .ok();
    }
    Ok(())
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

/// The raw bytes of one column, for a binary value preview asking for the
/// bytes `cell` only ever summarizes.
pub(crate) fn raw_bytes(row: &SqliteRow, index: usize) -> Option<Vec<u8>> {
    row.try_get::<Vec<u8>, _>(index).ok()
}
