//! Scripts run a step at a time.

use super::*;

#[tokio::test]
async fn a_script_runs_every_statement_in_one_transaction() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;

    let outcome = finished(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            "INSERT INTO items VALUES (3, 'gamma', 3.0, NULL);\n\
             SELECT name FROM items WHERE id = 3;",
            &[],
            ScriptMode::Transaction,
            OnFailure::Ask,
        )
        .await,
    );
    assert_eq!(outcome.results.len(), 2, "one result per statement");
    assert_eq!(outcome.results[1].rows, [[Some("gamma".to_string())]]);
    assert!(outcome.failures.is_empty());
    assert_eq!(outcome.summary(), None, "a clean run has nothing to add");

    connection.close().await;
}

#[tokio::test]
async fn a_failure_pauses_the_script_and_rolling_back_undoes_it() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;

    let (run, failure) = paused(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            FAILS_IN_THE_MIDDLE,
            &[],
            ScriptMode::Transaction,
            OnFailure::Ask,
        )
        .await,
    );
    assert_eq!((failure.index, failure.total, failure.line), (1, 3, 2));
    assert!(
        failure.message.contains("not_a_table"),
        "the failure should carry the server's message: {}",
        failure.message
    );

    let outcome = finished(run.resume(Decision::Abort).await);
    assert!(outcome.rolled_back);
    assert_eq!(
        item_count(&connection).await,
        "2",
        "the insert before the failure should have been rolled back too"
    );

    connection.close().await;
}

#[tokio::test]
async fn skipping_a_failure_keeps_the_rest_of_the_transaction() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;

    let (run, _) = paused(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            FAILS_IN_THE_MIDDLE,
            &[],
            ScriptMode::Transaction,
            OnFailure::Ask,
        )
        .await,
    );
    let outcome = finished(run.resume(Decision::Skip).await);
    assert!(!outcome.rolled_back);
    assert_eq!(outcome.results.len(), 2);
    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(
        item_count(&connection).await,
        "4",
        "both inserts should have been committed"
    );

    connection.close().await;
}

#[tokio::test]
async fn skipping_every_error_stops_asking() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;

    let (run, _) = paused(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            "INSERT INTO not_a_table VALUES (1);\n\
             INSERT INTO items VALUES (3, 'gamma', 3.0, NULL);\n\
             INSERT INTO nor_this VALUES (1);",
            &[],
            ScriptMode::Transaction,
            OnFailure::Ask,
        )
        .await,
    );
    let outcome = finished(run.resume(Decision::SkipAll).await);
    assert_eq!(
        outcome
            .failures
            .iter()
            .map(|failure| failure.index)
            .collect::<Vec<_>>(),
        [0, 2]
    );
    assert_eq!(item_count(&connection).await, "3");

    let failures = outcome.failures_result().expect("the failures are listed");
    assert_eq!(failures.columns, ["#", "line", "statement", "error"]);
    assert_eq!(failures.rows.len(), 2);

    connection.close().await;
}

#[tokio::test]
async fn ignoring_errors_without_a_transaction_keeps_what_worked() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;

    let outcome = finished(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            FAILS_IN_THE_MIDDLE,
            &[],
            ScriptMode::Autocommit,
            OnFailure::Skip,
        )
        .await,
    );
    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(item_count(&connection).await, "4");

    connection.close().await;
}

#[tokio::test]
async fn stopping_without_a_transaction_keeps_what_ran_before() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;

    let (run, _) = paused(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            FAILS_IN_THE_MIDDLE,
            &[],
            ScriptMode::Autocommit,
            OnFailure::Ask,
        )
        .await,
    );
    let outcome = finished(run.resume(Decision::Abort).await);
    assert!(outcome.stopped && !outcome.rolled_back);
    assert_eq!(item_count(&connection).await, "3");

    connection.close().await;
}

#[tokio::test]
async fn closing_a_paused_script_rolls_its_transaction_back() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;

    let (run, _) = paused(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            FAILS_IN_THE_MIDDLE,
            &[],
            ScriptMode::Transaction,
            OnFailure::Ask,
        )
        .await,
    );
    run.close().await;
    assert_eq!(item_count(&connection).await, "2");

    connection.close().await;
}

#[tokio::test]
async fn a_scripts_statements_are_logged_one_by_one() {
    let database = TempDatabase::new().await;
    let connection = shared(&database).await;
    connection.query_log().clear();

    finished(
        ScriptRun::start(
            PinnedConnection::new(connection.clone()),
            FAILS_IN_THE_MIDDLE,
            &[],
            ScriptMode::Transaction,
            OnFailure::Skip,
        )
        .await,
    );
    let log = connection.query_log().snapshot();
    let sql: Vec<&str> = log.iter().map(|entry| entry.sql.as_str()).collect();
    assert_eq!(sql.len(), 3, "the savepoints are not the user's: {sql:?}");
    assert!(
        log.iter()
            .any(|entry| matches!(entry.outcome, QueryOutcome::Error(_))
                && entry.sql.contains("not_a_table")),
        "the failure is logged with its error"
    );
    assert!(log.iter().all(|entry| entry.source == QuerySource::User));

    connection.close().await;
}

#[tokio::test]
async fn a_read_only_connection_refuses_a_script_before_it_runs() {
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

    let error = match ScriptRun::start(
        PinnedConnection::new(connection.clone()),
        "SELECT 1; DELETE FROM items",
        &[],
        ScriptMode::Transaction,
        OnFailure::Ask,
    )
    .await
    {
        Ok(_) => panic!("a read-only connection should refuse the delete"),
        Err(error) => format!("{error:#}"),
    };
    assert!(
        error.contains("read-only") && error.contains("DELETE"),
        "{error}"
    );

    connection.close().await;
}
