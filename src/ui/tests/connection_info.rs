//! The connection information tab: offered on every engine, since SQLite's
//! facts are its file and its library version rather than a server to ask.
//! `db::tests` covers `Connection::connection_info` (and the ignored
//! live-server tests for the real thing); this file covers the tab opening
//! once, being reused rather than duplicated, and loading the SQLite facts.

use super::*;
use crate::db::Engine;
use crate::ui::session::Tool;

#[gpui_kit::test]
fn the_connection_info_tab_opens_once_and_loads_the_sqlite_facts(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    for _ in 0..2 {
        handle
            .update(cx, |session, window, cx| {
                session.open_tool(Tool::ConnectionInfo, window, cx)
            })
            .unwrap();
    }
    cx.run_until_parked();

    let titles = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();
    assert_eq!(
        titles
            .iter()
            .filter(|title| *title == "Connection Info")
            .count(),
        1,
        "opening the tool twice brings its one tab forward rather than opening a second"
    );

    let view = handle
        .update(cx, |session, _, cx| session.active_connection_info_view(cx))
        .unwrap()
        .expect("the active tab should be the connection information tab");
    let info = view.read_with(cx, |view, _| view.info_for_test());
    let info = info.expect("the panel should have loaded the connection's facts");
    assert_eq!(info.engine, Engine::Sqlite);
    assert!(
        !info.version.is_empty(),
        "SQLite reports its library version"
    );
    assert!(
        info.size_bytes.is_some_and(|size| size > 0),
        "the test database file has a size"
    );
    assert_eq!(
        info.database, None,
        "SQLite's database is its file, shown as the File row instead"
    );
    assert_eq!(info.uptime_secs, None);
    assert_eq!(info.connections, None);
}
