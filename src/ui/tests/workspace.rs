//! Connections in tabs of their own, and the toolbar over them.

use super::*;

#[gpui_kit::test]
fn each_connection_opens_in_a_tab_of_its_own(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    assert_eq!(
        titles(cx, &handle),
        ["New connection"],
        "the window should start on the connection manager"
    );

    let first = connect(cx, &handle);
    assert_eq!(
        titles(cx, &handle),
        [first.config().display_name()],
        "connecting should turn the tab it was opened from into the session"
    );

    click(cx, &handle, "new-connection");
    assert_eq!(
        titles(cx, &handle),
        [first.config().display_name(), "New connection".to_string()]
    );
    assert_eq!(active(cx, &handle), 1, "a new tab comes to the front");

    let second = connect(cx, &handle);
    assert_eq!(
        titles(cx, &handle),
        [
            first.config().display_name(),
            second.config().display_name()
        ],
        "both connections should stay open"
    );
}

#[gpui_kit::test]
fn the_connection_bar_only_shows_with_more_than_one_tab(cx: &mut TestAppContext) {
    let handle = workspace(cx);

    let bar = |cx: &mut TestAppContext, expect: bool| {
        let found = cx
            .update_window(handle.window.into(), |_, window, cx| {
                window.render_frame(cx);
                window.try_find("connection-bar").is_some()
            })
            .unwrap();
        assert_eq!(found, expect, "the connection bar's visibility");
    };

    // A single connection — the manager included — needs no strip.
    bar(cx, false);

    let _database = connect(cx, &handle);
    bar(cx, false);

    click(cx, &handle, "new-connection");
    bar(cx, true);
}

#[gpui_kit::test]
fn the_shortcut_opens_the_editor_on_the_welcome_screen(cx: &mut TestAppContext) {
    let handle = workspace(cx);

    // Nothing has been clicked yet, so the keystroke has only the launcher's
    // own context to travel through.
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("secondary-n", cx);
    })
    .unwrap();

    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");

    assert!(
        welcome.update(cx, |welcome, _| welcome.editor_open_for_test()),
        "the new-connection shortcut should open the editor on the welcome screen"
    );
    assert_eq!(
        titles(cx, &handle),
        ["New connection"],
        "opening the editor should not add another connection tab"
    );
}

#[gpui_kit::test]
fn switching_connections_leaves_each_session_where_it_was(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _first = connect(cx, &handle);

    let sql_of = |cx: &mut TestAppContext| {
        let session = handle
            .update(cx, |workspace, _, _| workspace.active_session_for_test())
            .unwrap()
            .expect("the active tab should be a session");
        session.update(cx, |session, cx| session.active_sql(cx))
    };
    let type_sql = |cx: &mut TestAppContext, sql: &str| {
        let session = handle
            .update(cx, |workspace, _, _| workspace.active_session_for_test())
            .unwrap()
            .expect("the active tab should be a session");
        handle
            .update(cx, |_, window, cx| {
                session.update(cx, |session, cx| {
                    session.prepare_active_editor_for_test(sql, window, cx)
                })
            })
            .unwrap();
    };

    type_sql(cx, "select 1");

    click(cx, &handle, "new-connection");
    let _second = connect(cx, &handle);
    type_sql(cx, "select 2");

    // Back one tab, and the first session is as it was left.
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("secondary-shift-[", cx);
    })
    .unwrap();

    assert_eq!(
        active(cx, &handle),
        0,
        "the shortcut should step back a tab"
    );
    assert_eq!(sql_of(cx), "select 1");

    cx.update_window(handle.window.into(), |_, window, cx| {
        window.press("secondary-shift-]", cx);
    })
    .unwrap();

    assert_eq!(active(cx, &handle), 1);
    assert_eq!(sql_of(cx), "select 2");
}

#[gpui_kit::test]
fn closing_a_connection_tab_leaves_the_others(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _first = connect(cx, &handle);
    click(cx, &handle, "new-connection");
    let second = connect(cx, &handle);

    click(cx, &handle, "close-connection-0");

    assert_eq!(
        titles(cx, &handle),
        [second.config().display_name()],
        "closing one connection should leave the other open"
    );
    assert_eq!(active(cx, &handle), 0);
}

#[gpui_kit::test]
fn closing_the_last_connection_leaves_the_manager(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    // A lone connection has no strip to close it from, so the shortcut (and
    // the menu item behind it) is the way out.
    press_workspace(cx, &handle, "secondary-shift-w");

    assert_eq!(
        titles(cx, &handle),
        ["New connection"],
        "the window always has a tab to open a connection from"
    );
}

#[gpui_kit::test]
fn closing_a_connection_tab_with_unsaved_changes_asks_first(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let first = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the active tab should be a session");
    handle
        .update(cx, |_, window, cx| {
            session.update(cx, |session, cx| {
                session.prepare_active_editor_for_test("select 1", window, cx)
            })
        })
        .unwrap();

    press_workspace(cx, &handle, "secondary-shift-w");
    assert_eq!(
        titles(cx, &handle),
        [first.config().display_name()],
        "a connection with unsaved changes should not close until the question is answered"
    );

    // Saying no leaves the tab open, still connected.
    click(cx, &handle, "cancel");
    assert_eq!(titles(cx, &handle), [first.config().display_name()]);

    // Saying yes closes it despite the unsaved changes.
    press_workspace(cx, &handle, "secondary-shift-w");
    click(cx, &handle, "ok");
    assert_eq!(
        titles(cx, &handle),
        ["New connection"],
        "confirming should close the connection"
    );
}

#[gpui_kit::test]
fn disconnecting_returns_the_tab_to_the_manager(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let first = connect(cx, &handle);
    click(cx, &handle, "new-connection");
    let _second = connect(cx, &handle);

    click(cx, &handle, "disconnect");
    cx.run_until_parked();

    assert_eq!(
        titles(cx, &handle),
        [first.config().display_name(), "New connection".to_string()],
        "disconnecting should keep the tab and show the manager in it"
    );
    assert_eq!(
        active(cx, &handle),
        1,
        "the tab that was disconnected stays in front"
    );
}

#[gpui_kit::test]
fn disconnecting_with_unsaved_changes_asks_first(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let first = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the active tab should be a session");
    handle
        .update(cx, |_, window, cx| {
            session.update(cx, |session, cx| {
                session.prepare_active_editor_for_test("select 1", window, cx)
            })
        })
        .unwrap();

    click(cx, &handle, "disconnect");
    assert_eq!(
        titles(cx, &handle),
        [first.config().display_name()],
        "a connection with unsaved changes should not disconnect until the question is answered"
    );

    // Saying no leaves it connected.
    click(cx, &handle, "cancel");
    assert_eq!(titles(cx, &handle), [first.config().display_name()]);
    cx.run_until_parked();

    // Saying yes disconnects it, keeping the tab for the next connection.
    click(cx, &handle, "disconnect");
    click(cx, &handle, "ok");
    cx.run_until_parked();
    assert_eq!(
        titles(cx, &handle),
        ["New connection"],
        "confirming should disconnect the connection"
    );
}

#[gpui_kit::test]
fn the_toolbar_buttons_are_disabled_without_a_connection(cx: &mut TestAppContext) {
    let handle = workspace(cx);

    // Disabled buttons still register a click; only their effect proves the
    // disabled state, since gpui-component's Button does not surface it to
    // the accessibility tree that the test harness reads.
    click(cx, &handle, "refresh");
    click(cx, &handle, "new-query");

    assert_eq!(
        titles(cx, &handle),
        ["New connection"],
        "refresh and new-query should have no session to act on"
    );
}

#[gpui_kit::test]
fn the_toolbar_new_query_button_opens_a_tab(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the active tab should be a session");
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 1"]
    );

    click(cx, &handle, "new-query");

    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 1", "Query 2"],
        "the toolbar button should behave like the session's own new-tab button"
    );
}

#[gpui_kit::test]
fn many_connection_tabs_scroll_inside_the_bar(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);
    for _ in 0..20 {
        click(cx, &handle, "new-connection");
    }

    // The outer bar wraps every screen, so a connection tab strip that grew
    // with its tabs would push the session — and the toolbar above it — out of
    // the window.
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.render_frame(cx);
        let viewport = Bounds {
            origin: Default::default(),
            size: size(px(WINDOW.0), px(WINDOW.1)),
        };
        for id in ["new-connection", "welcome-new-connection"] {
            let found = window.find(id);
            assert!(
                contains(viewport, found.bounds()),
                "{id} ran outside the window with 21 connection tabs open: {:?}",
                found.bounds()
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn restored_connections_reopen_their_tabs_in_order(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let config = database.config();
    let state = WorkspaceState {
        active: 0,
        sessions: vec![SessionState {
            connection: config.id,
            database: Some(config.database.clone()),
            active: 1,
            panels: vec![
                PanelState::Query {
                    title: "Query 1".into(),
                    sql: "select 1".into(),
                    file: None,
                },
                PanelState::Query {
                    title: "Query 2".into(),
                    sql: "select 2".into(),
                    file: None,
                },
            ],
        }],
        ..WorkspaceState::default()
    };

    let (_dir, handle) = restored_workspace(cx, state, vec![config.clone()]);
    cx.run_until_parked();

    assert_eq!(
        titles(cx, &handle),
        [config.display_name()],
        "the saved connection should reopen as a session tab"
    );
    assert_eq!(active(cx, &handle), 0);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the restored connection should have become a session");
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 1", "Query 2"],
        "the tabs should come back in order, with no leftover empty editor"
    );
    assert_eq!(
        session.read_with(cx, |session, cx| session.active_sql(cx)),
        "select 2",
        "the saved active tab should be the one showing"
    );
}

#[gpui_kit::test]
fn a_restored_file_tab_reads_its_file_to_decide_dirty(cx: &mut TestAppContext) {
    let file_dir = ScratchDir::new();
    let file = file_dir.file("report.sql", "select 1;\n");

    let database = runtime::block_on(TempDatabase::new());
    let config = database.config();
    let state = WorkspaceState {
        sessions: vec![SessionState {
            connection: config.id,
            database: Some(config.database.clone()),
            active: 0,
            panels: vec![PanelState::Query {
                title: "report.sql".into(),
                // The saved buffer differs from what is on disk, so the tab is
                // unsaved work once the file has been read back.
                sql: "select 2;\n".into(),
                file: Some(file),
            }],
        }],
        ..WorkspaceState::default()
    };

    let (_config_dir, handle) = restored_workspace(cx, state, vec![config]);
    cx.run_until_parked();

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the restored connection should have become a session");
    assert!(
        session.read_with(cx, |session, cx| session.tab_is_dirty_for_test(0, cx)),
        "a restored buffer that differs from its file should show as unsaved"
    );
}

#[gpui_kit::test]
fn a_restored_table_tab_replaces_the_empty_editor(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let config = database.config();
    let state = WorkspaceState {
        sessions: vec![SessionState {
            connection: config.id,
            database: Some(config.database.clone()),
            active: 0,
            panels: vec![PanelState::Table {
                object: DatabaseObject {
                    schema: None,
                    name: "items".into(),
                    kind: ObjectKind::Table,
                },
            }],
        }],
        ..WorkspaceState::default()
    };

    let (_dir, handle) = restored_workspace(cx, state, vec![config]);
    cx.run_until_parked();

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the restored connection should have become a session");
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["items"],
        "a restored table should not leave the empty editor beside it"
    );
}

#[gpui_kit::test]
fn a_restore_with_no_tabs_keeps_the_empty_editor(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let config = database.config();
    let state = WorkspaceState {
        sessions: vec![SessionState {
            connection: config.id,
            database: Some(config.database.clone()),
            active: 0,
            panels: Vec::new(),
        }],
        ..WorkspaceState::default()
    };

    let (_dir, handle) = restored_workspace(cx, state, vec![config]);
    cx.run_until_parked();

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the restored connection should have become a session");
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 1"],
        "a session always shows one editor"
    );
}

#[gpui_kit::test]
fn a_connection_deleted_since_the_last_run_is_skipped(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let config = database.config();
    let state = WorkspaceState {
        sessions: vec![
            SessionState {
                connection: config.id,
                database: Some(config.database.clone()),
                active: 0,
                panels: vec![PanelState::Query {
                    title: "Query 1".into(),
                    sql: "select 1".into(),
                    file: None,
                }],
            },
            SessionState {
                connection: Uuid::new_v4(),
                database: None,
                active: 0,
                panels: vec![PanelState::Query {
                    title: "Query 1".into(),
                    sql: "select 2".into(),
                    file: None,
                }],
            },
        ],
        ..WorkspaceState::default()
    };

    let (_dir, handle) = restored_workspace(cx, state, vec![config.clone()]);
    cx.run_until_parked();

    assert_eq!(
        titles(cx, &handle),
        [config.display_name()],
        "a connection that no longer exists should not get a tab"
    );
}

#[gpui_kit::test]
fn a_failed_reconnect_keeps_its_tabs_for_retrying(cx: &mut TestAppContext) {
    // A SQLite file that is not there: `open` refuses to create it, so the
    // connection fails the way a server being down would.
    let mut config = ConnectionConfig::new(Engine::Sqlite);
    config.database = std::env::temp_dir()
        .join(format!("zippa-missing-{}.sqlite", Uuid::new_v4()))
        .to_string_lossy()
        .into_owned();
    let state = WorkspaceState {
        sessions: vec![SessionState {
            connection: config.id,
            database: Some(config.database.clone()),
            active: 0,
            panels: vec![PanelState::Query {
                title: "Query 1".into(),
                sql: "select 1".into(),
                file: None,
            }],
        }],
        ..WorkspaceState::default()
    };

    let (_dir, handle) = restored_workspace(cx, state.clone(), vec![config]);
    cx.run_until_parked();

    assert_eq!(
        titles(cx, &handle),
        ["New connection"],
        "a connection that cannot be reopened stays on the launcher"
    );
    assert_eq!(
        handle
            .view
            .read_with(cx, |workspace, cx| workspace.state_for_test(cx)),
        state,
        "the tabs should still be recorded while the connect is outstanding"
    );
}

#[gpui_kit::test]
fn a_restored_state_round_trips_through_snapshot(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let config = database.config();
    let state = WorkspaceState {
        active: 0,
        sessions: vec![SessionState {
            connection: config.id,
            database: Some(config.database.clone()),
            active: 1,
            panels: vec![
                PanelState::Query {
                    title: "Query 1".into(),
                    sql: "select 1".into(),
                    file: None,
                },
                PanelState::Table {
                    object: DatabaseObject {
                        schema: None,
                        name: "items".into(),
                        kind: ObjectKind::Table,
                    },
                },
            ],
        }],
        ..WorkspaceState::default()
    };

    let (_dir, handle) = restored_workspace(cx, state.clone(), vec![config]);
    cx.run_until_parked();

    let round_tripped = handle
        .view
        .read_with(cx, |workspace, cx| workspace.state_for_test(cx));
    assert_eq!(
        round_tripped, state,
        "a restored window should snapshot back to the state it came from"
    );
}

#[gpui_kit::test]
fn clicking_a_saved_connection_opens_it(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let (_database, id) = saved_connection(cx, &handle);
    let target = gpui_kit::SharedString::from(format!("connect-{id}"));

    // A card is the connect target now, so a single click opens it.
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(target.clone(), cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        handle
            .update(cx, |workspace, _, _| workspace
                .active_session_for_test()
                .is_some())
            .unwrap(),
        "a click on the card should open the connection"
    );
}
