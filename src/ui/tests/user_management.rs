//! The user management tab: Postgres and MySQL only, since SQLite has no
//! users. `db::tests` covers `Connection::users`/`create_user`/`drop_user`/
//! `set_user_password` (and the ignored live-server tests for the real
//! thing); this file only covers the SQLite gate, which is all a
//! SQLite-backed test session can exercise.

use super::*;

#[gpui_kit::test]
fn a_sqlite_connection_never_opens_a_user_management_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let before = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();

    handle
        .update(cx, |session, window, cx| {
            session.open_user_management_for_test(window, cx)
        })
        .unwrap();

    let after = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();
    assert_eq!(
        after, before,
        "SQLite has no users to manage, so no tab should open"
    );
    assert!(
        handle
            .update(cx, |session, _, cx| session.active_user_management_view(cx))
            .unwrap()
            .is_none()
    );
}
