//! SQLite-only server tools and maintenance.

use super::*;

#[tokio::test]
async fn sqlite_has_no_process_list() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let error = connection
        .processes()
        .await
        .expect_err("SQLite has no server to list processes for");
    assert!(error.to_string().contains("no server processes"));

    let error = connection
        .kill_process("1")
        .await
        .expect_err("SQLite has no server process to end");
    assert!(error.to_string().contains("no server processes"));

    connection.close().await;
}

#[tokio::test]
async fn sqlite_has_no_server_variables() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let error = connection
        .server_variables()
        .await
        .expect_err("SQLite has no server-side configuration to list");
    assert!(error.to_string().contains("no server variables"));

    connection.close().await;
}

#[tokio::test]
async fn sqlite_has_no_query_digest() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let error = connection
        .query_digest()
        .await
        .expect_err("SQLite has no query instrumentation to read");
    assert!(error.to_string().contains("no query digest"));

    connection.close().await;
}

#[tokio::test]
async fn sqlite_connection_info_reports_version_file_and_size() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let info = connection
        .connection_info()
        .await
        .expect("SQLite can always report its own facts");
    assert_eq!(info.engine, Engine::Sqlite);
    assert_eq!(info.server, database.config().database);
    assert!(
        !info.version.is_empty(),
        "SQLite reports its library version"
    );
    assert!(
        info.size_bytes.is_some_and(|size| size > 0),
        "the test database file has a size"
    );
    assert_eq!(
        info.database, None,
        "SQLite's database is its file, not a separate name"
    );
    assert_eq!(info.username, None);
    assert_eq!(info.uptime_secs, None);
    assert_eq!(info.connections, None);

    connection.close().await;
}

#[tokio::test]
async fn sqlite_maintenance_runs_each_task() {
    use crate::db::Maintenance;

    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let check = connection
        .run_maintenance(Maintenance::IntegrityCheck)
        .await
        .expect("integrity check should run");
    assert_eq!(check.rows, vec![vec![Some("ok".to_string())]]);

    for task in Maintenance::ALL {
        connection
            .run_maintenance(task)
            .await
            .unwrap_or_else(|error| panic!("{task:?} failed: {error:#}"));
    }

    connection.close().await;
}

#[tokio::test]
async fn read_only_sqlite_refuses_the_maintenance_that_writes() {
    use crate::db::Maintenance;

    let database = TempDatabase::new().await;
    let mut config = database.config();
    config.safety = SafetyMode::ReadOnly;
    let connection = Connection::open(config, None)
        .await
        .expect("could not open the test database");

    for task in Maintenance::ALL {
        let outcome = connection.run_maintenance(task).await;
        if task.writes() {
            let error = outcome.expect_err("a write should be refused");
            assert!(error.to_string().contains("read-only"), "{task:?}");
        } else {
            outcome.unwrap_or_else(|error| panic!("{task:?} is a read: {error:#}"));
        }
    }

    connection.close().await;
}
