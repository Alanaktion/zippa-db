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

/// Frac digits `money` values are scaled by. Postgres takes this from
/// `lc_monetary`, which is two digits in every locale it ships with.
const MONEY_SCALE: i64 = 100;

pub(crate) fn cell(row: &PgRow, index: usize) -> Cell {
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
        "MONEY" => value!(PgMoney, format_money),
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
/// which is also the form it reads back.
fn format_money(money: PgMoney) -> String {
    let sign = if money.0 < 0 { "-" } else { "" };
    let units = money.0.unsigned_abs();
    let scale = MONEY_SCALE.unsigned_abs();
    format!("{sign}{}.{:02}", units / scale, units % scale)
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
    fn money_keeps_both_digits() {
        assert_eq!(format_money(PgMoney(1234)), "12.34");
        assert_eq!(format_money(PgMoney(5)), "0.05");
        assert_eq!(format_money(PgMoney(-1234)), "-12.34");
        assert_eq!(format_money(PgMoney(0)), "0.00");
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
