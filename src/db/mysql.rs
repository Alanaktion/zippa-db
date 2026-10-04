//! MySQL / MariaDB driver.

use anyhow::Result;
use sqlx::mysql::{MySqlConnectOptions, MySqlPool, MySqlQueryResult, MySqlRow};
use sqlx::{AssertSqlSafe, Executor, Row, TypeInfo, ValueRef};

use super::config::Engine;
use super::query::{self, Cell};
use super::sql::quote_literal_for;
use super::{ConnectionConfig, decode, pool_options};

/// Schemas double as databases in MySQL; the server's own are hidden.
pub(crate) const DATABASES_SQL: &str = "SELECT schema_name FROM information_schema.schemata \
     WHERE schema_name NOT IN ('information_schema', 'performance_schema', 'mysql', 'sys') \
     ORDER BY schema_name";

pub(crate) const OBJECTS_SQL: &str = "SELECT table_schema, table_name, table_type \
     FROM information_schema.tables WHERE table_schema = DATABASE() ORDER BY table_name";

/// Every other connection to the server, longest-running first — this
/// connection's own is left out. `info` (the running statement, if any) is
/// truncated by the server unless `performance_schema` carries the full text,
/// which this does not read.
pub(crate) const PROCESSES_SQL: &str = "SELECT id, user, host, db, command, time, state, info \
     FROM information_schema.processlist \
     WHERE id <> CONNECTION_ID() \
     ORDER BY time DESC";

/// Every server variable, with the "Changed" column the last one for
/// `ui::server_variables` to find by position — `yes` unless
/// `performance_schema.variables_info` says the running value is still the
/// one compiled in.
pub(crate) const VARIABLES_SQL: &str = "SELECT v.VARIABLE_NAME, v.VARIABLE_VALUE, i.VARIABLE_SOURCE, \
     CASE WHEN i.VARIABLE_SOURCE = 'COMPILED' THEN '' ELSE 'yes' END AS changed \
     FROM performance_schema.global_variables v \
     JOIN performance_schema.variables_info i ON v.VARIABLE_NAME = i.VARIABLE_NAME \
     ORDER BY v.VARIABLE_NAME";

/// Whether `performance_schema` is on — it is a compile-time-default-on
/// server variable that cannot be flipped at runtime, and the digest table
/// it feeds is empty without it.
pub(crate) const DIGEST_AVAILABLE_SQL: &str = "SHOW VARIABLES LIKE 'performance_schema'";

/// The digest itself, worst average time first. `*_timer_wait` columns are
/// picoseconds, so `/ 1000000000.0` gives milliseconds.
pub(crate) const DIGEST_SQL: &str = "SELECT digest_text, count_star, \
     round(sum_timer_wait / 1000000000.0, 2) AS total_time_ms, \
     round(avg_timer_wait / 1000000000.0, 2) AS avg_time_ms, sum_rows_examined \
     FROM performance_schema.events_statements_summary_by_digest \
     WHERE digest_text IS NOT NULL \
     ORDER BY avg_timer_wait DESC LIMIT 200";

/// Stored functions and procedures in the current database, with the parameter
/// types that tell one signature from another. MySQL has no sequences. A
/// function's return value is a row in `parameters` at position 0, so the
/// subquery counts from 1 and a routine of no arguments comes back NULL.
pub(crate) const ROUTINES_SQL: &str = "SELECT r.routine_schema, r.routine_name, r.routine_type, \
     (SELECT GROUP_CONCAT(p.dtd_identifier ORDER BY p.ordinal_position SEPARATOR ', ') \
      FROM information_schema.parameters p \
      WHERE p.specific_schema = r.routine_schema \
        AND p.specific_name = r.specific_name \
        AND p.ordinal_position > 0) \
     FROM information_schema.routines r \
     WHERE r.routine_schema = DATABASE() ORDER BY r.routine_name";

/// Every column in the current database, for the catalog search: owning
/// database, owning table, whether that object is a view, column name, and the
/// full type spec [`columns_sql`] produces for one table.
pub(crate) const CATALOG_COLUMNS_SQL: &str = "SELECT c.table_schema, c.table_name, t.table_type, \
     c.column_name, c.column_type \
     FROM information_schema.columns c \
     JOIN information_schema.tables t \
       ON t.table_schema = c.table_schema AND t.table_name = c.table_name \
     WHERE c.table_schema = DATABASE() \
     ORDER BY c.table_name, c.ordinal_position";

/// Every index in the current database: owning database, owning table, index
/// name, and its columns in index order. The kind is always a table, but it
/// rides along so every catalog row has the same five fields.
pub(crate) const CATALOG_INDEXES_SQL: &str = "SELECT table_schema, table_name, 'BASE TABLE', index_name, \
     GROUP_CONCAT(column_name ORDER BY seq_in_index SEPARATOR ', ') \
     FROM information_schema.statistics \
     WHERE table_schema = DATABASE() \
     GROUP BY table_schema, table_name, index_name \
     ORDER BY table_name, index_name";

/// Every trigger in the current database: owning database, owning table,
/// trigger name, and when it fires.
pub(crate) const CATALOG_TRIGGERS_SQL: &str = "SELECT trigger_schema, event_object_table, 'BASE TABLE', trigger_name, \
     CONCAT(action_timing, ' ', event_manipulation) \
     FROM information_schema.triggers \
     WHERE trigger_schema = DATABASE() \
     ORDER BY event_object_table, trigger_name";

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

    let mut pool_options = pool_options();

    // MySQL has no connect option for either setting, so every connection the
    // pool opens is told as it comes up.
    //
    // A read-only connection is read-only at the server too: the client-side
    // check in `Connection::refuse_write` only speaks for statements it can
    // recognise.
    let read_only = config.safety.is_read_only();
    let timeout = config.statement_timeout.filter(|&seconds| seconds > 0);
    if read_only || timeout.is_some() {
        pool_options =
            pool_options.after_connect(move |connection: &mut sqlx::MySqlConnection, _| {
                Box::pin(async move {
                    if read_only {
                        connection
                            .execute("SET SESSION TRANSACTION READ ONLY")
                            .await?;
                    }
                    if let Some(seconds) = timeout {
                        set_statement_timeout(connection, seconds).await;
                    }
                    Ok(())
                })
            });
    }

    let pool = pool_options.connect_with(options).await?;
    Ok(pool)
}

/// Limit how long one statement may run on `connection`.
///
/// MySQL spells it `max_execution_time`, in milliseconds, and applies it to
/// `SELECT` only; MariaDB has no such variable and spells it
/// `max_statement_time`, in seconds, for every statement. A server with
/// neither (MySQL before 5.7.8) is left without a limit rather than refused
/// a connection over a setting that is only a guard.
async fn set_statement_timeout(connection: &mut sqlx::MySqlConnection, seconds: u32) {
    let mysql = format!(
        "SET SESSION max_execution_time = {}",
        u64::from(seconds) * 1000
    );
    if sqlx::raw_sql(AssertSqlSafe(mysql))
        .execute(&mut *connection)
        .await
        .is_ok()
    {
        return;
    }
    let mariadb = format!("SET SESSION max_statement_time = {seconds}");
    let _ = sqlx::raw_sql(AssertSqlSafe(mariadb))
        .execute(&mut *connection)
        .await;
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
        quote_literal_for(Engine::MySql, table)
    )
}

/// Columns of a table in the current database: name, full type spec (as
/// `COLUMN_TYPE` already spells it, e.g. `varchar(255)`), nullability,
/// default, then the four MySQL-only properties `schema::parse_columns`
/// reads into `ColumnDef::mysql_extra` — a full restate (`MODIFY`/`CHANGE
/// COLUMN`) has to fold these back in itself, since MySQL has no narrower way
/// to change one property of a column.
pub(crate) fn columns_sql(table: &str) -> String {
    format!(
        "SELECT column_name, column_type, (is_nullable = 'YES'), column_default, \
         extra, collation_name, column_comment, generation_expression \
         FROM information_schema.columns \
         WHERE table_schema = DATABASE() AND table_name = {} \
         ORDER BY ordinal_position",
        quote_literal_for(Engine::MySql, table)
    )
}

/// One row per index: name, its columns in index order, unique?, primary key?
/// (`PRIMARY` is MySQL's fixed name for the primary key's own index.)
pub(crate) fn indexes_sql(table: &str) -> String {
    format!(
        "SELECT index_name, \
         GROUP_CONCAT(column_name ORDER BY seq_in_index SEPARATOR ','), \
         (non_unique = 0), (index_name = 'PRIMARY') \
         FROM information_schema.statistics \
         WHERE table_schema = DATABASE() AND table_name = {} \
         GROUP BY index_name, non_unique \
         ORDER BY index_name",
        quote_literal_for(Engine::MySql, table)
    )
}

/// One row per foreign key: name, local columns, referenced schema/table/
/// columns (matching order), `ON DELETE`/`ON UPDATE`.
pub(crate) fn foreign_keys_sql(table: &str) -> String {
    format!(
        "SELECT kcu.constraint_name, \
         GROUP_CONCAT(kcu.column_name ORDER BY kcu.ordinal_position SEPARATOR ','), \
         kcu.referenced_table_schema, kcu.referenced_table_name, \
         GROUP_CONCAT(kcu.referenced_column_name ORDER BY kcu.ordinal_position SEPARATOR ','), \
         rc.delete_rule, rc.update_rule \
         FROM information_schema.key_column_usage kcu \
         JOIN information_schema.referential_constraints rc \
           ON rc.constraint_name = kcu.constraint_name \
           AND rc.constraint_schema = kcu.constraint_schema \
         WHERE kcu.table_schema = DATABASE() AND kcu.table_name = {} \
           AND kcu.referenced_table_name IS NOT NULL \
         GROUP BY kcu.constraint_name, kcu.referenced_table_schema, \
           kcu.referenced_table_name, rc.delete_rule, rc.update_rule \
         ORDER BY kcu.constraint_name",
        quote_literal_for(Engine::MySql, table)
    )
}

pub(crate) fn rows_affected(result: &MySqlQueryResult) -> u64 {
    result.rows_affected()
}

/// Whether `error` is MySQL refusing a statement over the prepared protocol
/// (error 1295) — `BEGIN`, `SAVEPOINT`, `LOCK TABLES`, `USE`, and the like,
/// which only the text protocol accepts.
pub(crate) fn unpreparable(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<sqlx::Error>()
        .and_then(|error| error.as_database_error())
        .and_then(|error| error.try_downcast_ref::<sqlx::mysql::MySqlDatabaseError>())
        .is_some_and(|error| error.number() == 1295)
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
        "BOOLEAN" | "TINYINT" => decode!(row, index, i8).or_else(|| decode!(row, index, u8)),
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
        "YEAR" => decode!(row, index, u16),
        // A `BIT(n)` is shown as its digits rather than as a number, which is
        // also what `typed_placeholder` converts back on the way in — MySQL
        // reads a string bound to a `BIT` column as raw bytes otherwise.
        "BIT" => row
            .try_get::<u64, _>(index)
            .ok()
            .map(|bits| format!("{bits:b}")),
        "DATETIME" => decode!(row, index, chrono::NaiveDateTime),
        // A `TIMESTAMP` only decodes through an offset-aware type, but the
        // server sends the same wall clock a `DATETIME` would, so the two are
        // shown alike rather than labelled with an offset the server never
        // gave.
        "TIMESTAMP" => row
            .try_get::<chrono::DateTime<chrono::Utc>, _>(index)
            .ok()
            .map(|value| value.naive_utc().to_string()),
        // `TIME` is a signed span of up to 838 hours, so it only falls back to
        // the driver's own type when it is outside a clock time.
        "TIME" => decode!(row, index, chrono::NaiveTime)
            .or_else(|| decode!(row, index, sqlx::mysql::types::MySqlTime)),
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

/// The raw bytes of one column, for a binary value preview asking for the
/// bytes `cell` only ever summarizes. `Vec<u8>` alone does not tell a NULL
/// apart from an empty value, so the target is `Option<Vec<u8>>`.
pub(crate) fn raw_bytes(row: &MySqlRow, index: usize) -> Option<Vec<u8>> {
    row.try_get::<Option<Vec<u8>>, _>(index).ok().flatten()
}
