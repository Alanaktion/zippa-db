//! Moving around a grid: the arrows, what the keyboard holds, and copying.

use super::*;

#[gpui_kit::test]
fn the_arrows_move_the_selection_and_stop_at_the_edges(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    // `items` has two rows and four columns: id, name, score, payload.
    select_cell(cx, &view, 0, 0);

    focus_grid(cx, &view);
    press(cx, handle, "down");
    cx.run_until_parked();
    assert_eq!(
        selection(cx, &view),
        (Some(1), Some((1, 0))),
        "down should move to the row below, taking the row highlight with it"
    );

    press(cx, handle, "down");
    cx.run_until_parked();
    assert_eq!(
        selection(cx, &view),
        (Some(1), Some((1, 0))),
        "the last row is the last row: the selection must not reappear at the top"
    );

    press(cx, handle, "up");
    press(cx, handle, "up");
    cx.run_until_parked();
    assert_eq!(
        selection(cx, &view),
        (Some(0), Some((0, 0))),
        "the first row is where up stops"
    );

    press(cx, handle, "right");
    cx.run_until_parked();
    assert_eq!(selection(cx, &view).1, Some((0, 1)));

    // Past the last column, and one more for good measure.
    for _ in 0..3 {
        press(cx, handle, "right");
    }
    cx.run_until_parked();
    assert_eq!(
        selection(cx, &view).1,
        Some((0, 3)),
        "the last column must not wrap round to the first"
    );
}

#[gpui_kit::test]
fn the_selection_never_rests_on_the_checkbox_column(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 0, 0);
    focus_grid(cx, &view);

    // The checkbox column sits before the first value, so left from it has
    // nowhere to go rather than landing on a column with nothing to show.
    press(cx, handle, "left");
    cx.run_until_parked();
    assert_eq!(
        selection(cx, &view).1,
        Some((0, 0)),
        "left from the first value should stay on it"
    );

    // Home asks for the first column of the row, which is the checkbox one.
    press(cx, handle, "home");
    cx.run_until_parked();
    assert_eq!(
        selection(cx, &view).1,
        Some((0, 0)),
        "home should land on the first value, not on the checkbox column"
    );
}

#[gpui_kit::test]
fn the_copy_shortcut_copies_the_selected_cell(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 0, 1);
    focus_grid(cx, &view);
    press(cx, handle, "secondary-c");
    cx.run_until_parked();

    assert_eq!(clipboard(cx), Some("alpha".to_string()));
}

#[gpui_kit::test]
fn copying_with_rows_picked_out_takes_the_whole_rows(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    // A cell is selected too: the picked rows are what the user is working
    // with, so they win over it.
    select_cell(cx, &view, 0, 1);
    pick_row(cx, handle, 0);
    pick_through(cx, handle, 1);

    focus_grid(cx, &view);
    press(cx, handle, "secondary-c");
    cx.run_until_parked();

    assert_eq!(
        clipboard(cx),
        Some("1\talpha\t1.5\t<3 bytes>\n2\t\t\t".to_string()),
        "the rows should copy as one line each, columns separated by tabs, \
         and a NULL as nothing"
    );
}

#[gpui_kit::test]
fn space_picks_the_focused_row_out_and_puts_it_back(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 1, 0);
    focus_grid(cx, &view);
    press(cx, handle, "space");
    cx.run_until_parked();
    assert_eq!(picked(cx, &view), vec![1]);

    press(cx, handle, "space");
    cx.run_until_parked();
    assert!(
        picked(cx, &view).is_empty(),
        "space on a picked row should put it back"
    );
}

#[gpui_kit::test]
fn space_does_not_pick_the_row_while_editing_a_cell(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.downgrade()
        .update_in(cx, |grid, window, cx| {
            grid.focus_for_test(window, cx);
            grid.select_cell_for_test(0, 1, cx);
        })
        .unwrap();
    cx.run_until_parked();

    grid.downgrade()
        .update_in(cx, |grid, window, cx| {
            grid.begin_edit_for_test(0, 1, window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    // Without a binding that wins over the row's own, the editor's context is
    // deeper than the table's, so this reaches the row picker instead of the
    // input.
    press(cx, handle, "space");
    cx.run_until_parked();

    assert!(
        picked(cx, &view).is_empty(),
        "space while editing a cell must not pick the row out"
    );
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.editing_for_test(cx)),
        Some((0, 1)),
        "the editor should still be open"
    );
}

#[gpui_kit::test]
fn the_select_all_shortcut_picks_every_row_and_the_shifted_one_clears(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 0, 0);
    focus_grid(cx, &view);
    press(cx, handle, "secondary-a");
    cx.run_until_parked();
    assert_eq!(picked(cx, &view), vec![0, 1]);

    press(cx, handle, "secondary-shift-a");
    cx.run_until_parked();
    assert!(
        picked(cx, &view).is_empty(),
        "the shifted shortcut should put every row back"
    );
}

#[gpui_kit::test]
fn shifted_arrows_take_the_rows_they_pass(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 0, 0);
    focus_grid(cx, &view);
    press(cx, handle, "shift-down");
    cx.run_until_parked();

    assert_eq!(
        picked(cx, &view),
        vec![0, 1],
        "the row it started on goes with the one it moved to"
    );
    assert_eq!(
        selection(cx, &view).1,
        Some((1, 0)),
        "the selection should have moved with the range"
    );

    // Stopping at the edge is the same as for a plain arrow.
    press(cx, handle, "shift-down");
    cx.run_until_parked();
    assert_eq!(picked(cx, &view), vec![0, 1]);
}

#[gpui_kit::test]
fn escape_clears_both_the_selection_and_the_picked_rows(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 0, 0);
    focus_grid(cx, &view);
    press(cx, handle, "secondary-a");
    cx.run_until_parked();
    assert_eq!(picked(cx, &view), vec![0, 1]);

    press(cx, handle, "escape");
    cx.run_until_parked();

    assert!(picked(cx, &view).is_empty());
    assert_eq!(
        selection(cx, &view),
        (None, None),
        "escape should leave nothing highlighted"
    );
}

#[gpui_kit::test]
fn finishing_a_cell_hands_the_keyboard_back_to_the_grid(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "typed");
    assert!(
        !grid_focused(cx, handle, &view),
        "the editor should hold the keyboard while a cell is open"
    );

    press(cx, handle, "enter");
    cx.run_until_parked();

    assert!(
        grid_focused(cx, handle, &view),
        "enter should close the editor and put the keyboard back on the rows"
    );
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .cell_for_test(0, 1, cx)),
        Some("typed".to_string()),
        "what was typed should still have been staged"
    );

    // And the arrows work again straight away.
    press(cx, handle, "down");
    cx.run_until_parked();
    assert_eq!(selection(cx, &view).1, Some((1, 1)));
}

#[gpui_kit::test]
fn giving_up_on_a_cell_hands_the_keyboard_back_too(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never staged");
    press(cx, handle, "escape");
    cx.run_until_parked();

    assert!(
        grid_focused(cx, handle, &view),
        "escape should put the keyboard back on the rows"
    );
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .cell_for_test(0, 1, cx)),
        Some("alpha".to_string()),
        "the cell should show the loaded value again"
    );
}

#[gpui_kit::test]
fn dragging_down_the_boxes_sweeps_a_range(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    sweep_rows(cx, handle, 0, 1);
    cx.run_until_parked();
    assert_eq!(
        picked(cx, &view),
        vec![0, 1],
        "a drag across the boxes should pick up every row it passed"
    );

    // A drag that starts on a picked row puts the range back instead.
    sweep_rows(cx, handle, 0, 1);
    cx.run_until_parked();
    assert!(picked(cx, &view).is_empty());
}

#[gpui_kit::test]
fn a_modified_click_in_a_row_picks_it_out_away_from_the_box(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    // The platform's own modifier takes one row, anywhere in it.
    click_row(cx, handle, 0, Modifiers::secondary_key());
    cx.run_until_parked();
    assert_eq!(picked(cx, &view), vec![0]);

    // Shift takes the range from there.
    click_row(
        cx,
        handle,
        1,
        Modifiers {
            shift: true,
            ..Default::default()
        },
    );
    cx.run_until_parked();
    assert_eq!(picked(cx, &view), vec![0, 1]);
}

#[gpui_kit::test]
fn checking_a_row_leaves_the_keyboard_on_the_grid(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 0, 0);
    pick_row(cx, handle, 1);
    cx.run_until_parked();

    assert!(
        grid_focused(cx, handle, &view),
        "a box that took the focus would leave the next keystroke going nowhere"
    );
    assert_eq!(
        selection(cx, &view).0,
        Some(1),
        "checking a row should move the keyboard onto it"
    );

    // Which is what makes the keys above keep working after a click.
    press(cx, handle, "shift-up");
    cx.run_until_parked();
    assert_eq!(picked(cx, &view), vec![0, 1]);
}

#[gpui_kit::test]
fn the_menu_copies_the_cell_it_was_opened_on(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // The selection is on one cell and the menu on another: a right click
    // should copy what it landed on.
    select_cell(cx, &view, 0, 0);
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.copy_cell_for_test(0, 1, cx));
    cx.run_until_parked();

    assert_eq!(clipboard(cx), Some("alpha".to_string()));
}

#[gpui_kit::test]
fn closing_the_value_dialog_hands_the_keyboard_back_to_the_rows(cx: &mut TestAppContext) {
    let (_database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.downgrade()
        .update_in(cx, |grid, window, cx| {
            grid.focus_for_test(window, cx);
            grid.select_cell_for_test(0, 1, cx);
        })
        .unwrap();
    cx.run_until_parked();

    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    draw_workspace(cx, &handle);

    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();

    // The dialog took the keyboard; closing it has to give it back, or the
    // arrows answer nothing until the grid is clicked into again.
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("down", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.selection_for_test(cx)),
        (Some(1), Some((1, 1))),
        "the arrows should work straight after the dialog closes"
    );
}
