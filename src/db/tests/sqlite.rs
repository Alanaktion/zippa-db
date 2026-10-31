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

/// The snapshot a remote file's upload sends is the whole committed database,
/// in the journal mode the copy was in: a WAL file stays WAL on the host.
#[tokio::test]
async fn an_upload_snapshot_keeps_rows_and_wal_mode() {
    let database = TempDatabase::new().await;
    let pool = sqlx::SqlitePool::connect_with(
        SqliteConnectOptions::new()
            .filename(&database.path)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal),
    )
    .await
    .expect("open in WAL mode");
    pool.execute("INSERT INTO items (id, name) VALUES (9, 'in the wal')")
        .await
        .expect("write");

    let to = std::env::temp_dir().join(format!("zippa-db-snapshot-{}.sqlite", Uuid::new_v4()));
    crate::db::sqlite::snapshot(&pool, &to)
        .await
        .expect("snapshot");
    pool.close().await;

    let copy = sqlx::SqlitePool::connect_with(SqliteConnectOptions::new().filename(&to))
        .await
        .expect("open the snapshot");
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&copy)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    let name: String = sqlx::query_scalar("SELECT name FROM items WHERE id = 9")
        .fetch_one(&copy)
        .await
        .unwrap();
    assert_eq!(name, "in the wal");
    copy.close().await;
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", to.display()));
    }
}

/// A snapshot taken while another connection holds a write transaction open
/// has the committed rows and none of the pending ones, in either journal
/// mode: what an upload sends while a query tab has a transaction open.
#[tokio::test]
async fn an_upload_snapshot_leaves_out_an_open_transaction() {
    for wal in [false, true] {
        let database = TempDatabase::new().await;
        let options = SqliteConnectOptions::new().filename(&database.path);
        let options = if wal {
            options.journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        } else {
            options.journal_mode(sqlx::sqlite::SqliteJournalMode::Delete)
        };
        let pool = sqlx::pool::PoolOptions::<sqlx::Sqlite>::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .expect("open");
        let mut writer = pool.acquire().await.expect("writer");
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *writer)
            .await
            .unwrap();
        sqlx::query("INSERT INTO items (id, name) VALUES (10, 'pending')")
            .execute(&mut *writer)
            .await
            .unwrap();

        let to = std::env::temp_dir().join(format!("zippa-db-snapshot-{}.sqlite", Uuid::new_v4()));
        crate::db::sqlite::snapshot(&pool, &to)
            .await
            .expect("snapshot beside an open transaction");
        sqlx::query("ROLLBACK").execute(&mut *writer).await.unwrap();
        drop(writer);
        pool.close().await;

        let copy = sqlx::SqlitePool::connect_with(SqliteConnectOptions::new().filename(&to))
            .await
            .expect("open the snapshot");
        let ids: Vec<i64> = sqlx::query_scalar("SELECT id FROM items ORDER BY id")
            .fetch_all(&copy)
            .await
            .unwrap();
        assert_eq!(ids, [1, 2], "wal: {wal}");
        copy.close().await;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", to.display()));
        }
    }
}
