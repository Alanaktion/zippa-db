//! The History tab: what the user ran from query tabs, recorded by
//! `db::history` one store per connection.

use super::*;
use crate::db::history::{self, Keep};
use crate::ui::history::HistoryView;
use crate::ui::session::Tool;

/// Open the History tab and wait for it to read the store.
fn open_history(cx: &mut TestAppContext, handle: WindowHandle<Session>) -> Entity<HistoryView> {
    handle
        .update(cx, |session, window, cx| {
            session.open_tool(Tool::History, window, cx)
        })
        .unwrap();
    // The read happens on the background executor.
    cx.run_until_parked();
    handle
        .update(cx, |session, _, cx| session.active_history_view(cx))
        .unwrap()
        .expect("the active tab should be the history")
}

/// The session's connection: its id names its history store.
fn connection_of(cx: &mut TestAppContext, handle: WindowHandle<Session>) -> ConnectionConfig {
    handle
        .update(cx, |session, _, _| session.connection().config.clone())
        .unwrap()
}

fn shown_sql(cx: &mut TestAppContext, view: &Entity<HistoryView>, column: usize) -> Vec<String> {
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.read_with(cx, |grid, cx| grid.column_values_for_test(column, cx))
        .into_iter()
        .flatten()
        .collect()
}

#[gpui_kit::test]
fn a_run_is_kept_as_typed_without_the_implicit_limit(cx: &mut TestAppContext) {
    let _history = record_history(Keep::Disk);
    let (_database, handle) = session_with_objects(cx);

    run_sql(cx, handle, "select 1");
    run_sql(cx, handle, "select name from items");

    let view = open_history(cx, handle);
    // Newest first; the console shows the LIMIT the tab added, the history
    // keeps what was typed.
    assert_eq!(
        shown_sql(cx, &view, 2),
        ["select name from items", "select 1"]
    );

    let id = connection_of(cx, handle).id;
    let stored = history::entries(id);
    assert_eq!(stored.len(), 2, "both runs are on disk for this connection");
    assert!(
        history::history_file(id).expect("a path").exists(),
        "the connection has a file of its own"
    );
}

#[gpui_kit::test]
fn a_failed_run_is_kept_with_its_error(cx: &mut TestAppContext) {
    let _history = record_history(Keep::Disk);
    let (_database, handle) = session_with_objects(cx);

    run_sql(cx, handle, "select * from no_such_table");

    let view = open_history(cx, handle);
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    let result = grid.read_with(cx, |grid, cx| grid.column_values_for_test(3, cx));
    assert!(
        result[0]
            .as_deref()
            .is_some_and(|text| text.starts_with("Error: ")),
        "a failure says so in words, got {result:?}"
    );
}

#[gpui_kit::test]
fn each_statement_of_a_script_is_kept(cx: &mut TestAppContext) {
    let _history = record_history(Keep::Disk);
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "select 1;\nselect 2;", 0);
    press(cx, handle, "secondary-shift-enter");
    cx.run_until_parked();

    let view = open_history(cx, handle);
    assert_eq!(shown_sql(cx, &view, 2), ["select 2", "select 1"]);
}

#[gpui_kit::test]
fn the_app_s_own_reads_are_not_history(cx: &mut TestAppContext) {
    let _history = record_history(Keep::Disk);
    let (_database, handle, _view) = table_view(cx);
    cx.run_until_parked();

    let view = open_history(cx, handle);
    assert_eq!(view.read_with(cx, |view, _| view.entry_count()), 0);
}

#[gpui_kit::test]
fn with_history_off_nothing_is_recorded(cx: &mut TestAppContext) {
    let _history = record_history(Keep::Off);
    let (_database, handle) = session_with_objects(cx);

    run_sql(cx, handle, "select 1");

    let view = open_history(cx, handle);
    assert_eq!(view.read_with(cx, |view, _| view.entry_count()), 0);
    assert!(
        !history::history_file(connection_of(cx, handle).id)
            .expect("a path")
            .exists()
    );
}

#[gpui_kit::test]
fn in_memory_history_shows_but_writes_no_file(cx: &mut TestAppContext) {
    let _history = record_history(Keep::Memory);
    let (_database, handle) = session_with_objects(cx);

    run_sql(cx, handle, "select 1");

    let view = open_history(cx, handle);
    assert_eq!(shown_sql(cx, &view, 2), ["select 1"]);
    assert!(
        !history::history_file(connection_of(cx, handle).id)
            .expect("a path")
            .exists()
    );
}

#[gpui_kit::test]
fn an_open_tab_picks_up_a_new_run(cx: &mut TestAppContext) {
    let _history = record_history(Keep::Disk);
    let (_database, handle) = session_with_objects(cx);
    let view = open_history(cx, handle);
    assert_eq!(view.read_with(cx, |view, _| view.entry_count()), 0);

    handle
        .update(cx, |session, window, cx| session.new_query_tab(window, cx))
        .unwrap();
    run_sql(cx, handle, "select 42");

    assert_eq!(shown_sql(cx, &view, 2), ["select 42"]);
}

#[gpui_kit::test]
fn all_connections_merges_every_store(cx: &mut TestAppContext) {
    let _history = record_history(Keep::Disk);
    let (_database, handle) = session_with_objects(cx);
    // Another connection's statement, written to its own store.
    let other = Uuid::new_v4();
    history::record(history::HistoryEntry::new(
        other,
        "elsewhere",
        "select 'other'",
        crate::db::QueryOutcome::Rows(1),
        std::time::Duration::ZERO,
    ));
    run_sql(cx, handle, "select 'mine'");

    let view = open_history(cx, handle);
    assert_eq!(shown_sql(cx, &view, 2), ["select 'mine'"]);

    view.update(cx, |view, cx| view.set_all_connections_for_test(true, cx));
    cx.run_until_parked();
    assert_eq!(shown_sql(cx, &view, 3), ["select 'mine'", "select 'other'"]);
    let connections = shown_sql(cx, &view, 1);
    assert_eq!(connections[0], connection_of(cx, handle).display_name());
    assert_eq!(connections[1], "Deleted or unsaved connection");
}

#[gpui_kit::test]
fn open_puts_the_statement_in_a_new_query_tab(cx: &mut TestAppContext) {
    let _history = record_history(Keep::Disk);
    let (_database, handle) = session_with_objects(cx);
    run_sql(cx, handle, "select 'again'");
    let view = open_history(cx, handle);
    let tabs = handle
        .update(cx, |session, _, cx| session.tab_titles(cx).len())
        .unwrap();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.downgrade()
        .update_in(cx, |grid, window, cx| {
            grid.focus_for_test(window, cx);
            grid.select_cell_for_test(0, 2, cx);
        })
        .unwrap();
    cx.run_until_parked();
    click_in_session(cx, handle, "history-open");

    let (count, sql) = handle
        .update(cx, |session, _, cx| {
            (session.tab_titles(cx).len(), session.active_sql(cx))
        })
        .unwrap();
    assert_eq!(count, tabs + 1, "a new query tab opens");
    assert_eq!(sql, "select 'again'");
}

#[gpui_kit::test]
fn clearing_forgets_this_connection_only(cx: &mut TestAppContext) {
    let _history = record_history(Keep::Disk);
    let (_database, handle) = session_with_objects(cx);
    let other = Uuid::new_v4();
    history::record(history::HistoryEntry::new(
        other,
        "elsewhere",
        "select 'other'",
        crate::db::QueryOutcome::Rows(1),
        std::time::Duration::ZERO,
    ));
    run_sql(cx, handle, "select 1");

    let view = open_history(cx, handle);
    let id = connection_of(cx, handle).id;
    view.update(cx, |view, cx| view.clear(Some(id), cx));
    cx.run_until_parked();

    assert_eq!(view.read_with(cx, |view, _| view.entry_count()), 0);
    assert!(history::entries(id).is_empty());
    assert_eq!(history::entries(other).len(), 1);
}

#[gpui_kit::test]
fn a_restored_history_tab_reopens(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    open_history(cx, handle);
    draw(cx, handle);
    assert_eq!(Tool::from_state(&PanelState::History), Some(Tool::History));
}
