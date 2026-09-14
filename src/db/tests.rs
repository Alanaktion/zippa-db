//! Driver tests that need no external server: SQLite covers the shared query
//! path (column names, value stringification, NULL handling).

use std::env;
use std::path::PathBuf;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Executor};
use uuid::Uuid;

use super::query::Cell;
use super::{
    Connection, ConnectionConfig, DatabaseObject, Engine, ObjectKind, RowKey, SafetyMode,
    quote_identifier, typed_placeholder,
};

pub(crate) struct TempDatabase {
    path: PathBuf,
}

impl TempDatabase {
    /// Create a SQLite file with one populated table.
    pub(crate) async fn new() -> Self {
        let path = env::temp_dir().join(format!("zippa-db-test-{}.sqlite", Uuid::new_v4()));
        let pool = SqlitePoolOptions::new()
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("could not create the test database");

        pool.execute(AssertSqlSafe(
            "CREATE TABLE items (
                id INTEGER PRIMARY KEY,
                name TEXT,
                score REAL,
                payload BLOB
            );
            INSERT INTO items VALUES (1, 'alpha', 1.5, x'001122');
            INSERT INTO items VALUES (2, NULL, NULL, NULL);
            CREATE VIEW named_items AS SELECT id, name FROM items WHERE name IS NOT NULL;"
                .to_string(),
        ))
        .await
        .expect("could not seed the test database");
        pool.close().await;

        Self { path }
    }

    pub(crate) fn config(&self) -> ConnectionConfig {
        ConnectionConfig {
            database: self.path.to_string_lossy().to_string(),
            ..ConnectionConfig::new(Engine::Sqlite)
        }
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[tokio::test]
async fn reads_columns_values_and_nulls() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let result = connection
        .run_query("SELECT id, name, score, payload FROM items ORDER BY id")
        .await
        .expect("query failed");

    assert_eq!(result.columns, ["id", "name", "score", "payload"]);
    assert_eq!(
        result.rows[0],
        [
            Some("1".to_string()),
            Some("alpha".to_string()),
            Some("1.5".to_string()),
            Some("<3 bytes>".to_string()),
        ]
    );
    // NULL is distinct from an empty string.
    assert_eq!(result.rows[1][1], None);
    assert_eq!(result.rows[1][2], None);
    assert_eq!(result.row_count(), 2);

    connection.close().await;
}

#[tokio::test]
async fn reports_errors_from_the_server() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let error = connection
        .run_query("SELECT * FROM missing_table")
        .await
        .expect_err("a missing table should fail");
    assert!(error.to_string().contains("missing_table"));

    connection.close().await;
}

#[tokio::test]
async fn missing_file_is_an_error() {
    let mut config = ConnectionConfig::new(Engine::Sqlite);
    config.database = env::temp_dir()
        .join(format!("zippa-db-missing-{}.sqlite", Uuid::new_v4()))
        .to_string_lossy()
        .to_string();

    let error = Connection::open(config, None)
        .await
        .expect_err("opening a missing file should fail");
    assert!(error.to_string().contains("could not open"));
}

#[tokio::test]
async fn lists_databases_and_objects() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    assert_eq!(
        connection
            .databases()
            .await
            .expect("could not list databases"),
        ["main"]
    );

    let objects = connection.objects().await.expect("could not list objects");
    let described: Vec<_> = objects
        .iter()
        .map(|object| (object.label(), object.kind))
        .collect();
    assert_eq!(
        described,
        [
            ("items".to_string(), ObjectKind::Table),
            ("named_items".to_string(), ObjectKind::View),
        ]
    );
    // SQLite has no schemas, so nothing is qualified.
    assert!(objects.iter().all(|object| object.schema.is_none()));

    connection.close().await;
}

#[tokio::test]
async fn switching_database_is_a_new_connection() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    // SQLite only ever has "main", but the reopen path is engine-agnostic.
    let switched = connection
        .with_database(connection.database())
        .await
        .expect("could not reopen the connection");
    assert_eq!(switched.database(), connection.database());

    switched.close().await;
    connection.close().await;
}

#[test]
fn identifiers_are_quoted_only_when_they_have_to_be() {
    assert_eq!(quote_identifier("users", Engine::Postgres), "users");
    assert_eq!(quote_identifier("user_roles", Engine::MySql), "user_roles");

    // Upper case, spaces, and leading digits all need quoting.
    assert_eq!(quote_identifier("Users", Engine::Postgres), "\"Users\"");
    assert_eq!(
        quote_identifier("order items", Engine::MySql),
        "`order items`"
    );
    assert_eq!(quote_identifier("2fa", Engine::Sqlite), "\"2fa\"");

    // Embedded quotes are doubled rather than escaped.
    assert_eq!(
        quote_identifier("we\"ird", Engine::Postgres),
        "\"we\"\"ird\""
    );
    assert_eq!(quote_identifier("we`ird", Engine::MySql), "`we``ird`");
}

/// The object the sidebar would hand a table view for `name`.
fn table(name: &str) -> DatabaseObject {
    DatabaseObject {
        schema: None,
        name: name.to_string(),
        kind: ObjectKind::Table,
    }
}

#[tokio::test]
async fn column_types_come_back_with_the_columns() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let result = connection
        .run_query("SELECT * FROM items ORDER BY id")
        .await
        .expect("the query failed");
    assert_eq!(result.column_types, ["INTEGER", "TEXT", "REAL", "BLOB"]);

    connection.close().await;
}

#[tokio::test]
async fn primary_key_columns_are_found() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    assert_eq!(
        connection.row_key(&table("items")).await.expect("no key"),
        RowKey::Columns(vec!["id".to_string()])
    );

    connection.close().await;
}

#[tokio::test]
async fn a_view_cannot_be_edited() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let view = DatabaseObject {
        kind: ObjectKind::View,
        ..table("named_items")
    };
    assert!(matches!(
        connection.row_key(&view).await.expect("no key"),
        RowKey::Unavailable(_)
    ));

    connection.close().await;
}

#[tokio::test]
async fn a_table_without_a_primary_key_falls_back_to_rowid() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    connection
        .execute("CREATE TABLE notes (body TEXT)", Vec::new())
        .await
        .expect("could not create the table");

    assert_eq!(
        connection.row_key(&table("notes")).await.expect("no key"),
        RowKey::RowId("rowid")
    );

    connection.close().await;
}

#[tokio::test]
async fn execute_binds_parameters_and_reports_rows_affected() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let affected = connection
        .execute(
            "UPDATE items SET name = ? WHERE id = ?",
            vec![Some("renamed".to_string()), Some("1".to_string())],
        )
        .await
        .expect("the update failed");
    assert_eq!(affected, 1);

    let result = connection
        .run_query("SELECT name FROM items WHERE id = 1")
        .await
        .expect("the query failed");
    assert_eq!(result.rows, [[Some("renamed".to_string())]]);

    connection.close().await;
}

#[tokio::test]
async fn execute_writes_sql_null() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let affected = connection
        .execute(
            "UPDATE items SET name = ? WHERE id = ?",
            vec![None, Some("1".to_string())],
        )
        .await
        .expect("the update failed");
    assert_eq!(affected, 1);

    let result = connection
        .run_query("SELECT name FROM items WHERE id = 1")
        .await
        .expect("the query failed");
    let expected: Vec<Vec<Cell>> = vec![vec![None]];
    assert_eq!(result.rows, expected);

    connection.close().await;
}

#[tokio::test]
async fn execute_reports_no_rows_for_a_missing_key() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let affected = connection
        .execute(
            "UPDATE items SET name = ? WHERE id = ?",
            vec![Some("nobody".to_string()), Some("999".to_string())],
        )
        .await
        .expect("the update failed");
    assert_eq!(affected, 0);

    connection.close().await;
}

#[test]
fn a_connection_saved_before_safety_modes_reads_back_as_staged() {
    // The field is written by every save now, but files from before it
    // existed have to keep working, and they get the careful mode.
    let saved = r#"{
        "id": "00000000-0000-0000-0000-000000000001",
        "name": "old",
        "engine": "Postgres",
        "host": "localhost",
        "port": 5432,
        "username": "postgres",
        "database": "app"
    }"#;

    let config: ConnectionConfig =
        serde_json::from_str(saved).expect("an older connection should still load");
    assert_eq!(config.safety, SafetyMode::Staged);

    let written = serde_json::to_string(&ConnectionConfig {
        safety: SafetyMode::AutoApply,
        ..ConnectionConfig::new(Engine::Postgres)
    })
    .expect("the config should serialize");
    assert!(
        written.contains("\"safety\":\"AutoApply\""),
        "the mode belongs in the file: {written}"
    );
}

#[test]
fn only_postgres_casts_its_placeholders() {
    assert_eq!(
        typed_placeholder(Engine::Postgres, 2, "INT4"),
        "cast($2 as INT4)"
    );
    assert_eq!(typed_placeholder(Engine::MySql, 2, "INT"), "?");
    assert_eq!(typed_placeholder(Engine::Sqlite, 2, "INTEGER"), "?");
    // A column the driver could not name is left uncast rather than guessed at.
    assert_eq!(typed_placeholder(Engine::Postgres, 1, ""), "$1");
}

/// The seeded database, opened in `safety` mode.
async fn open_with(database: &TempDatabase, safety: SafetyMode) -> Connection {
    let config = ConnectionConfig {
        safety,
        ..database.config()
    };
    Connection::open(config, None)
        .await
        .expect("could not open the test database")
}

#[tokio::test]
async fn a_read_only_connection_refuses_writes_and_names_them() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::ReadOnly).await;

    let error = connection
        .run_query("update items set name = 'x' where id = 1")
        .await
        .expect_err("a read-only connection should refuse an UPDATE");
    let error = format!("{error:#}");
    assert!(
        error.contains("read-only") && error.contains("UPDATE"),
        "the error should say what was refused: {error}"
    );

    connection
        .execute("update items set name = ? where id = ?", vec![None, None])
        .await
        .expect_err("a read-only connection should refuse an inline edit too");

    // Reading is what the mode is for, and it still works.
    let result = connection
        .run_query("select name from items where id = 1")
        .await
        .expect("a read-only connection should still read");
    assert_eq!(result.rows, [[Some("alpha".to_string())]]);

    connection.close().await;
}

#[tokio::test]
async fn a_read_only_pool_refuses_a_write_the_client_did_not_catch() {
    // The classifier only speaks for statements it recognises, so the file is
    // opened read-only as well. This is that half.
    let database = TempDatabase::new().await;
    let config = ConnectionConfig {
        safety: SafetyMode::ReadOnly,
        ..database.config()
    };
    let pool = super::sqlite::connect(&config)
        .await
        .expect("could not open the test database");

    let error = sqlx::query("insert into items values (9, 'nine', 9.0, NULL)")
        .execute(&pool)
        .await
        .expect_err("the file should be open read-only");
    pool.close().await;

    let error = format!("{error}").to_ascii_lowercase();
    assert!(
        error.contains("readonly") || error.contains("read-only"),
        "the server should be the one refusing here: {error}"
    );
}

#[tokio::test]
async fn the_other_modes_still_write() {
    let database = TempDatabase::new().await;
    for safety in [
        SafetyMode::ConfirmWrites,
        SafetyMode::Staged,
        SafetyMode::AutoApply,
    ] {
        // Confirming is the UI's job; the connection itself writes for every
        // mode except read-only.
        let connection = open_with(&database, safety).await;
        let affected = connection
            .execute(
                "update items set name = ? where id = ?",
                vec![Some(format!("{safety:?}")), Some("1".to_string())],
            )
            .await
            .expect("the update failed");
        assert_eq!(affected, 1, "{safety:?} should write");
        connection.close().await;
    }
}

#[test]
fn a_width_free_cast_stands_in_for_one_that_would_truncate() {
    // `cast($1 as CHAR)` is `character(1)`, and `cast($1 as BIT)` is `bit(1)`,
    // so neither may be used for a column wider than that.
    assert_eq!(
        typed_placeholder(Engine::Postgres, 1, "CHAR"),
        "cast($1 as text)"
    );
    assert_eq!(
        typed_placeholder(Engine::Postgres, 2, "BIT"),
        "cast($2 as varbit)"
    );
    assert_eq!(
        typed_placeholder(Engine::Postgres, 3, "VARBIT"),
        "cast($3 as varbit)"
    );
    // Everything else is cast to the column's own type.
    assert_eq!(
        typed_placeholder(Engine::Postgres, 1, "TIMESTAMPTZ"),
        "cast($1 as TIMESTAMPTZ)"
    );
    assert_eq!(
        typed_placeholder(Engine::Postgres, 1, "INT4[]"),
        "cast($1 as INT4[])"
    );
    assert_eq!(typed_placeholder(Engine::Postgres, 1, ""), "$1");
}

#[test]
fn mysql_bit_digits_are_converted_rather_than_stored_as_text() {
    assert_eq!(
        typed_placeholder(Engine::MySql, 1, "BIT"),
        "cast(conv(?, 2, 10) as unsigned)"
    );
    assert_eq!(typed_placeholder(Engine::MySql, 1, "DATETIME"), "?");
    assert_eq!(typed_placeholder(Engine::Sqlite, 1, "DATETIME"), "?");
}
