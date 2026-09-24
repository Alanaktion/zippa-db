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
        .update(cx, |session, _, cx| {
            assert_eq!(session.tab_titles(cx), ["Query 1"]);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-items", cx);
    })
    .unwrap();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(session.tab_titles(cx), ["Query 1", "items"]);
            assert!(
                session.active_table_view(cx).is_some(),
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
            assert_eq!(session.tab_titles(cx), ["Query 1", "items", "Query 2"]);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("object-items", cx);
    })
    .unwrap();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(cx),
                ["Query 1", "items", "Query 2"],
                "the table should not be opened a second time"
            );
            assert!(
                session.active_table_view(cx).is_some(),
                "the existing table tab should have been focused"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn the_sidebar_highlights_the_table_the_active_tab_shows(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.selected_object_label_for_test(cx),
                None,
                "a fresh query tab highlights nothing"
            );
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-items", cx);
    })
    .unwrap();

    handle
        .update(cx, |session, window, cx| {
            assert_eq!(
                session.selected_object_label_for_test(cx),
                Some("items".to_string()),
                "opening a table should highlight it in the sidebar"
            );

            // Back to a query tab: nothing in the sidebar should stay lit.
            session.open_tab_for_test(window, cx);
            assert_eq!(session.selected_object_label_for_test(cx), None);

            // Switching back to the table tab restores the highlight.
            session.activate_tab_for_test(1, window, cx);
            assert_eq!(
                session.selected_object_label_for_test(cx),
                Some("items".to_string())
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn the_sidebar_switches_to_the_management_tools(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_sidebar_tab_for_test(true, cx)
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);

        assert!(
            window.find("open-console").visible(),
            "the console tool is missing from the Management tab"
        );
        assert!(
            window.try_find("object-items").is_none(),
            "the object list belongs on the Schema tab, not the Management one"
        );
        // A file-backed connection has no server to ask, so its tools are not
        // listed.
        assert!(
            window.try_find("open-process-list").is_none(),
            "SQLite should not offer the server tools"
        );
    })
    .unwrap();

    handle
        .update(cx, |session, _, cx| {
            session.show_sidebar_tab_for_test(false, cx)
        })
        .unwrap();

    // Back on the Schema tab, the search and import buttons share the filter's
    // line rather than taking rows of their own.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);

        assert!(
            window.try_find("open-console").is_none(),
            "the server tools belong on the Management tab"
        );
        let filter = window.find("object-filter").bounds();
        for id in ["search-schema", "import-dump"] {
            let button = window.find(id);
            assert!(button.visible(), "{id} is missing from the Schema tab");
            // The button's center sits within the filter's own band, so the two
            // share a line rather than one taking a row of its own.
            let center = button.bounds().center().y;
            assert!(
                center >= filter.top() && center <= filter.bottom(),
                "{id} is not on the filter's line: {:?} vs {:?}",
                button.bounds(),
                filter
            );
        }
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
        .update(cx, |session, window, cx| {
            session.activate_tab_for_test(0, window, cx)
        })
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
            assert_eq!(session.tab_titles(cx).len(), 2);

            session.close_tab_for_test(1, window, cx);
            assert_eq!(session.tab_titles(cx), ["Query 1"]);

            // The session always keeps one editor open.
            session.close_tab_for_test(0, window, cx);
            assert_eq!(session.tab_titles(cx), ["Query 3"]);
            assert_eq!(session.active_sql(cx), "");
        })
        .unwrap();
}

#[gpui_kit::test]
fn typing_into_a_tab_marks_it_dirty(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            assert!(
                !session.tab_is_dirty_for_test(0, cx),
                "a freshly opened tab has nothing unsaved"
            );
        })
        .unwrap();

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("SELECT 1;", window, cx);
            assert!(
                session.tab_is_dirty_for_test(0, cx),
                "typing into the buffer should mark the tab dirty"
            );
        })
        .unwrap();

    // A second, untouched tab starts clean even though the first is dirty.
    handle
        .update(cx, |session, window, cx| {
            session.open_tab_for_test(window, cx);
            assert!(!session.tab_is_dirty_for_test(1, cx));
            assert!(session.tab_is_dirty_for_test(0, cx));
        })
        .unwrap();
}

#[gpui_kit::test]
fn staging_an_edit_marks_a_table_tab_dirty(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    let table_tab = |cx: &mut TestAppContext| {
        handle
            .update(cx, |session, _, cx| {
                session
                    .tab_titles(cx)
                    .iter()
                    .position(|title| title == "items")
                    .expect("the items table should have a tab")
            })
            .unwrap()
    };

    let index = table_tab(cx);
    handle
        .update(cx, |session, _, cx| {
            assert!(
                !session.tab_is_dirty_for_test(index, cx),
                "a freshly opened table has nothing staged"
            );
        })
        .unwrap();

    // Typing into a cell is work a close would lose, so the tab counts as
    // unsaved even before the edit is applied.
    stage_cell(cx, &view, 0, 1, "renamed");
    cx.run_until_parked();
    handle
        .update(cx, |session, _, cx| {
            assert!(
                session.tab_is_dirty_for_test(index, cx),
                "a staged edit is unsaved work"
            );
        })
        .unwrap();

    // Applying the edit writes it, so the tab is clean again.
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    handle
        .update(cx, |session, _, cx| {
            assert!(
                !session.tab_is_dirty_for_test(index, cx),
                "a written edit is no longer unsaved"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn closing_a_dirty_tab_asks_first(cx: &mut TestAppContext) {
    let (_database, handle, session) = workspace_session(cx);

    prepare_workspace_editor(cx, &handle, &session, "SELECT 1;");

    close_workspace_tab(cx, &handle, &session, 0);
    assert!(
        workspace_dialog_open(cx, &handle),
        "a dirty tab should be asked about before it closes"
    );
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 1"],
        "the tab should still be open while the question is up"
    );

    // Saying no leaves the tab open, still dirty.
    click_workspace(cx, &handle, "cancel");
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 1"]
    );
    assert!(session.read_with(cx, |session, cx| session.tab_is_dirty_for_test(0, cx)));

    // Saying yes throws the buffer away and closes the tab; the session
    // always keeps one editor, so a fresh one takes its place.
    close_workspace_tab(cx, &handle, &session, 0);
    click_workspace(cx, &handle, "ok");
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 2"],
        "the session always keeps an editor open"
    );
    assert!(!session.read_with(cx, |session, cx| session.tab_is_dirty_for_test(0, cx)));
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
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(cx).len(),
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
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(cx).len(),
                1,
                "the close-tab shortcut did not close a tab"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn the_close_shortcut_keeps_working_from_tab_to_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, window, cx| {
            session.open_tab_for_test(window, cx);
            session.open_tab_for_test(window, cx);
            session.activate_tab_for_test(0, window, cx);
            assert_eq!(session.tab_titles(cx), ["Query 1", "Query 2", "Query 3"]);
        })
        .unwrap();

    // Twice in a row: closing a tab has to leave the one that takes its place
    // in front and holding the keyboard, or the second press lands on nothing.
    for _ in 0..2 {
        cx.update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            window.press("secondary-w", cx);
        })
        .unwrap();
    }

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(cx),
                ["Query 3"],
                "the shortcut should keep closing the tab in front"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn closing_other_tabs_keeps_the_tab_the_command_came_from(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, window, cx| {
            session.open_tab_for_test(window, cx);
            session.open_tab_for_test(window, cx);
            assert_eq!(session.tab_titles(cx), ["Query 1", "Query 2", "Query 3"]);
            session.close_scope_for_test(1, CloseScope::Others, cx);
        })
        .unwrap();
    cx.run_until_parked();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(cx),
                ["Query 2"],
                "closing the others should leave the tab the command came from"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn closing_tabs_to_the_right_keeps_the_ones_before_them(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, window, cx| {
            session.open_tab_for_test(window, cx);
            session.open_tab_for_test(window, cx);
            session.close_scope_for_test(0, CloseScope::ToTheRight, cx);
        })
        .unwrap();
    cx.run_until_parked();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(session.tab_titles(cx), ["Query 1"]);
        })
        .unwrap();
}

#[gpui_kit::test]
fn closing_all_table_tabs_keeps_the_queries(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let object = DatabaseObject {
        schema: None,
        name: "items".into(),
        kind: ObjectKind::Table,
    };

    handle
        .update(cx, |session, window, cx| {
            session.open_schema_for_test(&object, window, cx);
            session.open_tab_for_test(window, cx);
            assert_eq!(
                session.tab_titles(cx),
                ["Query 1", "items", "Query 2"],
                "the structure tab should sit between the two query tabs"
            );
            session.close_scope_for_test(1, CloseScope::TableTabs, cx);
        })
        .unwrap();
    cx.run_until_parked();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(cx),
                ["Query 1", "Query 2"],
                "only the table tabs should have gone"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn closing_several_dirty_tabs_asks_once(cx: &mut TestAppContext) {
    let (_database, handle, session) = workspace_session(cx);

    // Two query tabs, each holding text that was never written anywhere.
    prepare_workspace_editor(cx, &handle, &session, "SELECT 1;");
    cx.update_window(handle.window.into(), |_, window, cx| {
        session.update(cx, |session, cx| session.open_tab_for_test(window, cx));
    })
    .unwrap();
    prepare_workspace_editor(cx, &handle, &session, "SELECT 2;");

    // One command, two dirty tabs.
    session.update(cx, |session, cx| {
        session.close_scope_for_test(0, CloseScope::QueryTabs, cx)
    });
    cx.run_until_parked();

    assert!(
        workspace_dialog_open(cx, &handle),
        "closing dirty tabs should ask first"
    );
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 1", "Query 2"],
        "nothing should close while the question is up"
    );

    // Saying no leaves both of them open.
    click_workspace(cx, &handle, "cancel");
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 1", "Query 2"]
    );

    // Saying yes closes them both on one answer, and the session's own rule
    // leaves a fresh editor behind.
    session.update(cx, |session, cx| {
        session.close_scope_for_test(0, CloseScope::QueryTabs, cx)
    });
    cx.run_until_parked();
    click_workspace(cx, &handle, "ok");
    // The answer reaches the session on the dialog's own effects, so the set
    // is not closed until those have run.
    cx.run_until_parked();
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 3"],
        "one answer should close the whole set"
    );
    assert!(!session.read_with(cx, |session, cx| session.tab_is_dirty_for_test(0, cx)));
}

#[gpui_kit::test]
fn the_platform_shortcut_runs_the_query(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("select 1", window, cx);
            assert!(!session.active_is_running(cx));
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
                session.active_is_running(cx),
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

    let close_id = handle
        .update(cx, |session, window, cx| {
            session.open_tab_for_test(window, cx);
            assert_eq!(session.tab_titles(cx), ["Query 1", "Query 2"]);
            session.close_button_id_for_test(1, cx)
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);

        // Aim inside the second tab: its close button sits within it, and the
        // middle button is not what that button listens for.
        let target = window.find(close_id).bounds().center();

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
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(cx),
                ["Query 1"],
                "middle-clicking a tab should close it"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn refresh_reloads_the_schema_and_the_open_table_but_not_a_query_tab(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let database = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the active tab should be a session");
    cx.run_until_parked();

    // Open the table so refresh has rows to reread.
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("object-items", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let view = session
        .read_with(cx, |session, cx| session.active_table_view(cx))
        .expect("clicking a table should open a table view");
    let before = view.read_with(cx, |view, _| view.loaded_rows_for_test());
    assert_eq!(before, 2, "the seed data has two rows");

    // Insert a row from outside the app, the way an external client would.
    let other = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open a second connection to the test database");
    runtime::block_on(other.run_query("insert into items values (3, 'gamma', 3.0, NULL)"))
        .expect("could not insert the extra row");
    runtime::block_on(other.close());

    click(cx, &handle, "refresh");
    cx.run_until_parked();

    let after = view.read_with(cx, |view, _| view.loaded_rows_for_test());
    assert_eq!(after, 3, "refresh should reread the table's rows");

    // A query tab's buffer is the user's own SQL; refresh must not re-run it.
    click(cx, &handle, "new-query");
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

    click(cx, &handle, "refresh");
    cx.run_until_parked();

    let count = runtime::block_on(other_count(&database));
    assert_eq!(
        count, 3,
        "refresh must not execute a query tab's buffer, or it would have inserted a fourth row"
    );
}

#[gpui_kit::test]
async fn dragging_a_tab_splits_the_view(cx: &mut TestAppContext) {
    use gpui_kit::test::TestAppContextExt;
    use std::time::Duration;

    let (_database, handle) = session_with_objects(cx);

    // A table tab beside Query 1, both in the one group the session starts
    // with.
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-items", cx);
    })
    .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        // The right 35% of the group's own body is the drop zone that
        // splits rather than merges — the exact centre would just reorder
        // the tab within the same group.
        let body = window.find("tab-panel").bounds();
        let from = window.within("tab-bar").find(1usize).bounds().center();
        let to = gpui_kit::point(
            body.origin.x + body.size.width * 0.9,
            body.origin.y + body.size.height / 2.,
        );
        window.drag(from, to, cx);
    })
    .unwrap();

    cx.wait_for(handle.into(), Duration::from_secs(1), |window, _| {
        // "next-page" only ever appears in a table view's own footer, so its
        // presence is the table panel; the query panel keeps its stable
        // per-key id.
        match (
            window.try_find(("session-panel", 0usize)),
            window.try_find("next-page"),
        ) {
            (Some(query), Some(table)) => {
                query.visible()
                    && table.visible()
                    && query.bounds().right() <= table.bounds().left()
            }
            _ => false,
        }
    })
    .await;
}

#[gpui_kit::test]
fn closing_a_panel_from_the_dock_forgets_it(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let panel = handle
        .update(cx, |session, window, cx| {
            session.open_tab_for_test(window, cx);
            assert_eq!(session.tab_titles(cx), ["Query 1", "Query 2"]);
            session.panel_for_test(1)
        })
        .unwrap();

    // The `…`-menu Close path: it removes the panel from the dock directly,
    // never asking `Session::close_tab` first.
    handle
        .update(cx, |session, window, cx| {
            session.dock_remove_for_test(&panel, window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(cx),
                ["Query 1"],
                "the dock's own close path should still be noticed and forgotten"
            );
        })
        .unwrap();
}

#[gpui_kit::test]
fn the_footers_view_structure_opens_the_structure_tab(cx: &mut TestAppContext) {
    let (_database, handle, _view) = table_view(cx);
    cx.run_until_parked();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(session.tab_titles(cx), ["Query 1", "items"]);
        })
        .unwrap();

    click_in_session(cx, handle, "view-structure");
    cx.run_until_parked();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(cx),
                ["Query 1", "items", "items"],
                "the structure tab sits beside the data tab, not on top of it"
            );
            assert!(
                session.active_schema_view(cx).is_some(),
                "the footer button should open the table's structure tab"
            );
        })
        .unwrap();

    // Ask again from the data tab: the structure tab is brought forward rather
    // than opened a second time.
    handle
        .update(cx, |session, window, cx| {
            session.activate_tab_for_test(1, window, cx);
        })
        .unwrap();
    click_in_session(cx, handle, "view-structure");
    cx.run_until_parked();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(session.tab_titles(cx), ["Query 1", "items", "items"]);
            assert!(session.active_schema_view(cx).is_some());
        })
        .unwrap();
}
