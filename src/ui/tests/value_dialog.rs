//! The dialog that shows one cell's value in full.

use super::*;

#[gpui_kit::test]
fn a_value_is_handed_to_the_dialog(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    cx.run_until_parked();

    let request = pending_value(cx);
    assert_eq!(request.column, "name");
    assert_eq!(request.text, "alpha");
    assert!(
        request.save.is_some(),
        "a row of a writable table can be edited in the dialog"
    );
}

#[gpui_kit::test]
fn the_value_shortcut_hands_over_the_selected_cell(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 1, 1);
    press(cx, handle, "secondary-shift-v");
    cx.run_until_parked();

    let request = pending_value(cx);
    assert_eq!(request.column, "name");
    assert_eq!(request.text, "NULL", "the second row has no name");
}

#[gpui_kit::test]
fn json_is_laid_out_and_a_query_result_cannot_be_edited(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(
                QueryResult {
                    columns: vec!["document".into()],
                    column_types: vec!["JSONB".into()],
                    rows: vec![vec![Some(r#"{"name":"alpha","tags":[1,2]}"#.into())]],
                    ..QueryResult::default()
                },
                cx,
            );
        })
        .unwrap();

    let grid = handle
        .update(cx, |session, _, cx| session.active_grid(cx))
        .unwrap()
        .expect("a query tab has a grid");
    grid.update(cx, |grid, cx| grid.view_cell(0, 0, cx));
    cx.run_until_parked();

    let request = pending_value(cx);
    assert_eq!(request.column, "document");
    assert_eq!(
        request.text, "{\n  \"name\": \"alpha\",\n  \"tags\": [\n    1,\n    2\n  ]\n}",
        "JSON should be laid out over several lines"
    );
    assert!(
        request.save.is_none(),
        "a query result has no owner that could write it back"
    );
}

#[gpui_kit::test]
fn a_value_that_was_never_read_says_so(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // `payload` is a BLOB, so the grid only ever held a description of it.
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 3, cx));
    cx.run_until_parked();

    let request = pending_value(cx);
    assert!(request.text.contains("not read back"));
    assert!(request.save.is_none(), "there is nothing to write back");
}

#[gpui_kit::test]
fn the_dialog_saves_with_the_platform_shortcut(cx: &mut TestAppContext) {
    let (database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    draw_workspace(cx, &handle);

    let dialog = handle
        .update(cx, |workspace, _, _| workspace.value_dialog_for_test())
        .unwrap()
        .expect("the dialog should be open over the window");
    dialog.read_with(cx, |dialog, cx| {
        assert_eq!(dialog.column_for_test(), "name");
        assert_eq!(dialog.value_for_test(cx), "alpha");
        assert!(dialog.is_editable_for_test());
    });

    dialog
        .downgrade()
        .update_in(cx, |dialog, window, cx| {
            dialog.set_value_for_test("from the dialog", window, cx);
        })
        .unwrap();

    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("secondary-enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        handle
            .update(cx, |workspace, _, _| workspace
                .value_dialog_for_test()
                .is_none())
            .unwrap(),
        "saving should close the dialog"
    );
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.cell_for_test(0, 1, cx)),
        Some("from the dialog".to_string()),
        "the saved value should be staged in the cell"
    );

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("from the dialog".to_string()),
        "applying should write what the dialog staged"
    );
}

#[gpui_kit::test]
fn saving_an_untouched_null_leaves_it_null(cx: &mut TestAppContext) {
    let (database, handle, view) = workspace_table(cx);

    // The second row has no name, so the box opens on the word `NULL`.
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(1, 1, cx));
    draw_workspace(cx, &handle);

    let dialog = handle
        .update(cx, |workspace, _, _| workspace.value_dialog_for_test())
        .unwrap()
        .expect("the dialog should be open over the window");
    assert_eq!(
        dialog.read_with(cx, |dialog, cx| dialog.value_for_test(cx)),
        "NULL"
    );

    // Saving it as it stood is not an edit, so the NULL must survive.
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("secondary-enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        grid.read_with(cx, |grid, cx| grid.staged(cx).is_empty()),
        "saving the box untouched stages nothing"
    );

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    assert_eq!(
        runtime::block_on(name_of(&database, 2)),
        None,
        "an untouched NULL must not become the word NULL"
    );
}

#[gpui_kit::test]
fn the_dialog_coerces_a_typed_null_like_the_cell_editor(cx: &mut TestAppContext) {
    let (database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    draw_workspace(cx, &handle);

    let dialog = handle
        .update(cx, |workspace, _, _| workspace.value_dialog_for_test())
        .unwrap()
        .expect("the dialog should be open over the window");
    dialog
        .downgrade()
        .update_in(cx, |dialog, window, cx| {
            dialog.set_value_for_test("null", window, cx);
        })
        .unwrap();

    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("secondary-enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        None,
        "with the setting on, typing null in the dialog stores SQL NULL"
    );
}

#[gpui_kit::test]
fn escape_closes_the_dialog_without_saving(cx: &mut TestAppContext) {
    let (database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    draw_workspace(cx, &handle);

    let dialog = handle
        .update(cx, |workspace, _, _| workspace.value_dialog_for_test())
        .unwrap()
        .expect("the dialog should be open");
    dialog
        .downgrade()
        .update_in(cx, |dialog, window, cx| {
            dialog.set_value_for_test("thrown away", window, cx);
        })
        .unwrap();

    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        handle
            .update(cx, |workspace, _, _| workspace
                .value_dialog_for_test()
                .is_none())
            .unwrap(),
        "escape should close the dialog"
    );
    assert!(
        grid.read_with(cx, |grid, cx| grid.staged(cx).is_empty()),
        "closing must not stage anything"
    );
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string())
    );
}

#[gpui_kit::test]
fn a_click_inside_the_dialog_never_reaches_the_grid(cx: &mut TestAppContext) {
    let (_database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    // Put the grid's selection on the first row, the way a user just had it.
    grid.downgrade()
        .update_in(cx, |grid, window, cx| {
            grid.focus_for_test(window, cx);
            grid.select_cell_for_test(0, 1, cx);
        })
        .unwrap();
    cx.run_until_parked();

    let before = grid.read_with(cx, |grid, cx| grid.selection_for_test(cx));

    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    draw_workspace(cx, &handle);

    // Clicking into the dialog's box would land on whatever grid cell lies
    // under it, if the overlay let the click through.
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("value-dialog-body", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        handle
            .update(cx, |workspace, _, _| workspace
                .value_dialog_for_test()
                .is_some())
            .unwrap(),
        "clicking inside the dialog must not close it"
    );
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.selection_for_test(cx)),
        before,
        "the grid behind the dialog must not receive the click"
    );
}

#[gpui_kit::test]
fn double_clicking_picks_the_editor_or_the_dialog(_cx: &mut TestAppContext) {
    use crate::db::query::{blob, unsupported};
    use crate::ui::data_grid::needs_a_window;

    // Short, plain values are typed into the cell itself.
    assert!(!needs_a_window(&Some("alpha".to_string()), "TEXT"));
    assert!(!needs_a_window(&None, "TEXT"));

    // Everything a cell cannot show goes to the dialog.
    assert!(needs_a_window(&Some("one\ntwo".to_string()), "TEXT"));
    assert!(needs_a_window(&Some("x".repeat(201)), "TEXT"));
    assert!(needs_a_window(&Some(r#"{"a":1}"#.to_string()), "TEXT"));
    assert!(needs_a_window(&Some("[1, 2]".to_string()), "JSONB"));
    assert!(needs_a_window(&blob(b"abc"), "BLOB"));
    assert!(needs_a_window(&unsupported("XML"), "XML"));

    // A value that only looks like JSON is still a value.
    assert!(!needs_a_window(&Some("{not json".to_string()), "TEXT"));
}
