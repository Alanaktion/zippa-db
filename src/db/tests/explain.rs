//! Query plans.

use super::*;

#[tokio::test]
async fn explain_reads_a_sqlite_plan_tree() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::default()).await;

    let explained = connection
        .explain_with("select * from items where id = 1", Vec::new(), false)
        .await
        .expect("a select should explain");
    let crate::db::plan::Explained::Plan(plan) = explained else {
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
            .explain_with(sql, Vec::new(), true)
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
        .explain_with("select * from items", Vec::new(), true)
        .await
        .expect_err("SQLite has no EXPLAIN ANALYZE");
    assert!(
        format!("{error:#}").contains("EXPLAIN ANALYZE"),
        "the refusal should name the missing form: {error:#}"
    );

    // The plain plan is still there under the other button.
    let explained = connection
        .explain_with("select * from items", Vec::new(), false)
        .await
        .expect("a plain explain should still be allowed");
    assert!(matches!(explained, crate::db::plan::Explained::Plan(_)));

    connection.close().await;
}

#[tokio::test]
async fn plain_explain_of_a_write_does_not_run_it() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::AutoApply).await;

    connection
        .explain_with("delete from items", Vec::new(), false)
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
        .explain_with("select * from items", Vec::new(), false)
        .await
        .expect("a read-only connection should explain a read");

    // The classifier calls any `ANALYZE` a write, so the pool is the guard
    // here; either way the connection must not run the write.
    let _ = connection
        .explain_with("delete from items", Vec::new(), true)
        .await;
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
        .explain_with("   ;  ", Vec::new(), false)
        .await
        .expect_err("an empty buffer has nothing to explain");
    assert!(format!("{error:#}").contains("Select one statement"));

    // A plan describes one statement, so a selection of several is refused
    // rather than explained as whatever the server does with a script.
    let error = connection
        .explain_with("select 1; select 2", Vec::new(), false)
        .await
        .expect_err("a script has no single plan");
    assert!(format!("{error:#}").contains("Select one statement"));

    connection.close().await;
}
