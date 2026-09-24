//! Driver tests that need no external server: SQLite covers the shared query
//! path (column names, value stringification, NULL handling).

use std::env;
use std::path::PathBuf;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Executor};
use uuid::Uuid;

use super::query::Cell;
use super::schema::ReferentialAction;
use super::{
    CatalogKind, Connection, ConnectionConfig, DatabaseObject, Engine, ObjectKind, RowKey,
    SafetyMode, TagColor, keyword_literal, quote_identifier, typed_placeholder,
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
async fn fetch_binary_reads_the_real_bytes_a_query_only_describes() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    // `run_query` only ever hands back `<3 bytes>`; `fetch_binary` is the
    // path that reads what those bytes actually are.
    let described = connection
        .run_query("SELECT payload FROM items WHERE id = 1")
        .await
        .expect("query failed");
    assert_eq!(described.rows[0][0], Some("<3 bytes>".to_string()));

    let bytes = connection
        .fetch_binary(
            "SELECT payload FROM items WHERE id = ?",
            vec![Some("1".to_string())],
        )
        .await
        .expect("fetch failed");
    assert_eq!(bytes, Some(vec![0x00, 0x11, 0x22]));

    // A NULL column and a row that matches nothing both come back empty
    // rather than as an error.
    let null = connection
        .fetch_binary(
            "SELECT payload FROM items WHERE id = ?",
            vec![Some("2".to_string())],
        )
        .await
        .expect("fetch failed");
    assert_eq!(null, None);

    let missing = connection
        .fetch_binary(
            "SELECT payload FROM items WHERE id = ?",
            vec![Some("99".to_string())],
        )
        .await
        .expect("fetch failed");
    assert_eq!(missing, None);

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
async fn catalog_reads_columns_indexes_and_triggers() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    connection
        .run_query("CREATE INDEX items_name_idx ON items(name)")
        .await
        .expect("could not create the index");
    connection
        .run_query("CREATE TRIGGER items_guard AFTER INSERT ON items BEGIN SELECT 1; END")
        .await
        .expect("could not create the trigger");

    let catalog = connection
        .catalog()
        .await
        .expect("could not read the catalog");
    let find = |kind, name: &str| {
        catalog
            .entries
            .iter()
            .find(|entry| entry.kind == kind && entry.name == name)
    };

    let id = find(CatalogKind::Column, "id").expect("the id column is missing");
    assert_eq!(id.detail, "INTEGER");
    assert_eq!(id.owner().map(|object| object.name.as_str()), Some("items"));

    let index = find(CatalogKind::Index, "items_name_idx").expect("the index is missing");
    assert_eq!(index.detail, "name");
    assert_eq!(
        index.owner().map(|object| object.name.as_str()),
        Some("items")
    );

    let trigger = find(CatalogKind::Trigger, "items_guard").expect("the trigger is missing");
    assert_eq!(
        trigger.owner().map(|object| object.name.as_str()),
        Some("items")
    );

    // Views bring their columns; SQLite's own tables are left out.
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.kind == CatalogKind::View && entry.name == "named_items")
    );
    assert!(
        !catalog
            .entries
            .iter()
            .any(|entry| entry.name.starts_with("sqlite_"))
    );
    assert_eq!(
        catalog
            .entries
            .iter()
            .filter(|entry| entry.kind == CatalogKind::Table)
            .count(),
        1
    );
    assert_eq!(catalog.truncated(), None);

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

#[tokio::test]
async fn a_script_runs_every_statement_in_one_transaction() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let results = connection
        .run_script(
            "INSERT INTO items VALUES (3, 'gamma', 3.0, NULL);\n\
             SELECT name FROM items WHERE id = 3;",
        )
        .await
        .expect("the script failed");
    assert_eq!(results.len(), 2, "one result per statement");
    assert_eq!(results[1].rows, [[Some("gamma".to_string())]]);

    connection.close().await;
}

#[tokio::test]
async fn a_failing_statement_rolls_the_whole_script_back() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let error = connection
        .run_script(
            "INSERT INTO items VALUES (3, 'gamma', 3.0, NULL);\n\
             INSERT INTO not_a_table VALUES (1);",
        )
        .await
        .expect_err("the second statement should fail");
    assert!(
        format!("{error:#}").contains("statement 2"),
        "the error should name which statement failed: {error:#}"
    );

    let result = connection
        .run_query("SELECT COUNT(*) FROM items")
        .await
        .expect("the query failed");
    assert_eq!(
        result.rows,
        [[Some("2".to_string())]],
        "the insert before the failure should have been rolled back too"
    );

    connection.close().await;
}

#[tokio::test]
async fn a_failing_schema_statement_rolls_the_whole_change_back() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let error = connection
        .execute_script(&[
            "ALTER TABLE items ADD COLUMN note TEXT".to_string(),
            "ALTER TABLE not_a_table ADD COLUMN note TEXT".to_string(),
        ])
        .await
        .expect_err("the second statement should fail");
    assert!(
        format!("{error:#}").contains("statement 2"),
        "the error should name which statement failed: {error:#}"
    );

    let result = connection
        .run_query("SELECT name FROM pragma_table_info('items') WHERE name = 'note'")
        .await
        .expect("the query failed");
    assert!(
        result.rows.is_empty(),
        "the column added before the failure should have been rolled back too"
    );

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
fn a_connection_saved_before_tagging_reads_back_untagged() {
    // No tag, colour or last_connected in a file written before tagging, so
    // all three read back as absent rather than failing to parse.
    let saved = r#"{
        "id": "00000000-0000-0000-0000-000000000001",
        "name": "old",
        "engine": "Postgres",
        "host": "localhost",
        "port": 5432,
        "username": "postgres",
        "database": "app",
        "safety": "Staged"
    }"#;

    let config: ConnectionConfig =
        serde_json::from_str(saved).expect("an older connection should still load");
    assert_eq!(config.tag, None);
    assert_eq!(config.color, None);
    assert_eq!(config.last_connected, None);
}

#[test]
fn a_tagged_connection_survives_a_round_trip_through_the_file() {
    let config = ConnectionConfig {
        name: "Prod DB".into(),
        tag: Some("Production".into()),
        color: Some(TagColor::Red),
        last_connected: Some(
            "2024-01-02T03:04:05Z"
                .parse::<chrono::DateTime<chrono::Utc>>()
                .expect("a fixed timestamp"),
        ),
        ..ConnectionConfig::new(Engine::Postgres)
    };

    let written = serde_json::to_string(&config).expect("the config should serialize");
    let read: ConnectionConfig = serde_json::from_str(&written).expect("the config should parse");

    assert_eq!(read.tag.as_deref(), Some("Production"));
    assert_eq!(read.color, Some(TagColor::Red));
    assert_eq!(read.last_connected, config.last_connected);
}

#[test]
fn tag_color_keys_serialise_as_kebab_case() {
    for color in TagColor::ALL {
        let written = serde_json::to_string(&color).expect("the colour should serialize");
        assert_eq!(written, format!("\"{}\"", color.key()));
        let read: TagColor = serde_json::from_str(&written).expect("the colour should parse");
        assert_eq!(read, color);
    }
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

#[test]
fn keyword_literals_are_recognized_up_to_case_and_whitespace() {
    assert_eq!(keyword_literal("now()"), Some("NOW()"));
    assert_eq!(
        keyword_literal(" Current_Timestamp "),
        Some("CURRENT_TIMESTAMP")
    );
    assert_eq!(keyword_literal("current_date"), Some("CURRENT_DATE"));
    assert_eq!(keyword_literal("current_time"), Some("CURRENT_TIME"));
    // Text that merely contains a keyword is still just text.
    assert_eq!(keyword_literal("now"), None);
    assert_eq!(keyword_literal("it's now()"), None);
    assert_eq!(keyword_literal(""), None);
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

#[tokio::test]
async fn table_schema_reads_columns_and_marks_the_primary_key() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let schema = connection
        .table_schema(&table("items"))
        .await
        .expect("could not read the schema");

    let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "name", "score", "payload"]);

    // `id INTEGER PRIMARY KEY` is a `rowid` alias with no backing index, so
    // this only comes from the dedicated primary key read, not the indexes.
    assert!(schema.columns[0].is_primary_key);
    assert!(!schema.columns[1].is_primary_key);
    assert!(schema.indexes.is_empty());
    assert!(schema.foreign_keys.is_empty());

    connection.close().await;
}

#[tokio::test]
async fn table_schema_reads_a_unique_index_and_a_foreign_key() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    connection
        .execute(
            "CREATE TABLE tags (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
            Vec::new(),
        )
        .await
        .expect("could not create tags");
    connection
        .execute(
            "CREATE UNIQUE INDEX tags_label_idx ON tags(label)",
            Vec::new(),
        )
        .await
        .expect("could not create the index");
    connection
        .execute(
            "CREATE TABLE tagged_items ( \
                item_id INTEGER, \
                tag_id INTEGER, \
                FOREIGN KEY(item_id) REFERENCES items(id) ON DELETE CASCADE, \
                FOREIGN KEY(tag_id) REFERENCES tags(id) \
             )",
            Vec::new(),
        )
        .await
        .expect("could not create tagged_items");

    let tags = connection
        .table_schema(&table("tags"))
        .await
        .expect("could not read the tags schema");
    let index = tags
        .indexes
        .iter()
        .find(|index| index.name == "tags_label_idx")
        .expect("the unique index should be listed");
    assert!(index.unique);
    assert!(!index.is_primary_key);
    assert_eq!(index.columns, ["label"]);

    let tagged = connection
        .table_schema(&table("tagged_items"))
        .await
        .expect("could not read the tagged_items schema");
    assert_eq!(tagged.foreign_keys.len(), 2);

    let to_items = tagged
        .foreign_keys
        .iter()
        .find(|fk| fk.referenced_table == "items")
        .expect("the foreign key to items should be listed");
    assert_eq!(to_items.columns, ["item_id"]);
    assert_eq!(to_items.referenced_columns, ["id"]);
    assert_eq!(to_items.on_delete, ReferentialAction::Cascade);
    assert_eq!(to_items.on_update, ReferentialAction::NoAction);

    connection.close().await;
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

#[test]
fn mysql_metadata_escapes_a_backslash_the_way_the_server_reads_it() {
    // MySQL reads a backslash inside a string as an escape, so a table name
    // holding one has to be doubled or the `information_schema` query names a
    // different table. Every metadata builder that pastes a name must do it.
    let name = r"odd\name";
    let queries = [
        super::mysql::primary_key_sql(name),
        super::mysql::columns_sql(name),
        super::mysql::indexes_sql(name),
        super::mysql::foreign_keys_sql(name),
    ];
    for sql in queries {
        assert!(
            sql.contains(r"'odd\\name'"),
            "the name should be escaped for MySQL: {sql}"
        );
    }
}

#[tokio::test]
async fn explain_reads_a_sqlite_plan_tree() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::default()).await;

    let explained = connection
        .explain("select * from items where id = 1", false)
        .await
        .expect("a select should explain");
    let super::plan::Explained::Plan(plan) = explained else {
        panic!("SQLite should come back as a tree");
    };
    assert!(plan.parsed, "the rows are the shape SQLite documents");
    // The detail is the label, whether SQLite chose an index or a scan.
    let labels = labels(&plan.root);
    assert!(
        labels.iter().any(|label| label.contains("items")),
        "the plan should name the table: {labels:?}"
    );

    connection.close().await;
}

/// Every label in a tree, for tests that only care that one is there.
fn labels(node: &super::plan::PlanNode) -> Vec<String> {
    let mut all = vec![node.label.clone()];
    for child in &node.children {
        all.extend(labels(child));
    }
    all
}

#[tokio::test]
async fn explain_refuses_to_analyze_a_write() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::AutoApply).await;

    for sql in [
        "insert into items values (3, 'gamma', 3.0, NULL)",
        "with gone as (delete from items returning *) select * from gone",
        // The header says `ANALYZE`, so it runs even though the caller did not
        // ask for it.
        "explain analyze delete from items",
    ] {
        let error = connection
            .explain(sql, true)
            .await
            .expect_err("analyzing a write should be refused");
        assert!(
            format!("{error:#}").contains("changes data"),
            "{sql}: {error:#}"
        );
    }

    connection.close().await;
}

#[tokio::test]
async fn explain_refuses_to_analyze_a_read_on_sqlite() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::default()).await;

    // SQLite's planner reports no timings, so rather than answer a request for
    // actual times with the plain plan, it says it cannot.
    let error = connection
        .explain("select * from items", true)
        .await
        .expect_err("SQLite has no EXPLAIN ANALYZE");
    assert!(
        format!("{error:#}").contains("EXPLAIN ANALYZE"),
        "the refusal should name the missing form: {error:#}"
    );

    // The plain plan is still there under the other button.
    let explained = connection
        .explain("select * from items", false)
        .await
        .expect("a plain explain should still be allowed");
    assert!(matches!(explained, super::plan::Explained::Plan(_)));

    connection.close().await;
}

#[tokio::test]
async fn plain_explain_of_a_write_does_not_run_it() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::AutoApply).await;

    connection
        .explain("delete from items", false)
        .await
        .expect("explaining a write without ANALYZE is safe");

    let count = connection
        .run_query("select count(*) from items")
        .await
        .expect("could not count the rows");
    assert_eq!(
        count.rows[0][0].as_deref(),
        Some("2"),
        "nothing should be deleted"
    );

    connection.close().await;
}

#[tokio::test]
async fn a_read_only_connection_can_still_explain() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::ReadOnly).await;

    // A plain plan is a read, and the classifier already knows it.
    connection
        .explain("select * from items", false)
        .await
        .expect("a read-only connection should explain a read");

    // The classifier calls any `ANALYZE` a write, so the pool is the guard
    // here; either way the connection must not run the write.
    let _ = connection.explain("delete from items", true).await;
    let count = connection
        .run_query("select count(*) from items")
        .await
        .expect("could not count the rows");
    assert_eq!(count.rows[0][0].as_deref(), Some("2"));

    connection.close().await;
}

#[tokio::test]
async fn explain_needs_a_statement() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::default()).await;

    let error = connection
        .explain("   ;  ", false)
        .await
        .expect_err("an empty buffer has nothing to explain");
    assert!(format!("{error:#}").contains("Select one statement"));

    // A plan describes one statement, so a selection of several is refused
    // rather than explained as whatever the server does with a script.
    let error = connection
        .explain("select 1; select 2", false)
        .await
        .expect_err("a script has no single plan");
    assert!(format!("{error:#}").contains("Select one statement"));

    connection.close().await;
}

#[tokio::test]
async fn a_failed_table_rebuild_is_rolled_back() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::default()).await;

    // The first statement would have created the scratch table; the second
    // fails, so the whole rebuild has to come back off.
    let error = connection
        .rebuild_table(vec![
            "CREATE TABLE scratch (x INTEGER)".to_string(),
            "this is not sql".to_string(),
        ])
        .await
        .expect_err("a broken statement should fail the rebuild");
    assert!(format!("{error:#}").contains("statement 2 of 2"));

    let leftover = connection
        .run_query("select name from sqlite_master where name = 'scratch'")
        .await
        .expect("could not read the schema");
    assert!(leftover.rows.is_empty(), "the scratch table should be gone");

    connection.close().await;
}

#[tokio::test]
async fn a_read_only_connection_refuses_a_table_rebuild() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::ReadOnly).await;

    let error = connection
        .rebuild_table(vec!["CREATE TABLE scratch (x INTEGER)".to_string()])
        .await
        .expect_err("a read-only connection must not rebuild");
    assert!(format!("{error:#}").contains("read-only"));

    connection.close().await;
}

#[tokio::test]
async fn a_rebuild_leaves_the_data_and_the_schema_behind() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::default()).await;

    connection
        .rebuild_table(vec![
            "CREATE TABLE new_items (id INTEGER, name TEXT, score TEXT, payload BLOB)".to_string(),
            "INSERT INTO new_items (id, name, score, payload) \
             SELECT id, name, score, payload FROM items"
                .to_string(),
            "DROP TABLE items".to_string(),
            "ALTER TABLE new_items RENAME TO items".to_string(),
        ])
        .await
        .expect("a plain rebuild should run");

    let score = connection
        .run_query("select score from items where id = 1")
        .await
        .expect("could not read the rebuilt table");
    assert_eq!(score.rows[0][0].as_deref(), Some("1.5"));

    // The old table is gone and the new definition is what is left.
    let shape = connection
        .run_query("select name from pragma_table_info('items') order by cid")
        .await
        .expect("could not read the rebuilt columns");
    let columns: Vec<String> = shape
        .rows
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    assert_eq!(columns, ["id", "name", "score", "payload"]);

    connection.close().await;
}

/// A live MySQL server, for the metadata paths SQLite cannot exercise.
///
/// Ignored by default because it needs a server. Start one and run it:
///
/// ```text
/// docker run --rm -d -p 3307:3306 --name zippa-mysql \
///   -e MYSQL_ROOT_PASSWORD=secret -e MYSQL_DATABASE=app mysql:8.4
/// cargo test -- --ignored live_mysql
/// ```
///
/// `ZIPPA_TEST_MYSQL_URL` (default `mysql://root:secret@127.0.0.1:3307/app`)
/// points the test at another server.
#[tokio::test]
#[ignore = "needs a live MySQL server; see the doc comment"]
async fn live_mysql_routines_carry_their_argument_types() {
    let (connection, pool) = live_mysql().await;

    // `CREATE FUNCTION` is refused by the prepared-statement protocol, so the
    // fixture goes through the pool's text protocol instead.
    //
    // One of each shape `information_schema.parameters` spells: a function with
    // an argument, a function with none, and a procedure with two.
    for sql in [
        "DROP FUNCTION IF EXISTS zippa_add_one",
        "DROP FUNCTION IF EXISTS zippa_no_args",
        "DROP PROCEDURE IF EXISTS zippa_do_thing",
        "CREATE FUNCTION zippa_add_one(x INT) RETURNS INT DETERMINISTIC RETURN x + 1",
        "CREATE FUNCTION zippa_no_args() RETURNS INT DETERMINISTIC RETURN 42",
        "CREATE PROCEDURE zippa_do_thing(IN p_id INT, OUT p_name VARCHAR(50)) \
         SELECT p_id INTO p_name",
    ] {
        pool.execute(AssertSqlSafe(sql.to_string()))
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    let routines = connection
        .stored_objects()
        .await
        .expect("could not read the routines");
    let label = |name: &str| {
        routines
            .iter()
            .find(|routine| routine.name == name)
            .map(|routine| routine.label())
    };

    assert_eq!(
        label("zippa_add_one").as_deref(),
        Some("zippa_add_one(int)")
    );
    assert_eq!(label("zippa_no_args").as_deref(), Some("zippa_no_args()"));
    assert_eq!(
        label("zippa_do_thing").as_deref(),
        Some("zippa_do_thing(int, varchar(50))"),
        "a procedure's parameter types should tell it from another signature"
    );

    connection.close().await;
    pool.close().await;
}

/// A column edit on MySQL restates the whole column (`MODIFY`/`CHANGE
/// COLUMN`), so anything `table_schema` does not carry into
/// `ColumnDef::mysql_extra` would silently vanish from a real edit even
/// though the generator that reads it is only unit-tested against literal
/// `ColumnDef`s. This is the read half: that a live server's
/// `information_schema.columns` actually comes back shaped the way
/// `mysql::columns_sql`/`schema::parse_columns` assume.
#[tokio::test]
#[ignore = "needs a live MySQL server; see live_mysql_routines_carry_their_argument_types"]
async fn live_mysql_table_schema_carries_auto_increment_collation_comment_and_on_update() {
    let (connection, pool) = live_mysql().await;

    for sql in [
        "DROP TABLE IF EXISTS zippa_probe",
        "CREATE TABLE zippa_probe (
            id INT AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(50) COLLATE utf8mb4_bin COMMENT 'the display name',
            updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP \
                ON UPDATE CURRENT_TIMESTAMP,
            price DECIMAL(10,2),
            tax DECIMAL(10,2) GENERATED ALWAYS AS (price * 0.1) STORED,
            plain INT DEFAULT 5
        )",
    ] {
        pool.execute(AssertSqlSafe(sql.to_string()))
            .await
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }

    let object = DatabaseObject {
        schema: None,
        name: "zippa_probe".to_string(),
        kind: ObjectKind::Table,
    };
    let schema = connection
        .table_schema(&object)
        .await
        .expect("could not read the table schema");
    let column = |name: &str| {
        schema
            .columns
            .iter()
            .find(|column| column.name == name)
            .unwrap_or_else(|| panic!("no {name} column came back"))
    };

    assert!(column("id").mysql_extra.auto_increment);
    assert!(!column("plain").mysql_extra.auto_increment);

    assert!(column("updated_at").mysql_extra.on_update_current_timestamp);
    assert!(!column("plain").mysql_extra.on_update_current_timestamp);

    assert_eq!(
        column("name").mysql_extra.collation.as_deref(),
        Some("utf8mb4_bin")
    );
    assert_eq!(column("plain").mysql_extra.collation, None);

    assert_eq!(
        column("name").mysql_extra.comment.as_deref(),
        Some("the display name")
    );
    assert_eq!(
        column("plain").mysql_extra.comment,
        None,
        "an unset comment is an empty string on the server, folded to None"
    );

    assert_eq!(
        column("tax").mysql_extra.generation_expression.as_deref(),
        Some("(`price` * 0.1)")
    );
    assert_eq!(column("plain").mysql_extra.generation_expression, None);

    connection.close().await;
    pool.execute(AssertSqlSafe("DROP TABLE zippa_probe".to_string()))
        .await
        .expect("could not drop the probe table");
    pool.close().await;
}

/// `money` scales by `lc_monetary`'s fraction-digit count, which is two in
/// most locales but zero for a currency like the yen — so the raw integer a
/// `money` column decodes to means a different amount depending on the
/// server's locale, and a client that always divides by 100 would silently
/// show the wrong number for the others. This is the live half of
/// `postgres::money_keeps_both_digits_at_the_common_scale` and its sibling
/// scale tests: that `Connection::open`'s probe actually reads a real
/// server's scale rather than assuming it.
///
/// Ignored by default because it needs a server. Start one and run it:
///
/// ```text
/// docker run --rm -d -p 5433:5432 --name zippa-postgres \
///   -e POSTGRES_PASSWORD=secret -e POSTGRES_DB=app postgres:16
/// docker exec zippa-postgres psql -U postgres -c "CREATE DATABASE zippa_yen"
/// docker exec zippa-postgres psql -U postgres -d zippa_yen \
///   -c "ALTER DATABASE zippa_yen SET lc_monetary = 'ja_JP.utf8'"
/// cargo test -- --ignored live_postgres
/// ```
///
/// The yen locale needs generating first if the image does not already carry
/// it (`locale-gen ja_JP.UTF-8` inside the container, then a restart) —
/// confirmed against a plain `postgres:16` image, which does not.
///
/// `ZIPPA_TEST_POSTGRES_YEN_URL` (default
/// `postgres://postgres:secret@127.0.0.1:5433/zippa_yen`) points the test at
/// another server; `ZIPPA_TEST_POSTGRES_URL` (default
/// `postgres://postgres:secret@127.0.0.1:5433/app`) is the same server's
/// ordinary, two-digit database, for contrast.
#[tokio::test]
#[ignore = "needs a live Postgres server with a yen-locale database; see the doc comment"]
async fn live_postgres_money_scales_by_the_servers_locale_not_always_by_100() {
    let usual = live_postgres("ZIPPA_TEST_POSTGRES_URL", "app").await;
    let result = usual
        .run_query("SELECT '12.34'::money")
        .await
        .expect("could not read the ordinary-locale money value");
    assert_eq!(
        result.rows[0][0].as_deref(),
        Some("12.34"),
        "a two-digit locale should read back the way it was written"
    );
    usual.close().await;

    let yen = live_postgres("ZIPPA_TEST_POSTGRES_YEN_URL", "zippa_yen").await;
    let result = yen
        .run_query("SELECT '1234'::money")
        .await
        .expect("could not read the yen-locale money value");
    assert_eq!(
        result.rows[0][0].as_deref(),
        Some("1234"),
        "a zero-digit locale's value should not be shown divided by 100"
    );
    yen.close().await;
}

/// Open a Postgres connection for a live test, against the database named by
/// `var` (falling back to `127.0.0.1:5433`/`database` when unset).
async fn live_postgres(var: &str, database: &str) -> Connection {
    let url = env::var(var)
        .unwrap_or_else(|_| format!("postgres://postgres:secret@127.0.0.1:5433/{database}"));
    let rest = url.strip_prefix("postgres://").expect("a postgres:// URL");
    let (credentials, rest) = rest.split_once('@').expect("user:pass@host:port/db");
    let (username, password) = credentials.split_once(':').expect("user:pass");
    let (authority, database) = rest.split_once('/').expect("host:port/db");
    let (host, port) = authority.split_once(':').expect("host:port");
    let port: u16 = port.parse().expect("a port");

    let config = ConnectionConfig {
        host: host.to_string(),
        port,
        username: username.to_string(),
        database: database.to_string(),
        ..ConnectionConfig::new(Engine::Postgres)
    };
    Connection::open(config, Some(password.to_string()))
        .await
        .expect("could not open the live Postgres connection")
}

/// Open the MySQL server a live test runs against, and a pool onto the same
/// database for fixtures that need the text protocol.
async fn live_mysql() -> (Connection, sqlx::MySqlPool) {
    use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};

    let url = env::var("ZIPPA_TEST_MYSQL_URL")
        .unwrap_or_else(|_| "mysql://root:secret@127.0.0.1:3307/app".to_string());
    let rest = url.strip_prefix("mysql://").expect("a mysql:// URL");
    let (credentials, rest) = rest.split_once('@').expect("user:pass@host:port/db");
    let (username, password) = credentials.split_once(':').expect("user:pass");
    let (authority, database) = rest.split_once('/').expect("host:port/db");
    let (host, port) = authority.split_once(':').expect("host:port");
    let port: u16 = port.parse().expect("a port");

    let options = MySqlConnectOptions::new()
        .host(host)
        .port(port)
        .username(username)
        .password(password)
        .database(database);
    let pool = MySqlPoolOptions::new()
        .connect_with(options)
        .await
        .expect("could not open the live MySQL server");

    let config = ConnectionConfig {
        host: host.to_string(),
        port,
        username: username.to_string(),
        database: database.to_string(),
        ..ConnectionConfig::new(Engine::MySql)
    };
    let connection = Connection::open(config, Some(password.to_string()))
        .await
        .expect("could not open the live MySQL connection");

    (connection, pool)
}
