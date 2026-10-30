//! Read-only enforcement and the other safety modes.

use super::*;

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
    let pool = crate::db::sqlite::connect(&config)
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
async fn a_read_only_caller_is_refused_a_write_in_every_mode() {
    // A dashboard reads whatever the connection allows, so a widget that
    // writes never repeats its write on a refresh.
    let database = TempDatabase::new().await;
    for safety in [
        SafetyMode::ReadOnly,
        SafetyMode::ConfirmWrites,
        SafetyMode::Staged,
        SafetyMode::AutoApply,
    ] {
        let connection = open_with(&database, safety).await;
        let error = connection
            .run_read_with("delete from items", vec![])
            .await
            .expect_err("a read-only caller should refuse a DELETE");
        let error = format!("{error:#}");
        assert!(
            error.contains("only reads") && error.contains("DELETE"),
            "{safety:?}: the error should say what was refused: {error}"
        );
        let result = connection
            .run_read_with(
                "select name from items where id = ?",
                vec![Some("1".to_string())],
            )
            .await
            .expect("a read should still run");
        assert_eq!(result.rows, [[Some("alpha".to_string())]], "{safety:?}");
        assert_eq!(item_count(&connection).await, "2", "{safety:?}");
        connection.close().await;
    }
}
