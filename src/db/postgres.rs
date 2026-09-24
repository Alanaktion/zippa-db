//! PostgreSQL driver (also covers CockroachDB / Redshift).

use anyhow::Result;
use sqlx::postgres::types::{Oid, PgInterval, PgMoney, PgTimeTz};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions, PgQueryResult, PgRow};
use sqlx::{Row, TypeInfo, ValueRef};

use super::query::{self, Cell};
use super::{ConnectionConfig, POOL_SIZE, quote_literal};

/// Databases on this server the user can connect to.
pub(crate) const DATABASES_SQL: &str = "SELECT datname FROM pg_database \
     WHERE datistemplate = false AND datallowconn ORDER BY datname";

/// Tables and views outside the system schemas.
pub(crate) const OBJECTS_SQL: &str = "SELECT table_schema, table_name, table_type \
     FROM information_schema.tables \
     WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
     ORDER BY table_schema, table_name";

/// Every other backend connected to this database, oldest activity first —
/// the process list's own connection is left out, and so is a background
/// worker, which has no `datname` of its own.
pub(crate) const PROCESSES_SQL: &str = "SELECT pid, usename, datname, client_addr, state, \
     now() - query_start AS duration, query \
     FROM pg_stat_activity \
     WHERE pid <> pg_backend_pid() AND datname IS NOT NULL \
     ORDER BY query_start";

/// Every server setting, with the "Changed" column the last one for
/// `ui::server_variables` to find by position — `yes` when the running value
/// no longer matches the compiled-in default (`boot_val`), blank otherwise.
pub(crate) const VARIABLES_SQL: &str = "SELECT name, setting, unit, context, short_desc, \
     CASE WHEN setting IS DISTINCT FROM boot_val THEN 'yes' ELSE '' END AS changed \
     FROM pg_settings ORDER BY name";

/// Whether `pg_stat_statements` is installed on this database — it is a
/// contrib extension, not built in, and the digest has nothing to read
/// without it.
pub(crate) const DIGEST_AVAILABLE_SQL: &str =
    "SELECT 1 FROM pg_extension WHERE extname = 'pg_stat_statements'";

/// The digest itself, worst mean time first. `total_exec_time`/`mean_exec_time`
/// are the column names from Postgres 13 on, when the `_exec_` infix was
/// added to tell them apart from planning time; an older server has no
/// digest view for this to fall back to.
pub(crate) const DIGEST_SQL: &str = "SELECT query, calls, \
     round(total_exec_time::numeric, 2) AS total_time_ms, \
     round(mean_exec_time::numeric, 2) AS mean_time_ms, rows \
     FROM pg_stat_statements ORDER BY mean_exec_time DESC LIMIT 200";

/// Functions, procedures and sequences outside the system schemas and the
/// extensions, with the argument types that tell overloads apart.
pub(crate) const ROUTINES_SQL: &str = "SELECT n.nspname, p.proname, \
     CASE p.prokind WHEN 'p' THEN 'PROCEDURE' ELSE 'FUNCTION' END, \
     pg_get_function_identity_arguments(p.oid) \
     FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
     WHERE p.prokind IN ('f', 'p') \
     AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
     AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e') \
     UNION ALL \
     SELECT n.nspname, c.relname, 'SEQUENCE', NULL \
     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE c.relkind = 'S' AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
     ORDER BY 1, 2, 4";

/// Every column outside the system schemas, for the catalog search: owning
/// schema, owning table, whether that object is a view, column name, and the
/// same type spelling [`columns_sql`] produces for one table.
pub(crate) const CATALOG_COLUMNS_SQL: &str = "SELECT c.table_schema, c.table_name, \
     CASE WHEN t.table_type = 'VIEW' THEN 'VIEW' ELSE 'BASE TABLE' END, \
     c.column_name, \
     CASE \
       WHEN c.data_type = 'character varying' AND c.character_maximum_length IS NOT NULL \
         THEN 'character varying(' || c.character_maximum_length || ')' \
       WHEN c.data_type = 'character' AND c.character_maximum_length IS NOT NULL \
         THEN 'character(' || c.character_maximum_length || ')' \
       WHEN c.data_type = 'numeric' AND c.numeric_precision IS NOT NULL \
         THEN 'numeric(' || c.numeric_precision || ',' || COALESCE(c.numeric_scale, 0) || ')' \
       ELSE c.data_type \
     END \
     FROM information_schema.columns c \
     JOIN information_schema.tables t \
       ON t.table_schema = c.table_schema AND t.table_name = c.table_name \
     WHERE c.table_schema NOT IN ('pg_catalog', 'information_schema') \
     ORDER BY c.table_schema, c.table_name, c.ordinal_position";

/// Every index outside the system schemas: owning schema, owning table, index
/// name, and its columns in index order. The kind is always a table, but it
/// rides along so every catalog row has the same five fields.
pub(crate) const CATALOG_INDEXES_SQL: &str = "SELECT n.nspname, t.relname, 'BASE TABLE', i.relname, \
     array_to_string(ARRAY( \
       SELECT a.attname FROM pg_attribute a \
       WHERE a.attrelid = t.oid AND a.attnum = ANY(ix.indkey) \
       ORDER BY array_position(ix.indkey, a.attnum) \
     ), ', ') \
     FROM pg_index ix \
     JOIN pg_class t ON t.oid = ix.indrelid \
     JOIN pg_class i ON i.oid = ix.indexrelid \
     JOIN pg_namespace n ON n.oid = t.relnamespace \
     WHERE n.nspname NOT IN ('pg_catalog', 'information_schema') \
     ORDER BY n.nspname, t.relname, i.relname";

/// Every user trigger: owning schema, owning table or view, trigger name, and
/// the timing and events the trigger fires on.
pub(crate) const CATALOG_TRIGGERS_SQL: &str = "SELECT n.nspname, c.relname, \
     CASE WHEN c.relkind = 'v' THEN 'VIEW' ELSE 'BASE TABLE' END, t.tgname, \
     (CASE WHEN t.tgtype & 2 <> 0 THEN 'BEFORE ' \
           WHEN t.tgtype & 64 <> 0 THEN 'INSTEAD OF ' \
           ELSE 'AFTER ' END) || \
     concat_ws(' OR ', \
       CASE WHEN t.tgtype & 4 <> 0 THEN 'INSERT' END, \
       CASE WHEN t.tgtype & 16 <> 0 THEN 'UPDATE' END, \
       CASE WHEN t.tgtype & 8 <> 0 THEN 'DELETE' END, \
       CASE WHEN t.tgtype & 32 <> 0 THEN 'TRUNCATE' END) \
     FROM pg_trigger t \
     JOIN pg_class c ON c.oid = t.tgrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE NOT t.tgisinternal \
       AND n.nspname NOT IN ('pg_catalog', 'information_schema') \
     ORDER BY n.nspname, c.relname, t.tgname";

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

    // A read-only connection is read-only at the server too: the client-side
    // check in `Connection::refuse_write` only speaks for statements it can
    // recognise.
    if config.safety.is_read_only() {
        options = options.options([("default_transaction_read_only", "on")]);
    }

    let pool = PgPoolOptions::new()
        .max_connections(POOL_SIZE)
        .connect_with(options)
        .await?;
    Ok(pool)
}

/// How many raw units of `money` make up one whole one — `100` for a
/// currency with two fraction digits, `1` for one with none, and so on.
///
/// Not the fixed `100` the type's name suggests: `money` scales by
/// `lc_monetary`'s fraction-digit count, which is two almost everywhere but
/// zero for a currency like the yen — confirmed against a live server with
/// `lc_monetary` set to `ja_JP`, where `'1'::money` comes back raw `1`
/// rather than `100`. Asking the server this directly, once per connection,
/// sidesteps parsing a locale name to guess its digit count.
pub(crate) async fn money_scale(pool: &PgPool) -> i64 {
    match sqlx::query_scalar::<_, PgMoney>("SELECT '1'::money")
        .fetch_one(pool)
        .await
    {
        Ok(PgMoney(raw)) if raw > 0 => raw,
        _ => DEFAULT_MONEY_SCALE,
    }
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

/// Columns of a table: name, driver-spelled type, nullability, default.
pub(crate) fn columns_sql(schema: &str, table: &str) -> String {
    format!(
        "SELECT column_name, \
         CASE \
           WHEN data_type = 'character varying' AND character_maximum_length IS NOT NULL \
             THEN 'character varying(' || character_maximum_length || ')' \
           WHEN data_type = 'character' AND character_maximum_length IS NOT NULL \
             THEN 'character(' || character_maximum_length || ')' \
           WHEN data_type = 'numeric' AND numeric_precision IS NOT NULL \
             THEN 'numeric(' || numeric_precision || ',' || COALESCE(numeric_scale, 0) || ')' \
           ELSE data_type \
         END, \
         (is_nullable = 'YES'), column_default \
         FROM information_schema.columns \
         WHERE table_schema = {} AND table_name = {} \
         ORDER BY ordinal_position",
        quote_literal(schema),
        quote_literal(table)
    )
}

/// One row per index: name, its columns in index order, unique?, primary key?
pub(crate) fn indexes_sql(schema: &str, table: &str) -> String {
    format!(
        "SELECT i.relname, \
         array_to_string(ARRAY( \
           SELECT a.attname FROM pg_attribute a \
           WHERE a.attrelid = t.oid AND a.attnum = ANY(ix.indkey) \
           ORDER BY array_position(ix.indkey, a.attnum) \
         ), ','), \
         ix.indisunique, ix.indisprimary \
         FROM pg_index ix \
         JOIN pg_class t ON t.oid = ix.indrelid \
         JOIN pg_class i ON i.oid = ix.indexrelid \
         JOIN pg_namespace n ON n.oid = t.relnamespace \
         WHERE n.nspname = {} AND t.relname = {} \
         ORDER BY i.relname",
        quote_literal(schema),
        quote_literal(table)
    )
}

/// One row per foreign key: name, local columns, referenced schema/table/
/// columns (matching order), `ON DELETE`/`ON UPDATE`.
pub(crate) fn foreign_keys_sql(schema: &str, table: &str) -> String {
    format!(
        "SELECT con.conname, \
         array_to_string(ARRAY( \
           SELECT a.attname FROM pg_attribute a \
           WHERE a.attrelid = con.conrelid AND a.attnum = ANY(con.conkey) \
           ORDER BY array_position(con.conkey, a.attnum) \
         ), ','), \
         fn.nspname, fc.relname, \
         array_to_string(ARRAY( \
           SELECT a.attname FROM pg_attribute a \
           WHERE a.attrelid = con.confrelid AND a.attnum = ANY(con.confkey) \
           ORDER BY array_position(con.confkey, a.attnum) \
         ), ','), \
         rc.delete_rule, rc.update_rule \
         FROM pg_constraint con \
         JOIN pg_class c ON c.oid = con.conrelid \
         JOIN pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_class fc ON fc.oid = con.confrelid \
         JOIN pg_namespace fn ON fn.oid = fc.relnamespace \
         JOIN information_schema.referential_constraints rc \
           ON rc.constraint_name = con.conname AND rc.constraint_schema = n.nspname \
         WHERE con.contype = 'f' AND n.nspname = {} AND c.relname = {} \
         ORDER BY con.conname",
        quote_literal(schema),
        quote_literal(table)
    )
}

pub(crate) fn rows_affected(result: &PgQueryResult) -> u64 {
    result.rows_affected()
}

/// The scale [`money_scale`]'s probe falls back to if it cannot run, and what
/// `Connection` carries for the other two engines, which never read it — two
/// fraction digits, the common case.
pub(crate) const DEFAULT_MONEY_SCALE: i64 = 100;

pub(crate) fn cell(row: &PgRow, index: usize, money_scale: i64) -> Cell {
    let Ok(raw) = row.try_get_raw(index) else {
        return None;
    };
    if raw.is_null() {
        return None;
    }
    let type_name = raw.type_info().name().to_string();

    // An array reads as its element type repeated, so each arm below covers
    // both `INT4` and `INT4[]`; only the decode target changes.
    let (base, is_array) = match type_name.strip_suffix("[]") {
        Some(base) => (base, true),
        None => (type_name.as_str(), false),
    };

    /// Decode the cell as `$ty` and render it with `$format`, or as an array
    /// of `$ty` written the way Postgres writes one.
    macro_rules! value {
        ($ty:ty) => {
            value!($ty, |value: $ty| value.to_string())
        };
        ($ty:ty, $format:expr) => {
            if is_array {
                row.try_get::<Vec<Option<$ty>>, _>(index)
                    .ok()
                    .map(|values| array_literal(values.into_iter().map(|v| v.map($format))))
            } else {
                row.try_get::<$ty, _>(index).ok().map($format)
            }
        };
    }

    let value = match base {
        "BOOL" => value!(bool, query::boolean),
        "INT2" => value!(i16),
        "INT4" => value!(i32),
        "INT8" => value!(i64),
        "OID" => value!(Oid, |value: Oid| value.0.to_string()),
        "FLOAT4" => value!(f32),
        "FLOAT8" => value!(f64),
        "NUMERIC" => value!(sqlx::types::BigDecimal),
        "MONEY" => value!(PgMoney, |value: PgMoney| format_money(value, money_scale)),
        "UUID" => value!(sqlx::types::Uuid),
        "JSON" | "JSONB" => value!(sqlx::types::JsonValue),
        "DATE" => value!(chrono::NaiveDate),
        "TIME" => value!(chrono::NaiveTime),
        "TIMETZ" => value!(PgTimeTz<chrono::NaiveTime, chrono::FixedOffset>, format_timetz),
        "TIMESTAMP" => value!(chrono::NaiveDateTime),
        // The driver hands back an instant rather than the text Postgres would
        // have printed, so the offset is ours to pick: show it where the user
        // is, and spell the offset out so the value still means the same
        // instant when it is written back.
        "TIMESTAMPTZ" => value!(
            chrono::DateTime<chrono::Local>,
            |value: chrono::DateTime<chrono::Local>| {
                value.format("%Y-%m-%d %H:%M:%S%.f%:z").to_string()
            }
        ),
        "INTERVAL" => value!(PgInterval, format_interval),
        "INET" | "CIDR" => value!(sqlx::types::ipnetwork::IpNetwork, |value| {
            format_ip(value, base == "INET")
        }),
        "MACADDR" => value!(sqlx::types::mac_address::MacAddress),
        "BIT" | "VARBIT" => value!(sqlx::types::BitVec, format_bits),
        "BYTEA" => value!(Vec<u8>, |bytes: Vec<u8>| format!("<{} bytes>", bytes.len())),
        // `xml` has no Rust mapping in sqlx, but its binary encoding is the
        // document text, so the raw bytes are the value.
        "xml" if !is_array => raw.as_str().ok().map(str::to_string),
        _ => value!(String, |value: String| value),
    };

    value.or_else(|| query::unsupported(&type_name))
}

/// The raw bytes of one column, for a binary value preview asking for the
/// bytes `cell` only ever summarizes. `Vec<u8>` alone does not tell a NULL
/// apart from an empty value, so the target is `Option<Vec<u8>>`.
pub(crate) fn raw_bytes(row: &PgRow, index: usize) -> Option<Vec<u8>> {
    row.try_get::<Option<Vec<u8>>, _>(index).ok().flatten()
}

/// `{a,b,NULL}`, quoting the elements that need it.
fn array_literal(elements: impl Iterator<Item = Option<String>>) -> String {
    let mut out = String::from("{");
    for (index, element) in elements.enumerate() {
        if index > 0 {
            out.push(',');
        }
        match element {
            None => out.push_str("NULL"),
            Some(value) if needs_quoting(&value) => {
                out.push('"');
                for character in value.chars() {
                    if character == '"' || character == '\\' {
                        out.push('\\');
                    }
                    out.push(character);
                }
                out.push('"');
            }
            Some(value) => out.push_str(&value),
        }
    }
    out.push('}');
    out
}

/// True for an array element Postgres would not read back as written: one that
/// is empty, holds a delimiter or quote, has whitespace at either end, or
/// would otherwise be mistaken for the unquoted `NULL`.
fn needs_quoting(element: &str) -> bool {
    element.is_empty()
        || element.eq_ignore_ascii_case("null")
        || element.starts_with(char::is_whitespace)
        || element.ends_with(char::is_whitespace)
        || element.contains(['{', '}', ',', '"', '\\'])
}

/// `1 mon 2 days 03:04:05`, which is also how Postgres reads it back.
fn format_interval(interval: PgInterval) -> String {
    let mut parts: Vec<String> = Vec::new();

    let years = interval.months / 12;
    let months = interval.months % 12;
    if years != 0 {
        parts.push(format!("{years} {}", unit(years, "year", "years")));
    }
    if months != 0 {
        parts.push(format!("{months} {}", unit(months, "mon", "mons")));
    }
    if interval.days != 0 {
        parts.push(format!(
            "{} {}",
            interval.days,
            unit(interval.days, "day", "days")
        ));
    }

    // The clock part carries its own sign, so it is written whole rather than
    // split into fields, and is the only part shown when everything is zero.
    if interval.microseconds != 0 || parts.is_empty() {
        let sign = if interval.microseconds < 0 { "-" } else { "" };
        let total = interval.microseconds.unsigned_abs();
        let (seconds, microseconds) = (total / 1_000_000, total % 1_000_000);
        let clock = format!(
            "{sign}{:02}:{:02}:{:02}",
            seconds / 3_600,
            (seconds / 60) % 60,
            seconds % 60
        );
        parts.push(match microseconds {
            0 => clock,
            _ => format!("{clock}.{:06}", microseconds)
                .trim_end_matches('0')
                .to_string(),
        });
    }

    parts.join(" ")
}

fn unit(count: i32, one: &'static str, many: &'static str) -> &'static str {
    if count.abs() == 1 { one } else { many }
}

/// `03:04:05+02:00`.
fn format_timetz(value: PgTimeTz<chrono::NaiveTime, chrono::FixedOffset>) -> String {
    use chrono::Offset as _;
    format!("{}{}", value.time.format("%H:%M:%S%.f"), value.offset.fix())
}

/// `12.34`, the way Postgres prints `money` without its currency symbol —
/// which is also the form it reads back. `scale` is [`money_scale`]'s probe:
/// no decimal point at all for a currency with no fraction digits, rather
/// than assuming two.
fn format_money(money: PgMoney, scale: i64) -> String {
    let sign = if money.0 < 0 { "-" } else { "" };
    let units = money.0.unsigned_abs();
    let scale = scale.unsigned_abs().max(1);
    let digits = scale.ilog10() as usize;
    if digits == 0 {
        format!("{sign}{units}")
    } else {
        format!("{sign}{}.{:0digits$}", units / scale, units % scale)
    }
}

/// `192.168.0.1` for an `inet` host, `10.0.0.0/8` where the prefix says
/// something. A `cidr` always keeps its prefix.
fn format_ip(network: sqlx::types::ipnetwork::IpNetwork, is_inet: bool) -> String {
    let host_bits = match network {
        sqlx::types::ipnetwork::IpNetwork::V4(_) => 32,
        sqlx::types::ipnetwork::IpNetwork::V6(_) => 128,
    };
    if is_inet && network.prefix() == host_bits {
        return network.ip().to_string();
    }
    network.to_string()
}

/// `10101010`, most significant bit first.
fn format_bits(bits: sqlx::types::BitVec) -> String {
    bits.iter().map(|bit| if bit { '1' } else { '0' }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interval(months: i32, days: i32, microseconds: i64) -> String {
        format_interval(PgInterval {
            months,
            days,
            microseconds,
        })
    }

    #[test]
    fn intervals_are_written_the_way_postgres_reads_them() {
        assert_eq!(interval(0, 1, 7_384_000_000), "1 day 02:03:04");
        assert_eq!(interval(14, 0, 0), "1 year 2 mons");
        assert_eq!(interval(-1, -2, 0), "-1 mon -2 days");
        assert_eq!(interval(0, 0, 0), "00:00:00");
        assert_eq!(interval(0, 0, -1_000_000), "-00:00:01");
        assert_eq!(interval(0, 0, 1_500_000), "00:00:01.5");
        assert_eq!(interval(0, 0, 90_061_000_000), "25:01:01");
    }

    #[test]
    fn array_elements_are_quoted_only_where_they_have_to_be() {
        let literal = |values: &[Option<&str>]| {
            array_literal(
                values
                    .iter()
                    .map(|value| value.map(str::to_string))
                    .collect::<Vec<_>>()
                    .into_iter(),
            )
        };

        assert_eq!(literal(&[Some("1"), Some("2")]), "{1,2}");
        assert_eq!(literal(&[]), "{}");
        assert_eq!(literal(&[Some("x"), None]), "{x,NULL}");
        // A value that would otherwise read back as the empty-element NULL.
        assert_eq!(literal(&[Some("NULL")]), r#"{"NULL"}"#);
        assert_eq!(literal(&[Some("")]), r#"{""}"#);
        assert_eq!(literal(&[Some("a,b")]), r#"{"a,b"}"#);
        assert_eq!(literal(&[Some(" a ")]), r#"{" a "}"#);
        assert_eq!(literal(&[Some(r#"he said "hi""#)]), r#"{"he said \"hi\""}"#);
        assert_eq!(literal(&[Some(r"back\slash")]), r#"{"back\\slash"}"#);
    }

    #[test]
    fn money_keeps_both_digits_at_the_common_scale() {
        assert_eq!(format_money(PgMoney(1234), 100), "12.34");
        assert_eq!(format_money(PgMoney(5), 100), "0.05");
        assert_eq!(format_money(PgMoney(-1234), 100), "-12.34");
        assert_eq!(format_money(PgMoney(0), 100), "0.00");
    }

    #[test]
    fn money_has_no_decimal_point_at_all_with_no_fraction_digits() {
        // A currency like the yen, whose `lc_monetary` scales `money` by 1
        // rather than 100 — `12.34` here would not be a value the same
        // server would read back as the same amount.
        assert_eq!(format_money(PgMoney(1234), 1), "1234");
        assert_eq!(format_money(PgMoney(-7), 1), "-7");
    }

    #[test]
    fn money_pads_three_fraction_digits_the_same_way() {
        // A currency like the Bahraini dinar, three fraction digits.
        assert_eq!(format_money(PgMoney(1234), 1000), "1.234");
        assert_eq!(format_money(PgMoney(5), 1000), "0.005");
    }

    #[test]
    fn a_host_address_drops_its_full_width_prefix() {
        let parse = |text: &str| text.parse::<sqlx::types::ipnetwork::IpNetwork>().unwrap();
        assert_eq!(format_ip(parse("192.168.0.1/32"), true), "192.168.0.1");
        assert_eq!(format_ip(parse("192.168.0.1/24"), true), "192.168.0.1/24");
        // A `cidr` says what it covers even when that is one address.
        assert_eq!(format_ip(parse("192.168.0.1/32"), false), "192.168.0.1/32");
        assert_eq!(format_ip(parse("::1/128"), true), "::1");
    }
}
