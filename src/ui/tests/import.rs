//! The Import SQL Dump dialog.
//!
//! The runner itself is covered against a real database in `db::import`; these
//! tests are about the dialog: what it blocks, what it reports, and what the
//! session does when a run finishes.

use super::*;

use crate::db::OnError;
use crate::ui::import_dialog::ImportView;

/// Open the import dialog on `path`, the way the sidebar button does.
fn open_import(
    cx: &mut TestAppContext,
    handle: &WorkspaceWindow,
    session: &Entity<Session>,
    path: std::path::PathBuf,
) {
    let session = session.clone();
    cx.update_window(handle.window.into(), |_, window, cx| {
        session.update(cx, |session, cx| {
            session.open_import_for_test(path, window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
}

/// The import dialog the session is showing.
fn import_view(cx: &mut TestAppContext, session: &Entity<Session>) -> Entity<ImportView> {
    session
        .read_with(cx, |session, _| session.import_view_for_test())
        .expect("the import dialog should be open")
}

/// Start the run and let it finish, which under the test runtime is inline.
fn start(cx: &mut TestAppContext, view: &Entity<ImportView>) {
    view.update(cx, |view, cx| view.start_for_test(cx));
    cx.run_until_parked();
}

/// Count a table through its own connection; a table that does not exist
/// counts as zero, which is what a rollback leaves behind.
fn count_table(database: &TempDatabase, table: &str) -> i64 {
    runtime::block_on(async {
        let connection = Connection::open(database.config(), None)
            .await
            .expect("could not reopen the test database");
        let result = connection
            .run_query(&format!("SELECT count(*) FROM {table}"))
            .await;
        connection.close().await;
        match result {
            Ok(result) => result.rows[0][0]
                .as_ref()
                .expect("count(*) is never null")
                .parse()
                .expect("count(*) is a number"),
            Err(_) => 0,
        }
    })
}

#[gpui_kit::test]
fn a_read_only_connection_blocks_the_import(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    let path = dir.file("dump.sql", "CREATE TABLE extra (id int);\n");
    let (_database, handle, session) = workspace_session_with_safety(cx, SafetyMode::ReadOnly);

    open_import(cx, &handle, &session, path);
    let view = import_view(cx, &session);
    view.read_with(cx, |view, _| {
        assert!(view.is_ready_for_test());
        let reason = view
            .blocked_reason_for_test()
            .expect("a read-only connection is blocked");
        assert!(reason.contains("read-only"), "{reason}");
    });

    // Starting anyway must not run anything.
    start(cx, &view);
    view.read_with(cx, |view, _| {
        assert!(view.is_ready_for_test(), "a blocked import must not run");
        assert!(view.summary_for_test().is_none());
    });
}

#[gpui_kit::test]
fn escape_closes_the_dialog(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    let path = dir.file("dump.sql", "SELECT 1;\n");
    let (_database, handle, session) = workspace_session(cx);

    open_import(cx, &handle, &session, path);
    import_view(cx, &session);

    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        session
            .read_with(cx, |session, _| session.import_view_for_test())
            .is_none(),
        "escape should take the dialog off the session"
    );
    assert!(
        !workspace_dialog_open(cx, &handle),
        "the dialog should be closed"
    );
}

#[gpui_kit::test]
fn a_dump_is_imported_and_the_sidebar_reloads(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    let path = dir.file(
        "dump.sql",
        "CREATE TABLE extra (id int);\nINSERT INTO extra VALUES (1);\nINSERT INTO extra VALUES (2);\n",
    );
    let (database, handle, session) = workspace_session(cx);

    open_import(cx, &handle, &session, path);
    let view = import_view(cx, &session);
    start(cx, &view);

    view.read_with(cx, |view, _| {
        assert!(view.is_done_for_test());
        let summary = view.summary_for_test().expect("a summary");
        assert_eq!(summary.statements, 3);
        assert!(summary.errors.is_empty());
        assert!(!summary.rolled_back);
    });
    assert_eq!(count_table(&database, "extra"), 2);

    // Finishing reloads the schema, so the new table reaches the sidebar.
    cx.run_until_parked();
    let labels = session.read_with(cx, |session, _| session.visible_object_labels());
    assert!(labels.contains(&"extra".to_string()), "{labels:?}");
}

#[gpui_kit::test]
fn continuing_past_an_error_is_reported(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    let path = dir.file(
        "dump.sql",
        "CREATE TABLE extra (id int);\nINSERT INTO extra VALUES (1);\nTHIS IS NOT SQL;\nINSERT INTO extra VALUES (2);\n",
    );
    let (database, handle, session) = workspace_session(cx);

    open_import(cx, &handle, &session, path);
    let view = import_view(cx, &session);
    view.update(cx, |view, cx| {
        view.set_on_error_for_test(OnError::Continue, cx)
    });
    start(cx, &view);

    view.read_with(cx, |view, _| {
        let summary = view.summary_for_test().expect("a summary");
        assert_eq!(summary.errors.len(), 1);
        assert_eq!(summary.errors[0].line, 3);
        assert_eq!(summary.statements, 4);
    });
    assert_eq!(count_table(&database, "extra"), 2);

    view.update(cx, |view, cx| view.copy_log_for_test(cx));
    let log = clipboard(cx).expect("the log should be on the clipboard");
    assert!(log.contains("line 3"), "{log}");
    assert!(log.contains("Imported 4 statements"), "{log}");
}

#[gpui_kit::test]
fn rolling_back_undoes_the_import_in_the_dialog(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    let path = dir.file(
        "dump.sql",
        "CREATE TABLE extra (id int);\nINSERT INTO extra VALUES (1);\nTHIS IS NOT SQL;\n",
    );
    let (database, handle, session) = workspace_session(cx);

    open_import(cx, &handle, &session, path);
    let view = import_view(cx, &session);
    view.update(cx, |view, cx| {
        view.set_on_error_for_test(OnError::Rollback, cx)
    });
    start(cx, &view);

    view.read_with(cx, |view, _| {
        let summary = view.summary_for_test().expect("a summary");
        assert!(summary.rolled_back);
    });
    assert_eq!(
        count_table(&database, "extra"),
        0,
        "the table should not survive the rollback"
    );
}

#[gpui_kit::test]
fn closing_the_dialog_clears_the_session_handle(cx: &mut TestAppContext) {
    let dir = ScratchDir::new();
    let path = dir.file("dump.sql", "SELECT 1;\n");
    let (_database, handle, session) = workspace_session(cx);

    open_import(cx, &handle, &session, path);
    let view = import_view(cx, &session);

    view.update(cx, |view, cx| view.dismiss_for_test(cx));
    cx.run_until_parked();

    assert!(
        session
            .read_with(cx, |session, _| session.import_view_for_test())
            .is_none(),
        "closing should take the dialog off the session"
    );
    assert!(
        !workspace_dialog_open(cx, &handle),
        "the dialog should be closed"
    );
}
