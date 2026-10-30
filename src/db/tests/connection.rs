//! Opening a connection and the shared query path.

use super::*;

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
