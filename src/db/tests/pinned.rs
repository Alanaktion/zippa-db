//! A query tab's pinned connection and its transaction.

use super::*;

/// A transaction opened in one run is still open in the next, on a query
/// tab's pinned connection, and only that tab sees what it has not committed.
#[tokio::test]
async fn a_pinned_connection_keeps_a_transaction_between_runs() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;
    let pinned = PinnedConnection::new(connection.clone());
    assert_eq!(pinned.state(), TxnState::Idle, "nothing is checked out yet");

    pinned.run_query_with("BEGIN", &[]).await.expect("begin");
    assert_eq!(pinned.state(), TxnState::Open);
    pinned
        .run_query_with("INSERT INTO items VALUES (3, 'gamma', 3.0, NULL)", &[])
        .await
        .expect("insert");
    assert_eq!(pinned.state(), TxnState::Open);
    let inside = pinned
        .run_query_with("SELECT COUNT(*) FROM items", &[])
        .await
        .expect("count");
    assert_eq!(
        inside.rows[0][0].as_deref(),
        Some("3"),
        "the tab sees its own insert"
    );

    pinned
        .run_query_with("ROLLBACK", &[])
        .await
        .expect("rollback");
    assert_eq!(pinned.state(), TxnState::Idle);
    assert_eq!(
        item_count(&connection).await,
        "2",
        "the insert was rolled back"
    );

    // A checked-out connection holds the pool's close open until it goes.
    drop(pinned);
    connection.close().await;
}

#[tokio::test]
async fn a_pinned_connection_keeps_session_state_between_runs() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;
    let pinned = PinnedConnection::new(connection.clone());

    pinned
        .run_query_with("CREATE TEMP TABLE scratch (a)", &[])
        .await
        .expect("create temp");
    pinned
        .run_query_with("INSERT INTO scratch VALUES (1)", &[])
        .await
        .expect("insert temp");
    pinned
        .run_query_with("PRAGMA foreign_keys = ON", &[])
        .await
        .expect("pragma");

    let rows = pinned
        .run_query_with("SELECT a FROM scratch", &[])
        .await
        .expect("read temp");
    assert_eq!(rows.rows, [[Some("1".to_string())]]);
    let keys = pinned
        .run_query_with("PRAGMA foreign_keys", &[])
        .await
        .expect("read pragma");
    assert_eq!(keys.rows[0][0].as_deref(), Some("1"));

    // The pool's connections never saw the temporary table.
    assert!(connection.run_query("SELECT a FROM scratch").await.is_err());

    // A checked-out connection holds the pool's close open until it goes.
    drop(pinned);
    connection.close().await;
}

#[tokio::test]
async fn a_committed_transaction_is_visible_to_the_pool() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;
    let pinned = PinnedConnection::new(connection.clone());

    pinned.run_query_with("BEGIN", &[]).await.expect("begin");
    pinned
        .run_query_with("DELETE FROM items WHERE id = 2", &[])
        .await
        .expect("delete");
    pinned.run_query_with("COMMIT", &[]).await.expect("commit");
    assert_eq!(pinned.state(), TxnState::Idle);
    assert_eq!(item_count(&connection).await, "1");

    // A checked-out connection holds the pool's close open until it goes.
    drop(pinned);
    connection.close().await;
}

#[tokio::test]
async fn a_read_only_pin_runs_transaction_control_but_refuses_writes() {
    let database = TempDatabase::new().await;
    let connection = Arc::new(
        Connection::open(
            ConnectionConfig {
                safety: SafetyMode::ReadOnly,
                ..database.config()
            },
            None,
        )
        .await
        .expect("could not open the test database"),
    );
    let pinned = PinnedConnection::new(connection.clone());

    pinned
        .run_query_with("BEGIN", &[])
        .await
        .expect("begin is not a write");
    assert_eq!(pinned.state(), TxnState::Open);
    let error = pinned
        .run_query_with("DELETE FROM items", &[])
        .await
        .expect_err("a read-only connection refuses a delete");
    assert!(format!("{error:#}").contains("DELETE"), "{error:#}");
    pinned
        .run_query_with("ROLLBACK", &[])
        .await
        .expect("rollback");
    assert_eq!(pinned.state(), TxnState::Idle);

    // A checked-out connection holds the pool's close open until it goes.
    drop(pinned);
    connection.close().await;
}

/// A script run inside a transaction the tab already has open is a savepoint
/// in it: rolling the script back leaves the tab's own work, and the tab's
/// transaction, as they were.
#[tokio::test]
async fn a_script_inside_an_open_transaction_rolls_back_only_itself() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;
    let pinned = PinnedConnection::new(connection.clone());

    pinned.run_query_with("BEGIN", &[]).await.expect("begin");
    pinned
        .run_query_with("INSERT INTO items VALUES (10, 'mine', 1.0, NULL)", &[])
        .await
        .expect("insert");

    let (run, _) = paused(
        ScriptRun::start(
            pinned.clone(),
            FAILS_IN_THE_MIDDLE,
            &[],
            ScriptMode::Transaction,
            OnFailure::Ask,
        )
        .await,
    );
    let outcome = finished(run.resume(Decision::Abort).await);
    assert!(outcome.rolled_back);
    assert_eq!(
        pinned.state(),
        TxnState::Open,
        "the tab's transaction is still open"
    );

    let count = pinned
        .run_query_with("SELECT COUNT(*) FROM items", &[])
        .await
        .expect("count");
    assert_eq!(
        count.rows[0][0].as_deref(),
        Some("3"),
        "only the tab's insert is left"
    );
    pinned.run_query_with("COMMIT", &[]).await.expect("commit");
    assert_eq!(item_count(&connection).await, "3");

    // A checked-out connection holds the pool's close open until it goes.
    drop(pinned);
    connection.close().await;
}

#[tokio::test]
async fn a_scripts_own_begin_leaves_the_tab_in_a_transaction() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;
    let pinned = PinnedConnection::new(connection.clone());

    finished(
        ScriptRun::start(
            pinned.clone(),
            "BEGIN;\nINSERT INTO items VALUES (3, 'gamma', 3.0, NULL);",
            &[],
            ScriptMode::Autocommit,
            OnFailure::Ask,
        )
        .await,
    );
    assert_eq!(pinned.state(), TxnState::Open);
    pinned
        .run_query_with("ROLLBACK", &[])
        .await
        .expect("rollback");
    assert_eq!(item_count(&connection).await, "2");

    // A checked-out connection holds the pool's close open until it goes.
    drop(pinned);
    connection.close().await;
}

/// Query tabs share a fixed number of connections of their own, so they can
/// never take the ones the sidebar and table views need; one past the cap is
/// told how to free one rather than left waiting.
#[tokio::test]
async fn pinned_connections_are_capped_per_connection() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;

    let mut pins = Vec::new();
    for _ in 0..crate::db::connection::PINNED_MAX {
        let pinned = PinnedConnection::new(connection.clone());
        pinned
            .run_query_with("SELECT 1", &[])
            .await
            .expect("within the cap");
        pins.push(pinned);
    }
    let one_more = PinnedConnection::new(connection.clone());
    let error = one_more
        .run_query_with("SELECT 1", &[])
        .await
        .expect_err("past the cap");
    assert!(
        format!("{error:#}").contains("too many query tabs"),
        "{error:#}"
    );
    // The pool still answers the app's own reads.
    assert_eq!(item_count(&connection).await, "2");

    // Letting one tab go frees its slot.
    drop(pins.pop());
    one_more
        .run_query_with("SELECT 1", &[])
        .await
        .expect("a slot was freed");

    drop((pins, one_more));
    connection.close().await;
}
