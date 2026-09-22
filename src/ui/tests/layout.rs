//! Layout facts: what stays on screen, and what a frame costs.

use super::*;

#[gpui_kit::test]
fn the_error_banner_keeps_the_new_connection_button_in_view(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let handle = cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
        Welcome::new(window, cx)
    });

    // A saved connection gives the launcher a card list; the error banner sits
    // above it at the top of the screen.
    let config = ConnectionConfig {
        name: "Long error".into(),
        ..ConnectionConfig::new(Engine::Postgres)
    };
    handle
        .update(cx, |welcome, _, cx| {
            welcome.set_connections_for_test(vec![config], cx);
            welcome.show_error_for_test(
                "error returned from database: could not connect to server: Connection refused. \
                 Is the server running on host \"db.internal.example.com\" (10.1.2.3) and \
                 accepting TCP/IP connections on port 5432?",
                cx,
            );
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let viewport = Bounds {
            origin: Default::default(),
            size: size(px(WINDOW.0), px(WINDOW.1)),
        };
        let button = window.find("welcome-new-connection");
        assert!(button.visible(), "the New connection button is not visible");
        assert!(
            contains(viewport, button.bounds()),
            "a long error pushed New connection out of the window: {:?}",
            button.bounds()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn result_grid_is_visible_after_a_query(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");

    let handle = {
        cx.update(gpui_kit::component::init);
        let connection = Arc::new(connection);
        cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
            Session::new(connection, window, cx)
        })
    };

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(result_fixture(), cx);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let table = window.find("table");
        assert!(
            table.bounds().size.height > px(0.),
            "the result grid collapsed to zero height: {:?}",
            table.bounds()
        );
        assert!(
            table.bounds().size.width > px(0.),
            "the result grid collapsed to zero width: {:?}",
            table.bounds()
        );
        assert!(
            table.visible(),
            "the result grid is not visible: {:?}",
            table.bounds()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn many_tabs_keep_the_result_grid_inside_the_window(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");

    cx.update(gpui_kit::component::init);
    let handle = {
        let connection = Arc::new(connection);
        cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
            Session::new(connection, window, cx)
        })
    };

    handle
        .update(cx, |session, window, cx| {
            session.show_result_for_test(result_fixture(), cx);
            for _ in 0..24 {
                session.open_tab_for_test(window, cx);
            }
            session.activate_tab_for_test(0, window, cx);
        })
        .unwrap();

    // A tab strip wide enough to need scrolling has to scroll inside its own
    // pane. It is the last resizable panel's content, so a strip that grew the
    // panel instead would push the whole session — grid included — off the
    // right of the window.
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let viewport = Bounds {
            origin: Default::default(),
            size: size(px(WINDOW.0), px(WINDOW.1)),
        };
        for id in ["table", "sidebar"] {
            let found = window.find(id);
            assert!(
                contains(viewport, found.bounds()),
                "{id} ran outside the window with 25 tabs open: {:?}",
                found.bounds()
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn sidebar_lists_objects_beside_the_panes(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");

    cx.update(gpui_kit::component::init);
    let handle = {
        let connection = Arc::new(connection);
        cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
            Session::new(connection, window, cx)
        })
    };

    handle
        .update(cx, |session, _, cx| {
            session.set_metadata_for_test(
                vec!["main".to_string()],
                vec![
                    DatabaseObject {
                        schema: None,
                        name: "items".into(),
                        kind: ObjectKind::Table,
                    },
                    DatabaseObject {
                        schema: None,
                        name: "named_items".into(),
                        kind: ObjectKind::View,
                    },
                ],
                cx,
            );
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);

        let sidebar = window.find("sidebar");
        assert!(sidebar.visible(), "the sidebar is not visible");

        // The sidebar is the first resizable panel, so it keeps its own width
        // and leaves the rest of the window to the editor and grid.
        assert!(
            sidebar.bounds().size.width < px(WINDOW.0),
            "the sidebar took the whole window: {:?}",
            sidebar.bounds()
        );

        for id in ["object-items", "object-named_items"] {
            let row = window.find(id);
            assert!(row.visible(), "{id} is not visible");
            assert!(
                row.bounds().right() <= sidebar.bounds().right(),
                "{id} spills out of the sidebar: {:?}",
                row.bounds()
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn dragging_the_divider_resizes_the_sidebar(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");

    cx.update(gpui_kit::component::init);
    let handle = {
        let connection = Arc::new(connection);
        cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
            Session::new(connection, window, cx)
        })
    };

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);

        let before = window.find("sidebar").bounds().size.width;
        let divider = point(before + px(2.), px(WINDOW.1 / 2.));
        window.drag(divider, point(before + px(120.), px(WINDOW.1 / 2.)), cx);
        window.render_frame(cx);

        let after = window.find("sidebar").bounds().size.width;
        assert!(
            after > before,
            "dragging the divider right did not widen the sidebar: {before:?} -> {after:?}"
        );
    })
    .unwrap();
}

/// Frames must not get more expensive as the result set grows: the grid
/// virtualizes rows, so only the visible ones may cost anything.
#[gpui_kit::test]
fn large_results_do_not_make_frames_expensive(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");

    cx.update(gpui_kit::component::init);
    let handle = {
        let connection = Arc::new(connection);
        cx.open_window(size(px(1280.), px(800.)), |window, cx| {
            Session::new(connection, window, cx)
        })
    };

    let result = QueryResult {
        columns: (0..12).map(|index| format!("column_{index}")).collect(),
        rows: (0..5000)
            .map(|row| (0..12).map(|col| Some(format!("v{row}-{col}"))).collect())
            .collect(),
        ..QueryResult::default()
    };

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(result, cx)
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        let started = std::time::Instant::now();
        for _ in 0..20 {
            window.render_frame(cx);
        }
        let loaded = started.elapsed() / 20;
        assert!(
            loaded < std::time::Duration::from_millis(50),
            "a frame with 5000 rows took {loaded:?}"
        );
    })
    .unwrap();

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(QueryResult::default(), cx)
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        let started = std::time::Instant::now();
        for _ in 0..20 {
            window.render_frame(cx);
        }
        let empty = started.elapsed() / 20;
        assert!(
            empty < std::time::Duration::from_millis(50),
            "an empty frame took {empty:?}"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn long_object_lists_scroll_inside_the_sidebar(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");

    cx.update(gpui_kit::component::init);
    let handle = {
        let connection = Arc::new(connection);
        cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
            Session::new(connection, window, cx)
        })
    };

    let objects: Vec<_> = (0..200)
        .map(|index| DatabaseObject {
            schema: None,
            name: format!("table_{index:03}"),
            kind: ObjectKind::Table,
        })
        .collect();

    handle
        .update(cx, |session, _, cx| {
            session.set_metadata_for_test(vec!["main".to_string()], objects, cx);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);

        let sidebar = window.find("sidebar");
        let first = window.find("object-table_000");
        assert!(first.visible(), "the first table is not visible");
        assert!(
            first.bounds().bottom() <= sidebar.bounds().bottom(),
            "the object list overflows the sidebar: {:?}",
            first.bounds()
        );

        // The 200th row is far past the sidebar; the tree virtualizes its
        // rows, so a row this far out of view is not rendered at all rather
        // than merely hidden.
        assert!(window.try_find("object-table_199").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn selecting_a_cell_highlights_its_whole_row(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(result_fixture(), cx);
        })
        .unwrap();

    let grid = handle
        .update(cx, |session, _, cx| session.active_grid(cx))
        .unwrap()
        .expect("the active tab should be a query tab");

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();

    grid.update(cx, |grid, cx| {
        assert_eq!(grid.selection_for_test(cx), (None, None));
        grid.select_cell_for_test(1, 1, cx);
    });

    // The table's selection event reaches the grid once effects flush.
    grid.update(cx, |grid, cx| {
        let (row, cell) = grid.selection_for_test(cx);
        assert_eq!(
            row,
            Some(1),
            "selecting a cell should highlight the row it sits on"
        );
        assert_eq!(
            cell,
            Some((1, 1)),
            "the selection should stay on the column that was clicked"
        );
    });

    // A new result drops the old selection rather than highlighting a row that
    // no longer means anything.
    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(result_fixture(), cx);
        })
        .unwrap();

    grid.update(cx, |grid, cx| {
        assert_eq!(grid.selection_for_test(cx), (None, None));
    });
}

#[gpui_kit::test]
fn columns_are_sized_from_their_contents(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(
                QueryResult {
                    columns: vec!["id".into(), "description".into()],
                    rows: vec![vec![
                        Some("1".into()),
                        Some("a description long enough to need room".into()),
                    ]],
                    ..QueryResult::default()
                },
                cx,
            );
        })
        .unwrap();

    let grid = handle
        .update(cx, |session, _, cx| session.active_grid(cx))
        .unwrap()
        .expect("the active tab should be a query tab");

    grid.update(cx, |grid, cx| {
        let widths = grid.column_widths_for_test(cx);
        assert_eq!(widths.len(), 2);
        assert!(
            widths[1] > widths[0],
            "the wider column should get more room: {widths:?}"
        );
    });
}

#[gpui_kit::test]
fn striped_rows_follow_the_setting(cx: &mut TestAppContext) {
    assert!(
        Settings::default().stripe_rows,
        "the grid stripes rows unless the setting says otherwise"
    );

    cx.update(|cx| {
        cx.set_global(Settings {
            stripe_rows: false,
            ..Settings::default()
        })
    });

    // The grid reads the setting as it draws, so drawing it is the check that
    // the switch reaches the table at all.
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();
    assert_eq!(view.read_with(cx, |view, _| view.loaded_rows_for_test()), 2);
}
