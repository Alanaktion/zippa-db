//! Typing into a cell: what is staged, what is written, and when.

use super::*;

#[gpui_kit::test]
fn a_table_with_a_primary_key_can_be_edited(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.row_key_for_test(),
            Some(crate::db::RowKey::Columns(vec!["id".to_string()])),
            "items is keyed by its primary key"
        );
        assert!(view.is_editable_for_test());
        assert!(view.grid_for_test().read(cx).editable_for_test(cx));
    });
}

#[gpui_kit::test]
fn leaving_the_row_writes_it_on_an_auto_apply_connection(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view_with_safety(cx, SafetyMode::AutoApply);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "renamed");
    let grid = view.read_with(cx, |view, _| view.grid_for_test());

    // Moving the selection to another row is what applies the edit, the way a
    // form field applies when you tab out of it.
    grid.update(cx, |grid, cx| grid.select_cell_for_test(1, 1, cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("renamed".to_string()),
        "leaving the row should have written it"
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
fn leaving_the_row_keeps_the_edit_on_a_staged_connection(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view_with_safety(cx, SafetyMode::Staged);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "renamed");
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.select_cell_for_test(1, 1, cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "a staged connection must not write until the edit is applied"
    );
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .len()),
        1,
        "the edit should still be waiting"
    );

    // The edit is still there to apply by hand.
    focus_grid(cx, &view);
    press(cx, handle, "secondary-s");
    cx.run_until_parked();
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("renamed".to_string()),
        "applying by hand should write the staged row"
    );
}

#[gpui_kit::test]
fn applying_with_secondary_s_writes_the_row(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "applied");
    focus_grid(cx, &view);
    press(cx, handle, "secondary-s");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("applied".to_string()),
        "the save key should write a table tab's staged edits"
    );
}

#[gpui_kit::test]
fn discarding_reverts_staged_edits_without_writing(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "thrown away");
    focus_grid(cx, &view);
    press(cx, handle, "secondary-z");
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.read_with(cx, |grid, cx| {
        assert!(grid.staged(cx).is_empty(), "discard should drop the edits");
        assert_eq!(
            grid.cell_for_test(0, 1, cx),
            Some("alpha".to_string()),
            "the cell should show the loaded value again"
        );
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "discard must not write anything"
    );
}

#[gpui_kit::test]
fn setting_a_cell_to_null_writes_null(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| {
        grid.select_cell_for_test(0, 1, cx);
        grid.set_null(cx);
    });
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        None,
        "the set null command should store SQL NULL"
    );
}

#[gpui_kit::test]
fn a_table_without_a_primary_key_is_written_by_rowid(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view_on(
        cx,
        "notes",
        "create table notes (body text); insert into notes values ('first')",
    );
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.row_key_for_test(),
            Some(crate::db::RowKey::RowId("rowid")),
            "a table with no primary key falls back to the engine's row id"
        );
        assert!(view.is_editable_for_test());
        assert_eq!(
            view.query(cx),
            "select rowid, * from notes limit 500 offset 0",
            "the row id has to be asked for by name: select * leaves it out"
        );
        // It is asked for so rows can be addressed, not so it can be shown.
        assert_eq!(
            view.grid_for_test().read(cx).cell_for_test(0, 0, cx),
            Some("first".to_string()),
            "the row id column should be taken back out before the grid sees it"
        );
    });

    stage_cell(cx, &view, 0, 0, "rewritten");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    let body = runtime::block_on(async {
        let connection = Connection::open(database.config(), None)
            .await
            .expect("could not reopen the test database");
        let result = connection
            .run_query("select body from notes")
            .await
            .expect("could not read the row");
        connection.close().await;
        result.rows[0][0].clone()
    });
    assert_eq!(
        body,
        Some("rewritten".to_string()),
        "a row addressed by its row id should still be written"
    );
}

#[gpui_kit::test]
fn a_binary_cell_cannot_be_typed_into(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // `payload` is a BLOB, and the grid only holds a description of it.
    stage_cell(cx, &view, 0, 3, "not a blob");

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.read_with(cx, |grid, cx| {
        assert!(
            grid.staged(cx).is_empty(),
            "a value the grid never read back must not be writable"
        );
        assert_eq!(
            grid.cell_for_test(0, 3, cx),
            Some("<3 bytes>".to_string()),
            "the cell should still show what the driver said about it"
        );
    });
}

#[gpui_kit::test]
fn escape_closes_the_editor_without_staging(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "abandoned");
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.editing_for_test(cx)),
        Some((0, 1)),
        "the editor should be open on the cell when escape arrives"
    );

    press(cx, handle, "escape");
    cx.run_until_parked();

    grid.read_with(cx, |grid, cx| {
        assert!(
            grid.staged(cx).is_empty(),
            "escape should throw away what was being typed"
        );
        assert_eq!(
            grid.cell_for_test(0, 1, cx),
            Some("alpha".to_string()),
            "the cell should show the loaded value again"
        );
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "a cancelled edit must not reach the server"
    );
}

#[gpui_kit::test]
fn a_row_that_changed_underneath_is_reported_rather_than_written(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "too late");
    // The row is gone by the time the write runs, so nothing matches it.
    run_external(&database, "delete from items where id = 1");

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    let error = view
        .read_with(cx, |view, _| view.error_for_test())
        .expect("a write that matched no rows should be reported");
    assert!(
        error.contains("matched 0 rows"),
        "the error should say what the write did: {error}"
    );
    assert!(
        !view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .is_empty()),
        "a write that failed should leave the edit staged to retry"
    );
}

#[gpui_kit::test]
fn typing_null_means_sql_null_only_when_the_setting_says_so(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // Off by default: an edit means the four characters that were typed.
    stage_cell(cx, &view, 0, 1, "NULL");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("NULL".to_string()),
        "with the setting off, NULL is text"
    );

    cx.update(|cx| settings::update(cx, |settings| settings.coerce_null_literal = true));
    stage_cell(cx, &view, 0, 1, "null");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        None,
        "with the setting on, typing null stores SQL NULL"
    );
}

#[gpui_kit::test]
fn enter_opens_the_editor_on_the_selected_cell(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 0, 1);
    press(cx, handle, "enter");
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .editing_for_test(cx)),
        Some((0, 1)),
        "enter should open the editor on the selected cell"
    );
}

#[gpui_kit::test]
fn applying_while_typing_folds_the_cell_in(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    // The editor keeps the focus here, so the save key has to reach the table
    // view from inside the input rather than from the grid.
    stage_cell(cx, &view, 0, 1, "mid-edit");
    press(cx, handle, "secondary-s");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("mid-edit".to_string()),
        "applying while a cell is open should write what is being typed"
    );
}

#[gpui_kit::test]
fn the_null_shortcut_stages_sql_null(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 0, 1);
    press(cx, handle, "secondary-shift-n");
    cx.run_until_parked();

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        None,
        "the null shortcut should stage SQL NULL"
    );
}
