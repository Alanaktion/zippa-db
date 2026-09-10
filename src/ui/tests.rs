//! Headless UI tests: render the real views in a test window and check the
//! layout facts that are easy to break, such as controls being pushed out of
//! the window or a pane collapsing to zero height.

use std::sync::Arc;

use gpui_kit::component::table::ColumnSort;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext as _, Bounds, InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, TestAppContext, point, px, size,
};

use crate::db::query::QueryResult;
use crate::db::tests::TempDatabase;
use crate::db::{Connection, DatabaseObject, ObjectKind, runtime};
use crate::ui::session::Session;
use crate::ui::welcome::Welcome;

const WINDOW: (f32, f32) = (900., 600.);

fn contains(window: Bounds<Pixels>, element: Bounds<Pixels>) -> bool {
    element.left() >= window.left()
        && element.right() <= window.right()
        && element.top() >= window.top()
        && element.bottom() <= window.bottom()
}

fn result_fixture() -> QueryResult {
    QueryResult {
        columns: vec!["id".into(), "name".into()],
        rows: vec![
            vec![Some("1".into()), Some("alpha".into())],
            vec![Some("2".into()), None],
        ],
        ..QueryResult::default()
    }
}

#[gpui_kit::test]
fn footer_buttons_stay_in_view_with_a_long_error(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    let handle = cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
        Welcome::new(window, cx)
    });

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        let viewport = Bounds {
            origin: Default::default(),
            size: size(px(WINDOW.0), px(WINDOW.1)),
        };
        assert!(contains(viewport, window.find("connect").bounds()));
    })
    .unwrap();

    // Server errors are long: sqlx repeats the whole connection string.
    handle
        .update(cx, |welcome, _, cx| {
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
        let connect = window.find("connect");
        assert!(connect.visible(), "the Connect button is not visible");
        assert!(
            contains(viewport, connect.bounds()),
            "a long error pushed Connect out of the window: {:?}",
            connect.bounds()
        );
        assert!(contains(viewport, window.find("save").bounds()));
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

        // SQLite is a single file, so the picker names the file itself.
        let picker = window.find("database");
        assert!(picker.visible());
        assert!(
            picker.bounds().right() <= sidebar.bounds().right(),
            "the database picker spills out of the sidebar: {:?}",
            picker.bounds()
        );
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

        // The 200th row is far past the sidebar, so it must be scrolled out of
        // view rather than stretching the panel.
        assert!(!window.find("object-table_199").visible());
    })
    .unwrap();
}

/// Build a session on a temporary SQLite database with the sidebar filled in.
fn session_with_objects(
    cx: &mut TestAppContext,
) -> (TempDatabase, gpui_kit::WindowHandle<Session>) {
    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");

    cx.update(|cx| {
        gpui_kit::component::init(cx);
        crate::keymap::bind(cx);
    });
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
                vec![DatabaseObject {
                    schema: None,
                    name: "items".into(),
                    kind: ObjectKind::Table,
                }],
                cx,
            );
        })
        .unwrap();

    (database, handle)
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
fn selecting_a_cell_highlights_its_whole_row(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(result_fixture(), cx);
        })
        .unwrap();

    let grid = handle
        .update(cx, |session, _, _| session.active_grid())
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
        .update(cx, |session, _, _| session.active_grid())
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

/// Open a table tab on the seeded `items` table and hand back its view.
fn table_view(
    cx: &mut TestAppContext,
) -> (
    TempDatabase,
    gpui_kit::WindowHandle<Session>,
    gpui_kit::Entity<crate::ui::table_view::TableView>,
) {
    let (database, handle) = session_with_objects(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-items", cx);
    })
    .unwrap();

    let view = handle
        .update(cx, |session, _, _| session.active_table_view())
        .unwrap()
        .expect("clicking a table should open a table view");

    (database, handle, view)
}

#[gpui_kit::test]
fn the_table_view_pages_through_rows(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);

    view.update(cx, |view, cx| {
        assert_eq!(view.page_for_test(), 0);
        assert_eq!(view.limit_for_test(), 500);
        assert_eq!(view.query(), "select * from items limit 500 offset 0");

        // A short first page means there is nothing after it.
        view.set_loaded_rows_for_test(2);
        assert_eq!(view.can_page_for_test(), (false, false));

        // A full page suggests there is.
        view.set_loaded_rows_for_test(500);
        assert_eq!(view.can_page_for_test(), (false, true));

        view.go_for_test(1, cx);
        assert_eq!(view.page_for_test(), 1);
        assert_eq!(view.query(), "select * from items limit 500 offset 500");
        assert!(view.can_page_for_test().0, "page 2 can go back");
    });
}

#[gpui_kit::test]
fn sorting_the_table_view_reorders_on_the_server(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);

    view.update(cx, |view, cx| {
        view.go_for_test(3, cx);
        assert_eq!(view.page_for_test(), 3);

        view.sort_for_test("name", ColumnSort::Descending, cx);
        assert_eq!(
            view.query(),
            "select * from items order by name desc limit 500 offset 0",
            "sorting should ask the server for ordered rows"
        );
        assert_eq!(
            view.page_for_test(),
            0,
            "sorting reorders the whole table, so it starts from the first page"
        );

        view.sort_for_test("name", ColumnSort::Default, cx);
        assert_eq!(view.query(), "select * from items limit 500 offset 0");
    });
}

#[gpui_kit::test]
fn sorting_a_query_result_reorders_the_rows_in_place(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(
                QueryResult {
                    columns: vec!["score".into(), "name".into()],
                    rows: vec![
                        vec![Some("10".into()), Some("ada".into())],
                        vec![Some("9".into()), Some("grace".into())],
                        vec![None, Some("unknown".into())],
                        vec![Some("100".into()), Some("alan".into())],
                    ],
                    ..QueryResult::default()
                },
                cx,
            );
        })
        .unwrap();

    let grid = handle
        .update(cx, |session, _, _| session.active_grid())
        .unwrap()
        .expect("a query tab should have a grid");

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);

        grid.update(cx, |grid, cx| {
            grid.sort_for_test(0, ColumnSort::Ascending, window, cx);
            assert_eq!(
                grid.column_values_for_test(0, cx),
                [
                    Some("9".to_string()),
                    Some("10".to_string()),
                    Some("100".to_string()),
                    None,
                ],
                "numbers should sort numerically, with NULLs last"
            );

            grid.sort_for_test(0, ColumnSort::Descending, window, cx);
            assert_eq!(
                grid.column_values_for_test(0, cx),
                [
                    None,
                    Some("100".to_string()),
                    Some("10".to_string()),
                    Some("9".to_string()),
                ]
            );

            grid.sort_for_test(1, ColumnSort::Ascending, window, cx);
            assert_eq!(
                grid.column_values_for_test(1, cx),
                [
                    Some("ada".to_string()),
                    Some("alan".to_string()),
                    Some("grace".to_string()),
                    Some("unknown".to_string()),
                ],
                "text should sort alphabetically"
            );
        });
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
