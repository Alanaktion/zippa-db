//! Agent reads.

use super::*;

#[tokio::test]
async fn an_agent_read_runs_in_a_session_the_server_holds_read_only() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let read = connection
        .run_query_as_agent("SELECT name FROM items WHERE id = 1", 100)
        .await
        .expect("a plain read should run");
    assert_eq!(read.rows[0][0].as_deref(), Some("alpha"));

    // The connection itself allows writes; only the agent's session does not,
    // which is what stops a write hidden where the classifier cannot see it.
    let refused = connection
        .run_query_as_agent("INSERT INTO items (id, name) VALUES (9, 'agent')", 100)
        .await;
    assert!(
        refused.is_err(),
        "the agent's session should refuse a write"
    );
    let count = connection
        .run_query("SELECT count(*) FROM items WHERE id = 9")
        .await
        .expect("count failed");
    assert_eq!(count.rows[0][0].as_deref(), Some("0"));

    // And the pool is left as it was: `query_only` does not leak onto it.
    connection
        .execute("INSERT INTO items (id, name) VALUES (9, 'user')", vec![])
        .await
        .expect("the user's own write should still run");

    connection.close().await;
}

#[tokio::test]
async fn an_agent_write_leaves_no_session_state_on_the_pool() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    // sqlx resets nothing when a connection goes back to its pool. A
    // temporary table lives on the connection that made it and shadows the
    // real one, so had the write run on a pooled connection, one of the
    // reads below would have landed there and counted none.
    connection
        .execute_as_agent("CREATE TEMP TABLE items (id INTEGER)")
        .await
        .expect("the agent's write should run");

    // Reads from the pool rotate through its idle connections, so twice the
    // pool's size reaches every one of them.
    for _ in 0..2 * crate::db::connection::POOL_MAX {
        let count = connection
            .run_query("SELECT count(*) FROM items")
            .await
            .expect("count failed");
        assert_eq!(
            count.rows[0][0].as_deref(),
            Some("2"),
            "the agent's temporary table reached a pooled connection"
        );
    }

    // A write still reports what it changed.
    let affected = connection
        .execute_as_agent("UPDATE items SET name = 'beta' WHERE id = 2")
        .await
        .expect("the agent's update should run");
    assert_eq!(affected, 1);

    connection.close().await;
}
