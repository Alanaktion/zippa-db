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
fn a_binary_value_is_re_read_and_shown_as_a_hex_dump(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // `payload` is a BLOB, so the grid only ever held a `<3 bytes>`
    // description of it; the dialog opens with that stand-in, then a table
    // view can still address row 1 by its key (`x'001122'`), so the real
    // bytes are re-read and the dialog is replaced with them.
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 3, cx));
    cx.run_until_parked();

    let request = pending_value(cx);
    assert!(request.text.contains("00 11 22"), "{}", request.text);
    assert!(
        request.save.is_none(),
        "a binary value is not typed text, so it has no text save"
    );
    assert!(
        request.save_binary.is_some(),
        "a writable table's binary dialog can stage a file"
    );
}

#[gpui_kit::test]
fn a_loaded_file_stages_binary_bytes_that_commit_back(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 3, cx));
    cx.run_until_parked();

    let request = pending_value(cx);
    let save_binary = request
        .save_binary
        .expect("a writable table's binary dialog stages a file");
    cx.update(|cx| save_binary(vec![0xde, 0xad, 0xbe, 0xef], cx));

    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.cell_for_test(0, 3, cx)),
        Some("deadbeef".to_string()),
        "the staged value is the file's bytes as hex"
    );
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .len()),
        1,
        "staging a file is a staged edit like any other"
    );

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(payload_of(&database, 1)),
        Some(vec![0xde, 0xad, 0xbe, 0xef]),
        "applying should write the staged bytes back"
    );

    // A binary write is stale by definition: the staged value is hex text,
    // so applying it to the baseline would leave the cell showing hex
    // instead of the value's stand-in. The commit reloads instead.
    cx.run_until_parked();
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.cell_for_test(0, 3, cx)),
        Some("<4 bytes>".to_string()),
        "after a binary write the cell shows the re-read value's stand-in, not the staged hex"
    );
}

#[gpui_kit::test]
fn the_dialog_loads_a_file_and_saves_it_as_a_staged_edit(cx: &mut TestAppContext) {
    let (_database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 3, cx));
    cx.run_until_parked();
    draw_workspace(cx, &handle);

    let dialog = handle
        .update(cx, |workspace, _, _| workspace.value_dialog_for_test())
        .unwrap()
        .expect("the dialog should be open over the window");
    dialog.read_with(cx, |dialog, _| {
        assert!(
            dialog.has_binary_save_for_test(),
            "the binary dialog should offer to load a file"
        );
    });

    dialog
        .downgrade()
        .update_in(cx, |dialog, window, cx| {
            dialog.load_bytes_for_test(vec![0xde, 0xad, 0xbe, 0xef], window, cx);
        })
        .unwrap();
    dialog.read_with(cx, |dialog, _| {
        assert_eq!(
            dialog.staged_bytes_for_test(),
            Some(vec![0xde, 0xad, 0xbe, 0xef]),
            "loading a file should hold its bytes for saving"
        );
    });

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
        grid.read_with(cx, |grid, cx| grid.cell_for_test(0, 3, cx)),
        Some("deadbeef".to_string()),
        "the saved file should be staged in the cell"
    );
}

#[gpui_kit::test]
fn a_query_result_cannot_re_read_its_binary_value(cx: &mut TestAppContext) {
    use crate::db::query::blob;

    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(
                QueryResult {
                    columns: vec!["payload".into()],
                    column_types: vec!["BLOB".into()],
                    rows: vec![vec![blob(&[0x00, 0x11, 0x22])]],
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

    // A bare query result has no table to re-read the row from, so the
    // dialog is left on the description the driver gave.
    let request = pending_value(cx);
    assert!(request.text.contains("not read back"));
    assert!(request.save.is_none(), "there is nothing to write back");
}

#[gpui_kit::test]
fn the_dialog_saves_with_the_platform_shortcut(cx: &mut TestAppContext) {
    let (database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    // The value is laid out on the background executor.
    cx.run_until_parked();
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
    // The value is laid out on the background executor.
    cx.run_until_parked();
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
    // The value is laid out on the background executor.
    cx.run_until_parked();
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
    // The value is laid out on the background executor.
    cx.run_until_parked();
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
    // The value is laid out on the background executor.
    cx.run_until_parked();
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

#[gpui_kit::test]
fn staging_a_binary_value_against_a_stale_result_is_refused(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    let generation = grid.read_with(cx, |grid, _| grid.result_generation());

    let staged = grid.update(cx, |grid, cx| {
        grid.stage_binary_cell(0, "payload", vec![0xde, 0xad], generation + 1, cx)
    });
    assert!(
        !staged,
        "a save built on an older result must be refused, not staged onto rows that moved"
    );
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.staged(cx).len()),
        0,
        "a refused save stages nothing"
    );

    let staged = grid.update(cx, |grid, cx| {
        grid.stage_binary_cell(0, "payload", vec![0xde, 0xad], generation, cx)
    });
    assert!(staged, "a save built on the current result stages");
    assert_eq!(grid.read_with(cx, |grid, cx| grid.staged(cx).len()), 1);
}

#[gpui_kit::test]
fn a_reload_between_opening_and_saving_keeps_the_dialog_open(cx: &mut TestAppContext) {
    let (_database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 3, cx));
    cx.run_until_parked();
    draw_workspace(cx, &handle);

    let dialog = handle
        .update(cx, |workspace, _, _| workspace.value_dialog_for_test())
        .unwrap()
        .expect("the dialog should be open over the window");
    dialog
        .downgrade()
        .update_in(cx, |dialog, window, cx| {
            dialog.load_bytes_for_test(vec![0xde, 0xad, 0xbe, 0xef], window, cx);
        })
        .unwrap();

    // A reload lands while the dialog is open: the result it opened on is
    // gone, so its save is built on rows that may have moved.
    view.update(cx, |view, cx| view.reload(cx));
    cx.run_until_parked();

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
                .is_some())
            .unwrap(),
        "a refused save must leave the dialog open with its bytes, not dismiss and lose them"
    );
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .len()),
        0,
        "nothing should be staged onto rows that moved"
    );
}

#[gpui_kit::test]
fn an_earlier_file_load_finishing_late_does_not_replace_a_newer_one(cx: &mut TestAppContext) {
    let (_database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 3, cx));
    cx.run_until_parked();
    draw_workspace(cx, &handle);

    let dialog = handle
        .update(cx, |workspace, _, _| workspace.value_dialog_for_test())
        .unwrap()
        .expect("the dialog should be open over the window");
    dialog
        .downgrade()
        .update_in(cx, |dialog, window, cx| {
            let first = dialog.begin_load_for_test();
            let second = dialog.begin_load_for_test();
            dialog.finish_load_for_test(first, vec![0x01], window, cx);
            assert_eq!(
                dialog.staged_bytes_for_test(),
                None,
                "an earlier read finishing after a later one must not stage"
            );
            assert!(
                dialog.load_pending_for_test(),
                "the stale completion must not free the button while the newer read is in flight"
            );
            dialog.finish_load_for_test(second, vec![0x02], window, cx);
        })
        .unwrap();

    dialog.read_with(cx, |dialog, _| {
        assert_eq!(
            dialog.staged_bytes_for_test(),
            Some(vec![0x02]),
            "only the latest load stages"
        );
        assert!(
            !dialog.load_pending_for_test(),
            "the latest load frees the button"
        );
    });
}
