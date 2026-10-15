//! Writing SQL by hand: quoting names, quoting literals, and placing bind
//! parameters so every engine accepts them.
//!
//! Used wherever the app generates a statement — the table view's paging,
//! filtering, and writes — never for the user's own editor buffer, which is
//! run verbatim.

use super::config::Engine;

/// Words at least one of the three engines reserves, so a name spelled like
/// one cannot be pasted in bare: `order`, `key`, and `rank` turn up as real
/// column names, and Postgres reads a bare `user` as `CURRENT_USER` without
/// a word of complaint. Sorted, for [`is_reserved`]'s binary search.
const RESERVED: &[&str] = &[
    "ACCESSIBLE",
    "ADD",
    "ALL",
    "ALTER",
    "ANALYSE",
    "ANALYZE",
    "AND",
    "ANY",
    "ARRAY",
    "AS",
    "ASC",
    "ASYMMETRIC",
    "BETWEEN",
    "BIGINT",
    "BINARY",
    "BLOB",
    "BOTH",
    "BY",
    "CALL",
    "CASCADE",
    "CASE",
    "CAST",
    "CHANGE",
    "CHAR",
    "CHARACTER",
    "CHECK",
    "COLLATE",
    "COLUMN",
    "CONDITION",
    "CONSTRAINT",
    "CONTINUE",
    "CONVERT",
    "CREATE",
    "CROSS",
    "CUBE",
    "CURRENT",
    "CURRENT_CATALOG",
    "CURRENT_DATE",
    "CURRENT_ROLE",
    "CURRENT_TIME",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "CURSOR",
    "DATABASE",
    "DATABASES",
    "DECIMAL",
    "DECLARE",
    "DEFAULT",
    "DEFERRABLE",
    "DELAYED",
    "DELETE",
    "DENSE_RANK",
    "DESC",
    "DESCRIBE",
    "DISTINCT",
    "DISTINCTROW",
    "DIV",
    "DO",
    "DOUBLE",
    "DROP",
    "DUAL",
    "EACH",
    "ELSE",
    "ELSEIF",
    "EMPTY",
    "ENCLOSED",
    "END",
    "ESCAPE",
    "ESCAPED",
    "EXCEPT",
    "EXISTS",
    "EXIT",
    "EXPLAIN",
    "FALSE",
    "FETCH",
    "FIRST_VALUE",
    "FLOAT",
    "FOR",
    "FORCE",
    "FOREIGN",
    "FREEZE",
    "FROM",
    "FULL",
    "FULLTEXT",
    "FUNCTION",
    "GENERATED",
    "GET",
    "GLOB",
    "GRANT",
    "GROUP",
    "GROUPING",
    "GROUPS",
    "HAVING",
    "IF",
    "IGNORE",
    "ILIKE",
    "IN",
    "INDEX",
    "INFILE",
    "INITIALLY",
    "INNER",
    "INOUT",
    "INSERT",
    "INT",
    "INTEGER",
    "INTERSECT",
    "INTERVAL",
    "INTO",
    "IS",
    "ISNULL",
    "ITERATE",
    "JOIN",
    "JSON_TABLE",
    "KEY",
    "KEYS",
    "KILL",
    "LAG",
    "LAST_VALUE",
    "LATERAL",
    "LEAD",
    "LEADING",
    "LEAVE",
    "LEFT",
    "LIKE",
    "LIMIT",
    "LINEAR",
    "LINES",
    "LOAD",
    "LOCALTIME",
    "LOCALTIMESTAMP",
    "LOCK",
    "LONG",
    "LOOP",
    "MATCH",
    "MOD",
    "MODIFIES",
    "NATURAL",
    "NOT",
    "NOTNULL",
    "NTILE",
    "NULL",
    "NUMERIC",
    "OF",
    "OFFSET",
    "ON",
    "ONLY",
    "OPTIMIZE",
    "OPTION",
    "OPTIONALLY",
    "OR",
    "ORDER",
    "OUT",
    "OUTER",
    "OUTFILE",
    "OVER",
    "OVERLAPS",
    "PARTITION",
    "PLACING",
    "PRECISION",
    "PRIMARY",
    "PROCEDURE",
    "PURGE",
    "RANGE",
    "RANK",
    "READ",
    "READS",
    "REAL",
    "RECURSIVE",
    "REFERENCES",
    "REGEXP",
    "RELEASE",
    "RENAME",
    "REPEAT",
    "REPLACE",
    "REQUIRE",
    "RESIGNAL",
    "RESTRICT",
    "RETURN",
    "RETURNING",
    "REVOKE",
    "RIGHT",
    "RLIKE",
    "ROW",
    "ROWS",
    "ROW_NUMBER",
    "SCHEMA",
    "SCHEMAS",
    "SELECT",
    "SENSITIVE",
    "SEPARATOR",
    "SESSION_USER",
    "SET",
    "SHOW",
    "SIGNAL",
    "SIMILAR",
    "SMALLINT",
    "SOME",
    "SPATIAL",
    "SQL",
    "STARTING",
    "STORED",
    "SYMMETRIC",
    "SYSTEM",
    "SYSTEM_USER",
    "TABLE",
    "TABLESAMPLE",
    "TERMINATED",
    "THEN",
    "TINYINT",
    "TO",
    "TRAILING",
    "TRIGGER",
    "TRUE",
    "UNDO",
    "UNION",
    "UNIQUE",
    "UNLOCK",
    "UNSIGNED",
    "UPDATE",
    "USAGE",
    "USE",
    "USER",
    "USING",
    "VALUES",
    "VARCHAR",
    "VARIADIC",
    "VERBOSE",
    "VIRTUAL",
    "WHEN",
    "WHERE",
    "WHILE",
    "WINDOW",
    "WITH",
    "WRITE",
    "XOR",
    "YEAR_MONTH",
    "ZEROFILL",
];

/// Whether `name`, in any case, is a word [`RESERVED`] lists.
pub(crate) fn is_reserved(name: &str) -> bool {
    RESERVED
        .binary_search(&name.to_ascii_uppercase().as_str())
        .is_ok()
}

/// Quote `name` when it would not survive being pasted into a statement bare.
///
/// Plain lower-case identifiers are left alone so the generated SQL reads the
/// way someone would type it — unless the name is a reserved word.
pub fn quote_identifier(name: &str, engine: Engine) -> String {
    let bare = !name.is_empty()
        && !name.starts_with(|character: char| character.is_ascii_digit())
        && name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
        && !is_reserved(name);

    if bare {
        return name.to_string();
    }

    match engine {
        Engine::MySql => format!("`{}`", name.replace('`', "``")),
        Engine::Postgres | Engine::Sqlite => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// `CREATE DATABASE` for `name`, or `None` when there is nothing to create:
/// a SQLite database is a file, and an empty name would not parse.
///
/// The name is quoted with [`quote_identifier`].
pub fn create_database_sql(name: &str, engine: Engine) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    match engine {
        Engine::Postgres | Engine::MySql => Some(format!(
            "CREATE DATABASE {}",
            quote_identifier(name, engine)
        )),
        Engine::Sqlite => None,
    }
}

/// `CREATE USER`/`CREATE ROLE` for a login user, or `None` when there is
/// nothing valid to create: an empty name never parses, MySQL needs the
/// `user@host` pair, and SQLite has no users.
///
/// Postgres makes a role that can log in; MySQL makes `'name'@'host'`
/// identified by the password. The password is quoted as a literal for the
/// engine.
pub fn create_user_sql(
    name: &str,
    host: Option<&str>,
    password: &str,
    engine: Engine,
) -> Option<String> {
    let user = user_target(name, host, engine)?;
    let password = quote_literal_for(engine, password);
    Some(match engine {
        Engine::Postgres => format!("CREATE ROLE {user} WITH LOGIN PASSWORD {password}"),
        _ => format!("CREATE USER {user} IDENTIFIED BY {password}"),
    })
}

/// `DROP USER`/`DROP ROLE`, or `None` on the same inputs
/// [`create_user_sql`] refuses.
pub fn drop_user_sql(name: &str, host: Option<&str>, engine: Engine) -> Option<String> {
    let user = user_target(name, host, engine)?;
    Some(match engine {
        Engine::Postgres => format!("DROP ROLE {user}"),
        _ => format!("DROP USER {user}"),
    })
}

/// Set a user's password, or `None` on the same inputs the other two refuse.
pub fn set_user_password_sql(
    name: &str,
    host: Option<&str>,
    password: &str,
    engine: Engine,
) -> Option<String> {
    let user = user_target(name, host, engine)?;
    let password = quote_literal_for(engine, password);
    Some(match engine {
        Engine::Postgres => format!("ALTER ROLE {user} WITH PASSWORD {password}"),
        _ => format!("ALTER USER {user} IDENTIFIED BY {password}"),
    })
}

/// The user a user statement names, or `None` when there is none to name:
/// a Postgres role is an identifier, a MySQL user is `'name'@'host'` (both
/// halves string literals, and the host required), and SQLite has no users.
fn user_target(name: &str, host: Option<&str>, engine: Engine) -> Option<String> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    match engine {
        Engine::Postgres => Some(quote_identifier(name, engine)),
        Engine::MySql => {
            let host = host.map(str::trim).filter(|host| !host.is_empty())?;
            Some(format!(
                "{}@{}",
                quote_literal_for(engine, name),
                quote_literal_for(engine, host)
            ))
        }
        Engine::Sqlite => None,
    }
}

/// `schema.name` with both halves quoted, or just the name when there is no
/// schema to qualify it with — how every generated statement names a table.
pub(crate) fn qualified_name(schema: Option<&str>, name: &str, engine: Engine) -> String {
    match schema {
        Some(schema) => format!(
            "{}.{}",
            quote_identifier(schema, engine),
            quote_identifier(name, engine)
        ),
        None => quote_identifier(name, engine),
    }
}

/// Quoted identifiers joined by commas, as a column list is written.
pub(crate) fn quoted_list(names: &[String], engine: Engine) -> String {
    names
        .iter()
        .map(|name| quote_identifier(name, engine))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Quote `value` as a SQL string literal.
///
/// Used only for the metadata queries that cannot take a bind parameter (a
/// `PRAGMA` table function, for one); user data goes through
/// [`Connection::execute`](super::Connection::execute).
pub(crate) fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Quote `value` as a SQL string literal for `engine`.
///
/// MySQL reads a backslash inside a string as the start of an escape sequence,
/// so a value holding one — a Windows path, a JSON document with `\n` in it —
/// has to have it doubled or the server reads a different value back. The other
/// two engines take a backslash literally.
pub(crate) fn quote_literal_for(engine: Engine, value: &str) -> String {
    let escaped = match engine {
        Engine::MySql => value.replace('\\', "\\\\").replace('\'', "''"),
        Engine::Postgres | Engine::Sqlite => value.replace('\'', "''"),
    };
    format!("'{escaped}'")
}

/// The placeholder for the `index`-th bind parameter, counting from one.
pub(crate) fn placeholder(engine: Engine, index: usize) -> String {
    match engine {
        Engine::Postgres => format!("${index}"),
        Engine::MySql | Engine::Sqlite => "?".to_string(),
    }
}

/// A placeholder that will be accepted where a `type_name` value belongs.
///
/// Every parameter is bound as text. MySQL and SQLite coerce that to the
/// column's type on their own; Postgres refuses it outright, so its
/// placeholder is cast. Type names come back from the driver (`INT4`,
/// `TIMESTAMPTZ`, `INT4[]`), and all of them are castable as written — bar a
/// user-defined type whose name is not lower case, which Postgres down-cases
/// and then fails to find.
pub(crate) fn typed_placeholder(engine: Engine, index: usize, type_name: &str) -> String {
    let placeholder = placeholder(engine, index);
    match engine {
        // A `BIT` shows as its digits, which MySQL would otherwise store as
        // the bytes of the digits themselves.
        Engine::MySql if type_name.eq_ignore_ascii_case("BIT") => {
            format!("cast(conv({placeholder}, 2, 10) as unsigned)")
        }
        Engine::Postgres if !type_name.is_empty() => {
            format!("cast({placeholder} as {})", cast_target(type_name))
        }
        _ => placeholder,
    }
}

/// The type a Postgres parameter is cast to on its way into a `type_name`
/// column.
///
/// Usually the column's own type, but a driver type name carries no length:
/// `CHAR` alone means `character(1)` and `BIT` alone means `bit(1)`, so casting
/// to either would quietly cut the value down to one character or one bit.
/// Both have a width-free relative that the column accepts on assignment and
/// compares against, so the cast goes through that instead and the column
/// itself decides the width.
fn cast_target(type_name: &str) -> &str {
    match type_name.to_ascii_uppercase().as_str() {
        "CHAR" | "BPCHAR" => "text",
        "BIT" | "VARBIT" => "varbit",
        _ => type_name,
    }
}

/// The type a value is cast to when SQL needs it as text, per engine.
pub(crate) fn text_type(engine: Engine) -> &'static str {
    match engine {
        // MySQL has no `text` in a cast; `char` is its stand-in for one.
        Engine::MySql => "char",
        Engine::Postgres | Engine::Sqlite => "text",
    }
}

/// Keyword calls a cell accepts as a literal value, the way it already
/// accepts `NULL`: recognized up to case, all three engines evaluate every
/// one of them the same way.
const KEYWORD_LITERALS: &[&str] = &["NOW()", "CURRENT_TIMESTAMP", "CURRENT_DATE", "CURRENT_TIME"];

/// `value`, if it is one of [`KEYWORD_LITERALS`] up to case — the canonical
/// spelling to paste into the statement as written, rather than bind as a
/// parameter.
///
/// A bound parameter is always text, so a driver would quote `NOW()` and the
/// server would either store the four characters or refuse them outright,
/// rather than evaluate the call.
pub(crate) fn keyword_literal(value: &str) -> Option<&'static str> {
    let trimmed = value.trim();
    KEYWORD_LITERALS
        .iter()
        .find(|keyword| keyword.eq_ignore_ascii_case(trimmed))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserved_list_is_sorted_for_its_search() {
        assert!(RESERVED.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn a_reserved_word_is_quoted_even_in_lower_case() {
        assert_eq!(quote_identifier("order", Engine::Postgres), "\"order\"");
        assert_eq!(quote_identifier("user", Engine::Postgres), "\"user\"");
        assert_eq!(quote_identifier("key", Engine::MySql), "`key`");
        assert_eq!(quote_identifier("rank", Engine::MySql), "`rank`");
        assert_eq!(quote_identifier("ordered", Engine::Postgres), "ordered");
        assert_eq!(quote_identifier("user_id", Engine::Sqlite), "user_id");
    }

    #[test]
    fn create_database_quotes_the_name_per_engine() {
        assert_eq!(
            create_database_sql("analytics", Engine::Postgres),
            Some("CREATE DATABASE analytics".to_string())
        );
        assert_eq!(
            create_database_sql("analytics", Engine::MySql),
            Some("CREATE DATABASE analytics".to_string())
        );
        assert_eq!(
            create_database_sql("my database", Engine::Postgres),
            Some("CREATE DATABASE \"my database\"".to_string())
        );
        assert_eq!(
            create_database_sql("my database", Engine::MySql),
            Some("CREATE DATABASE `my database`".to_string())
        );
        assert_eq!(
            create_database_sql("we\"ird", Engine::Postgres),
            Some("CREATE DATABASE \"we\"\"ird\"".to_string())
        );
        assert_eq!(
            create_database_sql("we`ird", Engine::MySql),
            Some("CREATE DATABASE `we``ird`".to_string())
        );
    }

    #[test]
    fn create_database_refuses_empty_names_and_sqlite() {
        assert_eq!(create_database_sql("", Engine::Postgres), None);
        assert_eq!(create_database_sql("   ", Engine::MySql), None);
        assert_eq!(create_database_sql("analytics", Engine::Sqlite), None);
    }

    #[test]
    fn user_statements_cover_both_engines() {
        assert_eq!(
            create_user_sql("app", None, "s3cret", Engine::Postgres),
            Some("CREATE ROLE app WITH LOGIN PASSWORD 's3cret'".to_string())
        );
        assert_eq!(
            create_user_sql("app", Some("%"), "s3cret", Engine::MySql),
            Some("CREATE USER 'app'@'%' IDENTIFIED BY 's3cret'".to_string())
        );
        assert_eq!(
            drop_user_sql("app", None, Engine::Postgres),
            Some("DROP ROLE app".to_string())
        );
        assert_eq!(
            drop_user_sql("app", Some("localhost"), Engine::MySql),
            Some("DROP USER 'app'@'localhost'".to_string())
        );
        assert_eq!(
            set_user_password_sql("app", None, "n3w", Engine::Postgres),
            Some("ALTER ROLE app WITH PASSWORD 'n3w'".to_string())
        );
        assert_eq!(
            set_user_password_sql("app", Some("%"), "n3w", Engine::MySql),
            Some("ALTER USER 'app'@'%' IDENTIFIED BY 'n3w'".to_string())
        );
    }

    #[test]
    fn user_statements_quote_names_and_passwords() {
        assert_eq!(
            create_user_sql("my app", None, "it's", Engine::Postgres),
            Some("CREATE ROLE \"my app\" WITH LOGIN PASSWORD 'it''s'".to_string())
        );
        assert_eq!(
            create_user_sql("o'brien", Some("%"), "a\\b", Engine::MySql),
            Some("CREATE USER 'o''brien'@'%' IDENTIFIED BY 'a\\\\b'".to_string())
        );
    }

    #[test]
    fn user_statements_refuse_empty_names_missing_hosts_and_sqlite() {
        assert_eq!(create_user_sql("", None, "pw", Engine::Postgres), None);
        assert_eq!(create_user_sql("app", None, "pw", Engine::MySql), None);
        assert_eq!(create_user_sql("app", Some(""), "pw", Engine::MySql), None);
        assert_eq!(
            create_user_sql("app", Some("%"), "pw", Engine::Sqlite),
            None
        );
        assert_eq!(drop_user_sql("", None, Engine::Postgres), None);
        assert_eq!(
            set_user_password_sql("app", None, "pw", Engine::Sqlite),
            None
        );
    }
}
