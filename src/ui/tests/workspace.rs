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
fn the_shortcut_opens_a_connection_tab_from_an_untouched_window(cx: &mut TestAppContext) {
    let handle = workspace(cx);

    // Nothing has been clicked yet, so the keystroke has only the window's own
    // root to travel through.
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("secondary-n", cx);
    })
    .unwrap();

    assert_eq!(
        titles(cx, &handle),
        ["New connection", "New connection"],
        "the new-connection shortcut should work before anything has focus"
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

    click(cx, &handle, "close-connection-0");

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

    click(cx, &handle, "close-connection-0");
    assert_eq!(
        titles(cx, &handle),
        [first.config().display_name()],
        "a connection with unsaved changes should not close until the question is answered"
    );

    // Saying no leaves the tab open, still connected.
    click(cx, &handle, "cancel");
    assert_eq!(titles(cx, &handle), [first.config().display_name()]);

    // Saying yes closes it despite the unsaved changes.
    click(cx, &handle, "close-connection-0");
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
fn double_clicking_a_saved_connection_opens_it(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let (_database, id) = saved_connection(cx, &handle);
    let target = gpui_kit::SharedString::from(format!("open-{id}"));

    // One click only fills the form in, so the tab is still the manager.
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(target.clone(), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        handle
            .update(cx, |workspace, _, _| workspace
                .active_welcome_for_test()
                .is_some())
            .unwrap(),
        "a single click should not connect"
    );

    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.double_click(target.clone(), cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        handle
            .update(cx, |workspace, _, _| workspace
                .active_session_for_test()
                .is_some())
            .unwrap(),
        "a double click should open the connection"
    );
}

#[gpui_kit::test]
fn deleting_a_saved_connection_asks_first(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let (_database, id) = saved_connection(cx, &handle);

    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be the connection manager");

    click_workspace(
        cx,
        &handle,
        gpui_kit::SharedString::from(format!("delete-{id}")),
    );

    assert!(
        workspace_dialog_open(cx, &handle),
        "deleting a saved connection should be asked about"
    );
    assert_eq!(
        welcome
            .read_with(cx, |welcome, _| welcome.saved_names_for_test())
            .len(),
        1,
        "nothing should be deleted while the question is up"
    );

    // Keeping it leaves the connection and its password alone.
    click_workspace(cx, &handle, "cancel");
    assert_eq!(
        welcome
            .read_with(cx, |welcome, _| welcome.saved_names_for_test())
            .len(),
        1
    );
    assert!(!workspace_dialog_open(cx, &handle));
}
