//! What each safety mode refuses, confirms, or stages.

use super::*;

#[gpui_kit::test]
fn a_view_is_read_only(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.set_metadata_for_test(
                vec!["main".to_string()],
                vec![DatabaseObject {
                    schema: None,
                    name: "named_items".into(),
                    kind: ObjectKind::View,
                }],
                cx,
            );
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-named_items", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let view = handle
        .update(cx, |session, _, cx| session.active_table_view(cx))
        .unwrap()
        .expect("clicking a view should open a table view");

    view.update(cx, |view, cx| {
        assert!(
            !view.is_editable_for_test(),
            "a view has no rows of its own to write back"
        );
        assert!(!view.grid_for_test().read(cx).editable_for_test(cx));
    });
}

#[gpui_kit::test]
fn a_read_only_connection_cannot_edit_the_grid(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view_with_safety(cx, SafetyMode::ReadOnly);
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert!(
            !view.is_editable_for_test(),
            "a read-only connection writes nothing"
        );
        assert!(!view.grid_for_test().read(cx).editable_for_test(cx));
    });
}

#[gpui_kit::test]
fn a_read_only_connection_reports_what_the_editor_refused(cx: &mut TestAppContext) {
    let (database, handle) = session_with_safety(cx, SafetyMode::ReadOnly);

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test(
                "insert into items values (5, 'five', 5.0, NULL)",
                window,
                cx,
            );
        })
        .unwrap();
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    let status = handle
        .update(cx, |session, _, cx| session.active_status_for_test(cx))
        .unwrap();
    assert!(
        status.contains("read-only") && status.contains("INSERT"),
        "the status bar should say what was refused: {status}"
    );
    // The colour it is drawn in is the only other thing saying this went
    // wrong, so the words have to say it too.
    assert!(
        status.starts_with("Error: "),
        "the status bar should name an error as one: {status}"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "the seed rows should be untouched"
    );
}

#[gpui_kit::test]
fn a_confirming_connection_shows_the_update_before_it_runs(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view_with_safety(cx, SafetyMode::ConfirmWrites);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "confirmed");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, _| view.confirming_for_test()),
        Some("1 edited row".to_string()),
        "the panel should say what is about to be applied"
    );
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "nothing should have been sent yet"
    );

    // Saying no leaves the edit staged to try again.
    click_in_session(cx, handle, "cancel-write");
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        assert!(view.confirming_for_test().is_none());
        assert_eq!(view.grid_for_test().read(cx).staged(cx).len(), 1);
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "cancelling must not write"
    );

    // Saying yes runs it.
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    click_in_session(cx, handle, "confirm-write");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("confirmed".to_string()),
        "confirming should write the row"
    );
    assert!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .is_empty()),
        "a written row should no longer be staged"
    );
}

#[gpui_kit::test]
fn the_footers_own_apply_hides_while_a_write_waits_to_be_confirmed(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view_with_safety(cx, SafetyMode::ConfirmWrites);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "confirmed");
    focus_grid(cx, &view);
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    // The confirm banner already asks about this write; the footer's own
    // Discard/Apply asking the same thing looks like clicking Apply did
    // nothing.
    let found = cx
        .update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            window.try_find("discard-edits").is_some() || window.try_find("apply-edits").is_some()
        })
        .unwrap();
    assert!(
        !found,
        "the footer's own Discard/Apply should not show while a write waits to be confirmed"
    );
}

#[gpui_kit::test]
fn a_confirming_connection_asks_before_running_a_write_from_the_editor(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session_with_safety(cx, SafetyMode::ConfirmWrites);

    prepare_workspace_editor(
        cx,
        &handle,
        &session,
        "insert into items values (5, 'five', 5.0, NULL)",
    );
    press_workspace(cx, &handle, "secondary-enter");

    assert!(
        workspace_dialog_open(cx, &handle),
        "a write should be asked about before it runs"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "the statement should be held until it is confirmed"
    );

    click_workspace(cx, &handle, "ok");

    assert_eq!(
        runtime::block_on(other_count(&database)),
        3,
        "confirming should run the statement"
    );
}

#[gpui_kit::test]
fn a_confirming_connection_leaves_a_cancelled_statement_unrun(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session_with_safety(cx, SafetyMode::ConfirmWrites);

    prepare_workspace_editor(cx, &handle, &session, "delete from items");
    press_workspace(cx, &handle, "secondary-enter");
    click_workspace(cx, &handle, "cancel");

    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "cancelling must not run the statement"
    );
    assert!(
        !workspace_dialog_open(cx, &handle),
        "the question should be gone once it is answered"
    );
}

#[gpui_kit::test]
fn a_read_from_the_editor_is_never_confirmed(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_safety(cx, SafetyMode::ConfirmWrites);

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("select * from items", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    let status = handle
        .update(cx, |session, _, cx| session.active_status_for_test(cx))
        .unwrap();
    assert!(
        !status.contains("Run it?"),
        "a select should run without being asked about: {status}"
    );
}

#[gpui_kit::test]
fn a_read_only_connection_deletes_nothing(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view_with_safety(cx, SafetyMode::ReadOnly);
    cx.run_until_parked();

    pick_row(cx, handle, 0);
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.delete_selected(cx));
    cx.run_until_parked();

    assert!(
        grid.read_with(cx, |grid, cx| grid.deletions(cx).is_empty()),
        "a read-only grid marks nothing"
    );
    assert_eq!(runtime::block_on(other_count(&database)), 2);
}
