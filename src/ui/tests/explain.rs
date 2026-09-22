//! Reading a query plan: the Explain actions, the plan pane, and the gate on
//! `ANALYZE`.

use super::*;

/// The labels of the plan the active tab is showing, in tree order.
fn plan_labels(cx: &mut TestAppContext, handle: WindowHandle<Session>) -> Vec<String> {
    handle
        .update(cx, |session, _, cx| {
            session
                .active_plan_view_for_test(cx)
                .map(|plan| plan.read(cx).labels_for_test())
                .unwrap_or_default()
        })
        .unwrap()
}

#[gpui_kit::test]
fn explaining_a_select_shows_a_plan_and_its_warning(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // No index on `name`, so SQLite scans the table and the viewer should say
    // so in words.
    prepare_editor(cx, handle, "select * from items where name = 'alpha'", 0);
    press(cx, handle, "secondary-e");
    cx.run_until_parked();

    // Draw the plan pane itself, so its tree is really built and laid out.
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();

    // The warnings themselves are behind a button, so a long list cannot push
    // the steps off the pane.
    let has_button = cx
        .update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            window.try_find("plan-warnings").is_some()
        })
        .unwrap();
    assert!(has_button, "the warnings should have a button of their own");

    assert!(
        handle
            .update(cx, |session, _, cx| session
                .active_plan_showing_for_test(cx))
            .unwrap(),
        "a plan should be the pane being shown"
    );

    let labels = plan_labels(cx, handle);
    assert!(
        labels.iter().any(|label| label.contains("items")),
        "the plan should name the table: {labels:?}"
    );

    let warnings = handle
        .update(cx, |session, _, cx| {
            session
                .active_plan_view_for_test(cx)
                .map(|plan| plan.read(cx).warnings_for_test())
                .unwrap_or_default()
        })
        .unwrap();
    assert!(
        warnings
            .iter()
            .any(|(_, warning)| warning.contains("Full table scan")),
        "a scan should warn: {warnings:?}"
    );

    let status = handle
        .update(cx, |session, _, cx| session.active_status_for_test(cx))
        .unwrap();
    assert!(
        status.starts_with("Plan for 1 statement in"),
        "the status bar should describe the plan: {status}"
    );
}

#[gpui_kit::test]
fn the_analyze_button_is_off_for_a_write(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // The gate is on the statement itself, so a write is refused whatever the
    // engine can do.
    let writes = |cx: &mut TestAppContext, sql: &str| {
        prepare_editor(cx, handle, sql, 0);
        handle
            .update(cx, |session, _, cx| {
                session
                    .active_editor_for_test(cx)
                    .map(|editor| editor.read(cx).statement_writes(cx))
                    .unwrap_or(false)
            })
            .unwrap()
    };

    assert!(
        writes(cx, "delete from items where id = 1"),
        "analyzing a write should be disabled"
    );
    assert!(
        !writes(cx, "select * from items"),
        "analyzing a read is what the button is for"
    );
}

#[gpui_kit::test]
fn the_analyze_button_is_off_on_sqlite(cx: &mut TestAppContext) {
    // SQLite's planner cannot report actual times, so its analyze button is off
    // even for a read rather than showing the plain plan under it.
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "select * from items", 0);
    let disabled = handle
        .update(cx, |session, _, cx| {
            session
                .active_editor_for_test(cx)
                .map(|editor| editor.read(cx).analyze_disabled_for_test(cx))
                .unwrap_or(false)
        })
        .unwrap();
    assert!(
        disabled,
        "SQLite has no EXPLAIN ANALYZE, so analyze should be off"
    );
}

#[gpui_kit::test]
fn the_analyze_backstop_refuses_a_write(cx: &mut TestAppContext) {
    // Auto-apply runs the request without asking first, so the connection's
    // own refusal is the only thing standing between the write and the server.
    let (_database, handle) = session_with_safety(cx, SafetyMode::AutoApply);

    // Driving the action directly skips the disabled button, which is exactly
    // what the connection's own refusal is there for.
    prepare_editor(cx, handle, "delete from items", 0);
    press(cx, handle, "secondary-shift-e");
    cx.run_until_parked();

    let status = handle
        .update(cx, |session, _, cx| session.active_status_for_test(cx))
        .unwrap();
    assert!(
        status.starts_with("Error: ") && status.contains("changes data"),
        "the refusal should name itself: {status}"
    );
}

#[gpui_kit::test]
fn switching_between_the_result_and_the_plan_keeps_both(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // A result to switch back to.
    prepare_editor(cx, handle, "select * from items", 0);
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    prepare_editor(cx, handle, "select * from items where id = 1", 0);
    press(cx, handle, "secondary-e");
    cx.run_until_parked();
    assert!(
        handle
            .update(cx, |session, _, cx| session
                .active_plan_showing_for_test(cx))
            .unwrap()
    );

    click_in_session(cx, handle, "show-results");
    cx.run_until_parked();
    assert!(
        !handle
            .update(cx, |session, _, cx| session
                .active_plan_showing_for_test(cx))
            .unwrap(),
        "Results should bring the grid back"
    );
    assert!(
        handle
            .update(cx, |session, _, cx| session.active_plan_view_for_test(cx))
            .unwrap()
            .is_some(),
        "the plan should still be there to switch back to"
    );

    click_in_session(cx, handle, "show-plan");
    cx.run_until_parked();
    assert!(
        handle
            .update(cx, |session, _, cx| session
                .active_plan_showing_for_test(cx))
            .unwrap(),
        "Plan should bring the plan back"
    );
}

#[gpui_kit::test]
fn explaining_uses_the_statement_the_caret_is_in(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // The first statement would fail if it were the one explained; the caret
    // is in the second, which reads.
    let sql = "select * from does_not_exist;\nselect * from items";
    prepare_editor(cx, handle, sql, sql.len());
    press(cx, handle, "secondary-e");
    cx.run_until_parked();

    let status = handle
        .update(cx, |session, _, cx| session.active_status_for_test(cx))
        .unwrap();
    assert!(
        status.starts_with("Plan for 1 statement in"),
        "the caret's statement should be the one explained: {status}"
    );
}

#[gpui_kit::test]
fn analyze_asks_before_running_on_a_careful_connection(cx: &mut TestAppContext) {
    let (_database, handle, session) = workspace_session(cx);

    prepare_workspace_editor(cx, &handle, &session, "select * from items");
    press_workspace(cx, &handle, "secondary-shift-e");

    assert!(
        workspace_dialog_open(cx, &handle),
        "a read being analyzed is a query that runs, so it is asked about"
    );
    assert!(
        !session.read_with(cx, |session, cx| session.active_plan_showing_for_test(cx)),
        "nothing should have run while the question is open"
    );

    click_workspace(cx, &handle, "ok");

    // Confirming sends the request on to the connection, which refuses it:
    // SQLite has no EXPLAIN ANALYZE to report actual times with.
    let status = session.read_with(cx, |session, cx| session.active_status_for_test(cx));
    assert!(
        status.starts_with("Error: ") && status.contains("EXPLAIN ANALYZE"),
        "SQLite's refusal should be reported: {status}"
    );
    assert!(
        !session.read_with(cx, |session, cx| session.active_plan_showing_for_test(cx)),
        "a refused analyze shows no plan"
    );
}

#[gpui_kit::test]
fn a_cancelled_analyze_reads_nothing(cx: &mut TestAppContext) {
    let (_database, handle, session) = workspace_session(cx);

    prepare_workspace_editor(cx, &handle, &session, "select * from items");
    press_workspace(cx, &handle, "secondary-shift-e");
    click_workspace(cx, &handle, "cancel");

    assert!(
        !session.read_with(cx, |session, cx| session.active_plan_showing_for_test(cx)),
        "cancelling must not read a plan"
    );
}

#[gpui_kit::test]
fn the_warnings_button_opens_a_dialog(cx: &mut TestAppContext) {
    let (_database, handle, session) = workspace_session(cx);

    prepare_workspace_editor(
        cx,
        &handle,
        &session,
        "select * from items where name = 'alpha'",
    );
    press_workspace(cx, &handle, "secondary-e");

    click_workspace(cx, &handle, "plan-warnings");

    assert!(
        workspace_dialog_open(cx, &handle),
        "the warnings button should open a dialog"
    );
}
