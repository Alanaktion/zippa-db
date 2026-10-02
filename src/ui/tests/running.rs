//! Running SQL: one statement, a selection, a whole script.

use super::*;

#[gpui_kit::test]
fn the_run_shortcut_sends_only_the_statement_the_caret_is_in(cx: &mut TestAppContext) {
    let (database, handle) = session_with_objects(cx);

    let sql = "insert into items values (3, 'gamma', 3.0, NULL);\n\
               insert into items values (4, 'delta', 4.0, NULL);";
    // The caret sits in the second statement.
    prepare_editor(cx, handle, sql, sql.len());
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        3,
        "only the statement the caret is in should have run"
    );
    assert_eq!(
        runtime::block_on(name_of(&database, 4)),
        Some("delta".to_string())
    );
}

#[gpui_kit::test]
fn a_selection_is_what_runs(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let sql = "select * from items;\nselect 1;";
    prepare_editor(cx, handle, sql, 0);
    handle
        .update(cx, |session, _, cx| {
            let editor = session
                .active_editor_for_test(cx)
                .expect("a query tab has an editor");
            editor.update(cx, |editor, cx| {
                // Select the second statement, caret notwithstanding.
                editor.select_for_test(21..30, cx);
                // A selection runs as written, semicolon and all.
                assert_eq!(editor.statement_for_test(cx).as_deref(), Some("select 1;"));
            });
        })
        .unwrap();
}

#[gpui_kit::test]
fn the_script_shortcut_runs_every_statement(cx: &mut TestAppContext) {
    let (database, handle) = session_with_objects(cx);

    let sql = "insert into items values (3, 'gamma', 3.0, NULL);\n\
               select * from items;\n\
               select name from items where id = 1;";
    prepare_editor(cx, handle, sql, 0);
    press(cx, handle, "secondary-shift-enter");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        3,
        "the insert should have run as part of the script"
    );

    let (results, shown) = handle
        .update(cx, |session, _, cx| session.results_for_test(cx))
        .unwrap();
    assert_eq!(results, 3, "one result per statement");
    assert_eq!(shown, 0, "the first one is showing");

    let status = handle
        .update(cx, |session, _, cx| session.active_status_for_test(cx))
        .unwrap();
    assert!(
        status.starts_with("3 statements in") && status.contains("result 1 of 3"),
        "the banner should count the statements: {status}"
    );
}

#[gpui_kit::test]
fn a_result_tab_shows_that_statements_rows(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(
        cx,
        handle,
        "select * from items;\nselect name from items where id = 1;",
        0,
    );
    press(cx, handle, "secondary-shift-enter");
    cx.run_until_parked();

    click_in_session(cx, handle, "result-1");
    cx.run_until_parked();

    let (_, shown) = handle
        .update(cx, |session, _, cx| session.results_for_test(cx))
        .unwrap();
    assert_eq!(shown, 1, "the second result should be showing");

    let grid = handle
        .update(cx, |session, _, cx| session.active_grid(cx))
        .unwrap()
        .expect("a query tab has a grid");
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.column_values_for_test(0, cx)),
        vec![Some("alpha".to_string())],
        "the grid should hold the second statement's rows"
    );
}

#[gpui_kit::test]
fn a_write_reports_the_rows_it_changed(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "update items set score = 9.0", 0);
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    let status = handle
        .update(cx, |session, _, cx| session.active_status_for_test(cx))
        .unwrap();
    assert!(
        status.starts_with("2 rows affected in"),
        "a write should say what it changed: {status}"
    );
}

#[gpui_kit::test]
fn cancelling_stops_waiting_on_a_query(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // Something has to hold the caret for a keystroke to reach the session.
    prepare_editor(cx, handle, "select 1", 0);

    // A query finishes before anything can be cancelled under test, so the
    // running state is set by hand and the cancel path driven from there.
    handle
        .update(cx, |session, _, cx| session.mark_running_for_test(cx))
        .unwrap();
    assert!(
        handle
            .update(cx, |session, _, cx| session.active_is_running(cx))
            .unwrap()
    );

    press(cx, handle, "secondary-.");
    cx.run_until_parked();

    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.active_status_for_test(cx))
            .unwrap(),
        "Cancelled"
    );
    assert!(
        !handle
            .update(cx, |session, _, cx| session.active_is_running(cx))
            .unwrap(),
        "the editor should stop saying it is running"
    );
}

/// Three inserts, the middle one into a table that is not there.
const SCRIPT_FAILING_IN_THE_MIDDLE: &str = "insert into items values (3, 'gamma', 3.0, NULL);\n\
                                            insert into not_a_table values (1);\n\
                                            insert into items values (4, 'delta', 4.0, NULL);";

fn workspace_status(cx: &mut TestAppContext, session: &Entity<Session>) -> String {
    session.read_with(cx, |session, cx| session.active_status_for_test(cx))
}

#[gpui_kit::test]
fn a_failing_script_pauses_and_rolling_back_keeps_nothing(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session_with_safety(cx, SafetyMode::Staged);

    prepare_workspace_editor(cx, &handle, &session, SCRIPT_FAILING_IN_THE_MIDDLE);
    press_workspace(cx, &handle, "secondary-shift-enter");

    assert!(
        workspace_dialog_open(cx, &handle),
        "the failure should be asked about"
    );

    click_workspace(cx, &handle, "script-abort");
    cx.run_until_parked();

    assert!(!workspace_dialog_open(cx, &handle));
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "rolling back should undo the insert before the failure"
    );
    let status = workspace_status(cx, &session);
    assert!(
        status.starts_with("Error: Rolled back"),
        "the status bar should say the script was rolled back: {status}"
    );
}

#[gpui_kit::test]
fn skipping_a_failure_finishes_the_script_and_lists_it(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session_with_safety(cx, SafetyMode::Staged);

    prepare_workspace_editor(cx, &handle, &session, SCRIPT_FAILING_IN_THE_MIDDLE);
    press_workspace(cx, &handle, "secondary-shift-enter");
    click_workspace(cx, &handle, "script-skip");
    cx.run_until_parked();

    assert_eq!(runtime::block_on(other_count(&database)), 4);
    let (results, _) = session.read_with(cx, |session, cx| session.results_for_test(cx));
    assert_eq!(results, 3, "two inserts, then the failures");
    let status = workspace_status(cx, &session);
    assert!(
        status.contains("1 statement failed and skipped"),
        "the status bar should count the failures: {status}"
    );
}

#[gpui_kit::test]
fn the_ignore_errors_shortcut_runs_everything_without_asking(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session_with_safety(cx, SafetyMode::Staged);

    prepare_workspace_editor(cx, &handle, &session, SCRIPT_FAILING_IN_THE_MIDDLE);
    press_workspace(cx, &handle, "secondary-alt-shift-enter");

    assert!(
        !workspace_dialog_open(cx, &handle),
        "ignoring errors asks nothing"
    );
    assert_eq!(runtime::block_on(other_count(&database)), 4);
}

#[gpui_kit::test]
fn a_script_a_transaction_cannot_hold_is_asked_about_first(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session_with_safety(cx, SafetyMode::Staged);

    prepare_workspace_editor(
        cx,
        &handle,
        &session,
        "insert into items values (3, 'gamma', 3.0, NULL);\nvacuum;",
    );
    press_workspace(cx, &handle, "secondary-shift-enter");

    assert!(
        workspace_dialog_open(cx, &handle),
        "a script with VACUUM should ask before running without a transaction"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "nothing runs until the question is answered"
    );

    click_workspace(cx, &handle, "ok");
    cx.run_until_parked();
    assert_eq!(runtime::block_on(other_count(&database)), 3);
}

#[gpui_kit::test]
fn escaping_the_failure_question_rolls_back(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session_with_safety(cx, SafetyMode::Staged);

    prepare_workspace_editor(cx, &handle, &session, SCRIPT_FAILING_IN_THE_MIDDLE);
    press_workspace(cx, &handle, "secondary-shift-enter");
    assert!(workspace_dialog_open(cx, &handle));

    press_workspace(cx, &handle, "escape");

    assert!(!workspace_dialog_open(cx, &handle));
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "a question dismissed without an answer should take the careful one"
    );
    assert!(
        !session.read_with(cx, |session, cx| session.active_is_running(cx)),
        "the tab should be free to run again"
    );
}
