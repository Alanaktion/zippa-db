//! The sidebar's Saved tab: opening a saved query in a new tab, and
//! deleting one.

use super::*;
use crate::saved_queries::{self, SavedQueries};

/// A session whose Saved tab is showing, with one query saved under `name`.
fn saved_session(
    cx: &mut TestAppContext,
    name: &str,
    sql: &str,
) -> (TempDatabase, WindowHandle<Session>) {
    saved_queries::reset_for_test();
    cx.update(|cx| saved_queries::upsert(name, sql, cx));
    cx.run_until_parked();
    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");
    cx.update(init_ui);
    let handle = {
        let connection = Arc::new(connection);
        cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
            Session::new(connection, window, cx)
        })
    };
    handle
        .update(cx, |session, _, cx| {
            session.show_saved_tab_for_test(cx);
        })
        .unwrap();
    (database, handle)
}

#[gpui_kit::test]
fn opening_a_saved_query_starts_a_new_tab_with_its_sql(cx: &mut TestAppContext) {
    let _lock = saved_queries::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = saved_session(cx, "All users", "SELECT * FROM users;");
    let before = handle
        .read_with(cx, |session, cx| session.tab_titles(cx))
        .unwrap()
        .len();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(("saved-query", 0usize), cx);
    })
    .unwrap();
    cx.run_until_parked();

    let titles = handle
        .read_with(cx, |session, cx| session.tab_titles(cx))
        .unwrap();
    assert_eq!(titles.len(), before + 1, "titles: {titles:?}");
    assert!(
        titles.iter().any(|title| title == "All users"),
        "titles: {titles:?}"
    );
    let sql = handle
        .update(cx, |session, _, cx| {
            session
                .active_editor_for_test(cx)
                .expect("the new tab has an editor")
                .read(cx)
                .sql(cx)
        })
        .unwrap();
    assert_eq!(sql, "SELECT * FROM users;");

    saved_queries::reset_for_test();
}

#[gpui_kit::test]
fn deleting_a_saved_query_forgets_it(cx: &mut TestAppContext) {
    let _lock = saved_queries::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = saved_session(cx, "Temporary", "SELECT 1;");
    assert_eq!(cx.update(|cx| SavedQueries::get(cx).len()), 1);
    let tabs_before = handle
        .read_with(cx, |session, cx| session.tab_titles(cx))
        .unwrap()
        .len();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(("saved-delete", 0usize), cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        cx.update(|cx| SavedQueries::get(cx).is_empty()),
        "the deleted query should be gone"
    );
    assert!(
        saved_queries::load()
            .expect("the file should read back")
            .is_empty(),
        "the deletion should reach the file too"
    );
    let tabs_after = handle
        .read_with(cx, |session, cx| session.tab_titles(cx))
        .unwrap()
        .len();
    assert_eq!(
        tabs_after, tabs_before,
        "deleting a saved query must not open it in a new tab"
    );
    saved_queries::reset_for_test();
}

/// Open the saved query in row 0 by clicking it, the way the Saved tab does.
fn open_first_saved(cx: &mut TestAppContext, handle: WindowHandle<Session>) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(("saved-query", 0usize), cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn saving_a_tab_opened_from_a_saved_query_updates_it_in_place(cx: &mut TestAppContext) {
    let _lock = saved_queries::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = saved_session(cx, "Report", "SELECT 1;");
    open_first_saved(cx, handle);

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("SELECT 2;", window, cx);
            window.dispatch_action(Box::new(crate::ui::session::SaveQuery), cx);
        })
        .unwrap();
    cx.run_until_parked();

    // No name to answer: the tab already belongs to "Report". (This window
    // has no dialog layer, so a dialog opening would have failed the test.)
    let saved = cx.update(|cx| SavedQueries::get(cx).to_vec());
    assert_eq!(saved.len(), 1, "updating should not add a second query");
    assert_eq!(saved[0].name, "Report");
    assert_eq!(saved[0].sql, "SELECT 2;");

    saved_queries::reset_for_test();
}

#[gpui_kit::test]
fn opening_a_saved_query_twice_brings_its_tab_back(cx: &mut TestAppContext) {
    let _lock = saved_queries::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = saved_session(cx, "Report", "SELECT 1;");
    open_first_saved(cx, handle);
    let after_first = handle
        .read_with(cx, |session, cx| session.tab_titles(cx))
        .unwrap();
    open_first_saved(cx, handle);
    let after_second = handle
        .read_with(cx, |session, cx| session.tab_titles(cx))
        .unwrap();
    assert_eq!(
        after_first, after_second,
        "the second click should not open a copy"
    );

    saved_queries::reset_for_test();
}

#[gpui_kit::test]
fn enter_in_the_save_dialog_saves_and_ties_the_tab(cx: &mut TestAppContext) {
    let _lock = saved_queries::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    saved_queries::reset_for_test();
    let (_database, handle, session) = workspace_session(cx);

    prepare_workspace_editor(cx, &handle, &session, "SELECT 3;");
    press_workspace(cx, &handle, "secondary-alt-s");
    assert!(
        workspace_dialog_open(cx, &handle),
        "a query that belongs to no saved one should ask for a name"
    );
    // The name box has the keyboard; an untitled tab suggests no name.
    for key in ["t", "h", "i", "r", "d", "s"] {
        press_workspace(cx, &handle, key);
    }
    press_workspace(cx, &handle, "enter");

    assert!(
        !workspace_dialog_open(cx, &handle),
        "Enter should save and close"
    );
    let saved = cx.update(|cx| SavedQueries::get(cx).to_vec());
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].name, "thirds");

    // The tab now belongs to it, so the next save updates it in place.
    prepare_workspace_editor(cx, &handle, &session, "SELECT 4;");
    press_workspace(cx, &handle, "secondary-alt-s");
    assert!(!workspace_dialog_open(cx, &handle));
    let saved = cx.update(|cx| SavedQueries::get(cx).to_vec());
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].sql, "SELECT 4;");

    saved_queries::reset_for_test();
}
