//! The console tab: every statement this connection has sent, read from
//! `Connection::query_log`.

use super::*;

#[gpui_kit::test]
fn a_user_run_shows_up_in_the_console(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "select 1", 8);
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    handle
        .update(cx, |session, window, cx| {
            session.open_console_for_test(window, cx)
        })
        .unwrap();

    let console = handle
        .update(cx, |session, _, cx| session.active_console_view(cx))
        .unwrap()
        .expect("the active tab should be the console");
    let grid = console.read_with(cx, |console, _| console.grid_for_test());

    // Newest first, so the run just sent is row 0.
    let sql = grid.read_with(cx, |grid, cx| grid.column_values_for_test(3, cx));
    let source = grid.read_with(cx, |grid, cx| grid.column_values_for_test(1, cx));
    assert_eq!(sql[0].as_deref(), Some("select 1"));
    assert_eq!(source[0].as_deref(), Some("User"));
}

#[gpui_kit::test]
fn opening_a_table_logs_its_reads_as_internal(cx: &mut TestAppContext) {
    let (_database, handle, _view) = table_view(cx);
    cx.run_until_parked();

    handle
        .update(cx, |session, window, cx| {
            session.open_console_for_test(window, cx)
        })
        .unwrap();

    let console = handle
        .update(cx, |session, _, cx| session.active_console_view(cx))
        .unwrap()
        .expect("the active tab should be the console");
    let entries = console.read_with(cx, |console, _| console.entry_count());
    assert!(entries > 0, "opening a table sends its own reads");

    let grid = console.read_with(cx, |console, _| console.grid_for_test());
    let source = grid.read_with(cx, |grid, cx| grid.column_values_for_test(1, cx));
    assert!(
        source
            .iter()
            .any(|value| value.as_deref() == Some("Internal")),
        "the table's own reads are not the user's own SQL"
    );
}

#[gpui_kit::test]
fn opening_the_console_twice_brings_the_same_tab_forward(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let before = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();

    handle
        .update(cx, |session, window, cx| {
            session.open_console_for_test(window, cx);
            session.open_console_for_test(window, cx);
        })
        .unwrap();

    let after = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();
    assert_eq!(
        after.len(),
        before.len() + 1,
        "the console should open once, not twice"
    );
}

#[gpui_kit::test]
fn the_clear_button_empties_the_log(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "select 1", 8);
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    handle
        .update(cx, |session, window, cx| {
            session.open_console_for_test(window, cx)
        })
        .unwrap();

    let console = handle
        .update(cx, |session, _, cx| session.active_console_view(cx))
        .unwrap()
        .expect("the active tab should be the console");
    assert!(console.read_with(cx, |console, _| console.entry_count()) > 0);

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("clear-console", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert_eq!(console.read_with(cx, |console, _| console.entry_count()), 0);
}
