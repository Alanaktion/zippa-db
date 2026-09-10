//! Driver tests that need no external server: SQLite covers the shared query
//! path (column names, value stringification, NULL handling).

use std::env;
use std::path::PathBuf;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Executor};
use uuid::Uuid;

use super::{Connection, ConnectionConfig, Engine, ObjectKind, quote_identifier};

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
