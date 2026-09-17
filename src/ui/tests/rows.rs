//! Rows as a whole: picking them out, adding them, marking them to go.

use super::*;

#[gpui_kit::test]
fn checking_rows_selects_them_and_shift_takes_the_range(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    pick_row(cx, handle, 0);
    pick_through(cx, handle, 1);

    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .rows_selected_for_test(cx)),
        vec![0, 1],
        "the pick boxes of the two rows should both be checked"
    );

    // Checking one that is already checked takes it back out.
    pick_row(cx, handle, 1);
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .rows_selected_for_test(cx)),
        vec![0],
        "checking a checked row should uncheck it"
    );
}

#[gpui_kit::test]
fn deleting_rows_marks_them_until_the_changes_are_applied(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    pick_row(cx, handle, 0);
    pick_through(cx, handle, 1);
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.delete_selected(cx));
    cx.run_until_parked();

    // The rows stay on screen, marked, until the changes are applied.
    view.update(cx, |view, cx| {
        assert_eq!(view.grid_for_test().read(cx).deletions(cx), vec![0, 1]);
        assert_eq!(
            view.grid_for_test().read(cx).pending_counts(cx),
            (0, 0, 2),
            "two rows are waiting to be deleted"
        );
    });
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "nothing should be deleted yet"
    );

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        0,
        "applying should delete both rows"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.loaded_rows_for_test()),
        0,
        "the page should be read again once the rows are gone"
    );
}

#[gpui_kit::test]
fn a_marked_row_can_be_restored_or_discarded(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());

    // Restoring takes the mark off the row again.
    pick_row(cx, handle, 0);
    grid.update(cx, |grid, cx| grid.delete_selected(cx));
    grid.update(cx, |grid, cx| grid.restore_selected(cx));
    cx.run_until_parked();
    assert!(grid.read_with(cx, |grid, cx| grid.deletions(cx).is_empty()));

    // So does throwing the staged changes away.
    grid.update(cx, |grid, cx| grid.delete_selected(cx));
    cx.run_until_parked();
    focus_grid(cx, &view);
    press(cx, handle, "secondary-z");
    cx.run_until_parked();

    assert!(grid.read_with(cx, |grid, cx| grid.deletions(cx).is_empty()));
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "a discarded delete must never reach the server"
    );
}

#[gpui_kit::test]
fn the_delete_shortcut_marks_the_selected_rows(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    pick_row(cx, handle, 0);
    pick_through(cx, handle, 1);
    focus_grid(cx, &view);
    press(cx, handle, "secondary-backspace");
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |view, cx| view.grid_for_test().read(cx).deletions(cx)),
        vec![0, 1]
    );

    focus_grid(cx, &view);
    press(cx, handle, "secondary-shift-backspace");
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .deletions(cx)
            .is_empty()),
        "the restore shortcut should take the mark off again"
    );
}

#[gpui_kit::test]
fn a_new_row_is_filled_in_and_inserted(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.row_count_for_test(cx)),
        3,
        "the new row should sit below the ones the server sent"
    );

    // The new row is the third one, after the two the seed data has.
    stage_cell(cx, &view, 2, 0, "7");
    stage_cell(cx, &view, 2, 1, "seven");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 7)),
        Some("seven".to_string()),
        "applying should insert the row"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.loaded_rows_for_test()),
        3,
        "the page should be read again so the row comes back from the server"
    );
}

#[gpui_kit::test]
fn a_column_nobody_typed_into_is_left_to_the_server(cx: &mut TestAppContext) {
    // `items.id` is a SQLite row id, so leaving it out is what gets one.
    let (database, handle, view) = table_view_with_safety(cx, SafetyMode::ConfirmWrites);
    cx.run_until_parked();

    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();
    stage_cell(cx, &view, 2, 1, "eight");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, _| view.confirming_for_test()),
        Some("1 new row".to_string()),
        "the panel should say what is about to be applied"
    );

    click_in_session(cx, handle, "confirm-write");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        3,
        "confirming should insert the row"
    );
    // The key was left out of the statement, so the server picked one.
    assert_eq!(
        runtime::block_on(id_of(&database, "eight")),
        Some("3".to_string()),
        "the column nobody typed into should come from the server"
    );
}

#[gpui_kit::test]
fn an_empty_new_row_is_reported_rather_than_written(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    let error = view
        .read_with(cx, |view, _| view.error_for_test())
        .expect("an empty row should be reported");
    assert!(error.contains("empty"), "{error}");
    assert_eq!(runtime::block_on(other_count(&database)), 2);
}

#[gpui_kit::test]
fn a_new_row_can_be_thrown_away(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();
    stage_cell(cx, &view, 2, 1, "never inserted");

    // Its own menu item drops the one row.
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.discard_draft_for_test(2, cx));
    cx.run_until_parked();

    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.row_count_for_test(cx)),
        2,
        "the row should be gone from the grid"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "a discarded row must never reach the server"
    );

    // So does the discard key, for a row left half-filled.
    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();
    focus_grid(cx, &view);
    press(cx, handle, "secondary-z");
    cx.run_until_parked();
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.row_count_for_test(cx)),
        2
    );
}

#[gpui_kit::test]
fn the_insert_shortcut_adds_a_row(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    focus_grid(cx, &view);
    press(cx, handle, "secondary-shift-i");
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .row_count_for_test(cx)),
        3
    );
}

#[gpui_kit::test]
fn the_delete_shortcut_discards_a_row_being_built(cx: &mut TestAppContext) {
    // A new row has nothing on the server to mark, so the same keystroke that
    // marks a loaded row drops this one — otherwise the only way out of a row
    // added by mistake is the right-click menu.
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    focus_grid(cx, &view);
    press(cx, handle, "secondary-shift-i");
    cx.run_until_parked();

    let rows = |cx: &mut TestAppContext| {
        view.read_with(cx, |view, cx| {
            view.grid_for_test().read(cx).row_count_for_test(cx)
        })
    };
    assert_eq!(rows(cx), 3, "the new row should be on screen");

    // The two rows the server sent come first, so the new one is the third.
    pick_row(cx, handle, 2);
    focus_grid(cx, &view);
    press(cx, handle, "secondary-backspace");
    cx.run_until_parked();

    assert_eq!(
        rows(cx),
        2,
        "the new row should be gone, not struck through"
    );
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .pending_counts(cx)),
        (0, 0, 0),
        "nothing should be left waiting to be written"
    );
}

#[gpui_kit::test]
fn changes_of_every_kind_are_counted_and_applied_together(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    // Edit the first row, mark the second for deletion, add a third.
    stage_cell(cx, &view, 0, 1, "edited");
    pick_row(cx, handle, 1);
    focus_grid(cx, &view);
    press(cx, handle, "secondary-backspace");
    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();
    // Two rows came from the server, so the new one is the third.
    stage_cell(cx, &view, 2, 1, "added");

    view.update(cx, |view, cx| {
        assert_eq!(
            view.grid_for_test().read(cx).pending_counts(cx),
            (1, 1, 1),
            "one edited row, one new row, one deleted row"
        );
    });

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("edited".to_string()),
        "the edit should have been written"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "one row went, one row arrived"
    );
    assert_eq!(
        runtime::block_on(id_of(&database, "added")),
        Some("3".to_string()),
        "the new row should be there"
    );
    assert!(
        view.read_with(cx, |view, cx| view.grid_for_test().read(cx).pending(cx)
            == 0),
        "nothing should be left staged"
    );
}

#[gpui_kit::test]
fn an_edit_on_a_row_marked_for_deletion_goes_with_it(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");
    pick_row(cx, handle, 0);
    focus_grid(cx, &view);
    press(cx, handle, "secondary-backspace");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.grid_for_test().read(cx).pending_counts(cx),
            (0, 0, 1),
            "the row counts once, as a deletion"
        );
    });

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        1,
        "the row should be gone"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.error_for_test()),
        None,
        "updating a row that is being deleted should never be attempted"
    );
}

#[gpui_kit::test]
fn a_foreign_key_jump_opens_and_filters_the_referenced_table(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view_on(
        cx,
        "orders",
        "create table customers (id integer primary key, name text); \
         insert into customers (id, name) values (1, 'ada'), (2, 'grace'); \
         create table orders (id integer primary key, customer_id integer references customers(id)); \
         insert into orders (id, customer_id) values (100, 2);",
    );
    cx.run_until_parked();

    // orders' columns are [id, customer_id] in that order, so customer_id is
    // column 1 — the only one the schema read should have flagged.
    view.update(cx, |view, cx| {
        assert!(view.grid_for_test().read(cx).is_foreign_key_for_test(1, cx));
        assert!(!view.grid_for_test().read(cx).is_foreign_key_for_test(0, cx));
    });

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.navigate_for_test(0, 1, cx));
    cx.run_until_parked();

    let target = handle
        .update(cx, |session, _, cx| session.active_table_view(cx))
        .unwrap()
        .expect("following the key should open the referenced table");
    target.read_with(cx, |view, _| {
        assert_eq!(view.object().name, "customers");
    });

    let filters = target.read_with(cx, |view, _| view.filters_for_test());
    filters.read_with(cx, |filters, cx| {
        let specs = filters.specs(cx);
        assert_eq!(specs.len(), 1, "the jump should set exactly one filter");
        assert_eq!(specs[0].column, "id");
        assert_eq!(specs[0].operator, Operator::Equals);
        assert_eq!(specs[0].value, "2");
    });
    target.update(cx, |view, _cx| {
        assert_eq!(
            view.loaded_rows_for_test(),
            1,
            "only the referenced row should match"
        );
    });
}

#[gpui_kit::test]
fn a_null_foreign_key_has_nothing_to_follow(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view_on(
        cx,
        "orders",
        "create table customers (id integer primary key, name text); \
         insert into customers (id, name) values (1, 'ada'); \
         create table orders (id integer primary key, customer_id integer references customers(id)); \
         insert into orders (id, customer_id) values (100, null);",
    );
    cx.run_until_parked();

    let tabs_before = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.navigate_for_test(0, 1, cx));
    cx.run_until_parked();

    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.tab_titles(cx))
            .unwrap(),
        tabs_before,
        "a NULL foreign key value has no row to jump to"
    );
}
