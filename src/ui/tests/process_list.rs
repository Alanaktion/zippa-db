//! The process list tab: Postgres and MySQL only, since SQLite has no server
//! to ask. `db::tests` covers `Connection::processes`/`kill_process` (and
//! the ignored live-server tests for the real thing); this file only covers
//! the SQLite gate, which is all a SQLite-backed test session can exercise.

use super::*;

#[gpui_kit::test]
fn a_sqlite_connection_never_opens_a_process_list_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let before = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();

    handle
        .update(cx, |session, window, cx| {
            session.open_process_list_for_test(window, cx)
        })
        .unwrap();

    let after = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();
    assert_eq!(
        after, before,
        "SQLite has no server to list processes for, so no tab should open"
    );
    assert!(
        handle
            .update(cx, |session, _, cx| session.active_process_list_view(cx))
            .unwrap()
            .is_none()
    );
}
