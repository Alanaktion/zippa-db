//! The sidebar's Saved tab: opening a saved query in a new tab, and
//! deleting one.

use super::*;
use crate::saved_queries;

/// A session whose Saved tab is showing, with one query saved under `name`.
fn saved_session(
    cx: &mut TestAppContext,
    name: &str,
    sql: &str,
) -> (TempDatabase, WindowHandle<Session>) {
    saved_queries::upsert(name, sql).expect("saving the test query should succeed");
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
    let _lock = saved_queries::FILE_LOCK.lock().unwrap();
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

    saved_queries::remove("All users").expect("test cleanup should succeed");
}

#[gpui_kit::test]
fn deleting_a_saved_query_forgets_it(cx: &mut TestAppContext) {
    let _lock = saved_queries::FILE_LOCK.lock().unwrap();
    let (_database, handle) = saved_session(cx, "Temporary", "SELECT 1;");
    assert_eq!(saved_queries::load().len(), 1);
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
        saved_queries::load().is_empty(),
        "the deleted query should be gone"
    );
    let tabs_after = handle
        .read_with(cx, |session, cx| session.tab_titles(cx))
        .unwrap()
        .len();
    assert_eq!(
        tabs_after, tabs_before,
        "deleting a saved query must not open it in a new tab"
    );
}
