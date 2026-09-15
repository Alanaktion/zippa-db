//! One open connection: its tabs, its sidebar, and its object list.

use super::*;

#[gpui_kit::test]
fn switching_database_is_refused_for_sqlite(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");
    let path = connection.database().to_string();

    cx.update(gpui_kit::component::init);
    let handle = {
        let connection = Arc::new(connection);
        cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
            Session::new(connection, window, cx)
        })
    };

    handle
        .update(cx, |session, _, cx| {
            // "main" is what SQLite reports; treating it as a file would break
            // the session.
            session.switch_database("main".to_string(), cx);
            assert_eq!(session.connection().database(), path);
        })
        .unwrap();
}

#[gpui_kit::test]
fn clicking_a_table_opens_a_table_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, _| {
            assert_eq!(session.tab_titles(), ["Query 1"]);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-items", cx);
    })
    .unwrap();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(session.tab_titles(), ["Query 1", "items"]);
            assert!(
                session.active_table_view().is_some(),
                "clicking a table should open a table view, not an editor"
            );
            assert_eq!(
                session.active_sql(cx),
                "select * from items limit 500 offset 0",
                "the table view should read the first page of the table"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn opening_the_same_table_twice_focuses_the_open_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-items", cx);
    })
    .unwrap();

    // A second query tab so the table tab is not simply the last one.
    handle
        .update(cx, |session, window, cx| {
            session.open_tab_for_test(window, cx);
            assert_eq!(session.tab_titles(), ["Query 1", "items", "Query 2"]);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("object-items", cx);
    })
    .unwrap();

    handle
        .update(cx, |session, _, _| {
            assert_eq!(
                session.tab_titles(),
                ["Query 1", "items", "Query 2"],
                "the table should not be opened a second time"
            );
            assert!(
                session.active_table_view().is_some(),
                "the existing table tab should have been focused"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn each_tab_keeps_its_own_result(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // First tab has a result.
    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(result_fixture(), cx);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("table").visible(),
            "the first tab lost its grid"
        );
    })
    .unwrap();

    // A second tab starts empty rather than inheriting the first tab's rows.
    handle
        .update(cx, |session, window, cx| {
            session.open_tab_for_test(window, cx);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("table").is_none(),
            "a fresh tab is showing another tab's result grid"
        );
    })
    .unwrap();

    // Going back shows the original result again.
    handle
        .update(cx, |session, _, cx| session.activate_tab_for_test(0, cx))
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("table").visible(),
            "switching back did not restore the first tab's result"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn closing_the_last_tab_leaves_an_empty_editor(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, window, cx| {
            session.open_tab_for_test(window, cx);
            assert_eq!(session.tab_titles().len(), 2);

            session.close_tab_for_test(1, window, cx);
            assert_eq!(session.tab_titles(), ["Query 1"]);

            // The session always keeps one editor open.
            session.close_tab_for_test(0, window, cx);
            assert_eq!(session.tab_titles(), ["Query 3"]);
            assert_eq!(session.active_sql(cx), "");
        })
        .unwrap();
}

#[gpui_kit::test]
fn the_filter_box_narrows_the_object_list(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.set_metadata_for_test(
                vec!["main".to_string()],
                ["items", "order_items", "customers", "Invoices"]
                    .into_iter()
                    .map(|name| DatabaseObject {
                        schema: None,
                        name: name.into(),
                        kind: ObjectKind::Table,
                    })
                    .collect(),
                cx,
            );
        })
        .unwrap();

    handle
        .update(cx, |session, window, cx| {
            // Plain substring.
            session.set_filter_for_test("item", window, cx);
            assert_eq!(session.visible_object_labels(), ["items", "order_items"]);

            // Matching ignores case, so a capitalized table is still reachable.
            session.set_filter_for_test("invoice", window, cx);
            assert_eq!(session.visible_object_labels(), ["Invoices"]);

            // Regex syntax works.
            session.set_filter_for_test("^order_", window, cx);
            assert_eq!(session.visible_object_labels(), ["order_items"]);

            session.set_filter_for_test("items$|^customers$", window, cx);
            assert_eq!(
                session.visible_object_labels(),
                ["items", "order_items", "customers"]
            );

            // A half-typed regex falls back to a literal match instead of
            // emptying the list.
            session.set_filter_for_test("order_item(", window, cx);
            assert_eq!(session.visible_object_labels(), Vec::<String>::new());
            session.set_filter_for_test("items(", window, cx);
            assert_eq!(session.visible_object_labels(), Vec::<String>::new());

            // Clearing brings everything back.
            session.set_filter_for_test("", window, cx);
            assert_eq!(session.visible_object_labels().len(), 4);
        })
        .unwrap();
}

#[gpui_kit::test]
fn typing_in_the_filter_box_filters_the_list(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.set_metadata_for_test(
                vec!["main".to_string()],
                ["items", "customers"]
                    .into_iter()
                    .map(|name| DatabaseObject {
                        schema: None,
                        name: name.into(),
                        kind: ObjectKind::Table,
                    })
                    .collect(),
                cx,
            );
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-filter", cx);
        window.input("cust", cx);
    })
    .unwrap();

    // Leaving the window closure lets the input's change event reach the
    // session before the next frame is drawn.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);

        assert_eq!(window.find("object-filter").value(), Some("cust"));
        assert!(window.find("object-customers").visible());
        assert!(
            window.try_find("object-items").is_none(),
            "the filter did not hide the non-matching table"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_platform_shortcut_opens_and_closes_tabs(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // Focus something inside the session so the keystroke has a path to the
    // Session key context.
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-filter", cx);
        window.press("secondary-t", cx);
    })
    .unwrap();

    handle
        .update(cx, |session, _, _| {
            assert_eq!(
                session.tab_titles().len(),
                2,
                "the new-tab shortcut did not open a tab"
            );
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.press("secondary-w", cx);
    })
    .unwrap();

    handle
        .update(cx, |session, _, _| {
            assert_eq!(
                session.tab_titles().len(),
                1,
                "the close-tab shortcut did not close a tab"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn the_platform_shortcut_runs_the_query(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("select 1", window, cx);
            assert!(!session.active_is_running());
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("secondary-enter", cx);
    })
    .unwrap();

    handle
        .update(cx, |session, _, cx| {
            assert!(
                session.active_is_running(),
                "the run shortcut did not start the query"
            );
            // The editor's own binding for this keystroke inserts a newline;
            // ours has to take it instead.
            assert_eq!(session.active_sql(cx), "select 1");
        })
        .unwrap();
}

#[gpui_kit::test]
fn middle_clicking_a_tab_closes_it(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, window, cx| {
            session.open_tab_for_test(window, cx);
            assert_eq!(session.tab_titles(), ["Query 1", "Query 2"]);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);

        // Aim inside the second tab: its close button sits within it, and the
        // middle button is not what that button listens for.
        let target = window.find("close-tab-1").bounds().center();

        window.dispatch_event(
            MouseMoveEvent {
                position: target,
                pressed_button: None,
                modifiers: Default::default(),
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Middle,
                position: target,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                button: MouseButton::Middle,
                position: target,
                modifiers: Default::default(),
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();

    handle
        .update(cx, |session, _, _| {
            assert_eq!(
                session.tab_titles(),
                ["Query 1"],
                "middle-clicking a tab should close it"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn refresh_reloads_the_schema_and_the_open_table_but_not_a_query_tab(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let database = connect(cx, handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the active tab should be a session");
    cx.run_until_parked();

    // Open the table so refresh has rows to reread.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("object-items", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let view = session
        .read_with(cx, |session, _| session.active_table_view())
        .expect("clicking a table should open a table view");
    let before = view.read_with(cx, |view, _| view.loaded_rows_for_test());
    assert_eq!(before, 2, "the seed data has two rows");

    // Insert a row from outside the app, the way an external client would.
    let other = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open a second connection to the test database");
    runtime::block_on(other.run_query("insert into items values (3, 'gamma', 3.0, NULL)"))
        .expect("could not insert the extra row");
    runtime::block_on(other.close());

    click(cx, handle, "refresh");
    cx.run_until_parked();

    let after = view.read_with(cx, |view, _| view.loaded_rows_for_test());
    assert_eq!(after, 3, "refresh should reread the table's rows");

    // A query tab's buffer is the user's own SQL; refresh must not re-run it.
    click(cx, handle, "new-query");
    session
        .downgrade()
        .update_in(cx, |session, window, cx| {
            session.prepare_active_editor_for_test(
                "insert into items values (4, 'delta', 4.0, NULL)",
                window,
                cx,
            );
        })
        .unwrap();

    click(cx, handle, "refresh");
    cx.run_until_parked();

    let count = runtime::block_on(other_count(&database));
    assert_eq!(
        count, 3,
        "refresh must not execute a query tab's buffer, or it would have inserted a fourth row"
    );
}
