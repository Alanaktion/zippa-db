//! The user management tab: Postgres and MySQL only, since SQLite has no
//! users. `db::tests` covers `Connection::users`/`create_user`/`drop_user`/
//! `set_user_password` (and the ignored live-server tests for the real
//! thing); this file only covers the SQLite gate, which is all a
//! SQLite-backed test session can exercise.

use super::*;
use crate::ui::session::Tool;

#[gpui_kit::test]
fn a_sqlite_connection_never_opens_a_user_management_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let before = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();

    handle
        .update(cx, |session, window, cx| {
            session.open_tool(Tool::Users, window, cx)
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

/// Escape gives up on the new-user dialog with the keyboard still in the
/// name box.
#[gpui_kit::test]
fn escape_closes_the_user_dialog(cx: &mut TestAppContext) {
    use crate::ui::user_management::{UserDialogEvent, UserDialogView};

    let database = runtime::block_on(TempDatabase::new());
    let connection = Arc::new(
        runtime::block_on(Connection::open(database.config(), None))
            .expect("could not open the test database"),
    );
    assert!(escape_dismisses(
        cx,
        |window, cx| UserDialogView::create_for_test(connection, window, cx),
        |view, window, cx| view.focus(window, cx),
        |event| matches!(event, UserDialogEvent::Dismissed),
    ));
}
