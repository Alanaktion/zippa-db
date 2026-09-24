//! The server variables tab: Postgres and MySQL only, since SQLite is an
//! embedded engine with no server-side configuration to show. `db::tests`
//! covers `Connection::server_variables` (and the ignored live-server test
//! for the real thing); `ui::server_variables`'s own `#[cfg(test)]` module
//! covers the client-side name filter and the "changed only" toggle. This
//! file only covers the SQLite gate, which is all a SQLite-backed test
//! session can exercise.

use super::*;

#[gpui_kit::test]
fn a_sqlite_connection_never_opens_a_server_variables_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let before = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();

    handle
        .update(cx, |session, window, cx| {
            session.open_server_variables_for_test(window, cx)
        })
        .unwrap();

    let after = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();
    assert_eq!(
        after, before,
        "SQLite has no server-side configuration to list, so no tab should open"
    );
    assert!(
        handle
            .update(cx, |session, _, cx| session
                .active_server_variables_view(cx))
            .unwrap()
            .is_none()
    );
}
