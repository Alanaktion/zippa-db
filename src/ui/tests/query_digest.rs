//! The query digest tab: Postgres and MySQL only, since SQLite is an
//! embedded engine with no query instrumentation to read this way.
//! `db::tests` covers `Connection::query_digest` (and the ignored
//! live-server tests for the real thing, including the "instrumentation is
//! off" path). This file only covers the SQLite gate, which is all a
//! SQLite-backed test session can exercise.

use super::*;

#[gpui_kit::test]
fn a_sqlite_connection_never_opens_a_query_digest_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let before = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();

    handle
        .update(cx, |session, window, cx| {
            session.open_query_digest_for_test(window, cx)
        })
        .unwrap();

    let after = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();
    assert_eq!(
        after, before,
        "SQLite has no query instrumentation to read, so no tab should open"
    );
    assert!(
        handle
            .update(cx, |session, _, cx| session.active_query_digest_view(cx))
            .unwrap()
            .is_none()
    );
}
