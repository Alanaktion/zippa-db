//! Headless UI tests: render the real views in a test window and check the
//! layout facts that are easy to break, such as controls being pushed out of
//! the window or a pane collapsing to zero height.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::table::ColumnSort;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext as _, Bounds, InputEvent as _, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, TestAppContext, WindowHandle, point, px, size,
};
use uuid::Uuid;

use crate::app::Workspace;
use crate::settings::{self, Appearance, Settings};
use crate::ui::settings_window;
use crate::ui::welcome::WelcomeEvent;

use crate::db::query::QueryResult;
use crate::db::tests::TempDatabase;
use crate::db::{Connection, ConnectionConfig, DatabaseObject, ObjectKind, SafetyMode, runtime};
use crate::ui::filter_bar::Operator;
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
    session_with_safety(cx, SafetyMode::default())
}

/// The same, on a connection that handles inline edits the way `safety` says.
fn session_with_safety(
    cx: &mut TestAppContext,
    safety: SafetyMode,
) -> (TempDatabase, gpui_kit::WindowHandle<Session>) {
    let database = runtime::block_on(TempDatabase::new());
    let config = ConnectionConfig {
        safety,
        ..database.config()
    };
    let connection = runtime::block_on(Connection::open(config, None))
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
    table_view_with_safety(cx, SafetyMode::default())
}

/// The same, on a connection that handles inline edits the way `safety` says.
fn table_view_with_safety(
    cx: &mut TestAppContext,
    safety: SafetyMode,
) -> (
    TempDatabase,
    gpui_kit::WindowHandle<Session>,
    gpui_kit::Entity<crate::ui::table_view::TableView>,
) {
    let (database, handle) = session_with_safety(cx, safety);

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
        assert_eq!(view.query(cx), "select * from items limit 500 offset 0");

        // A short first page means there is nothing after it.
        view.set_loaded_rows_for_test(2);
        assert_eq!(view.can_page_for_test(), (false, false));

        // A full page suggests there is.
        view.set_loaded_rows_for_test(500);
        assert_eq!(view.can_page_for_test(), (false, true));

        view.go_for_test(1, cx);
        assert_eq!(view.page_for_test(), 1);
        assert_eq!(view.query(cx), "select * from items limit 500 offset 500");
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
            view.query(cx),
            "select * from items order by name desc limit 500 offset 0",
            "sorting should ask the server for ordered rows"
        );
        assert_eq!(
            view.page_for_test(),
            0,
            "sorting reorders the whole table, so it starts from the first page"
        );

        view.sort_for_test("name", ColumnSort::Default, cx);
        assert_eq!(view.query(cx), "select * from items limit 500 offset 0");
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

/// A directory the file dialogs can be pointed at, removed with the test.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("zippa-db-files-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("could not create the scratch directory");
        Self { path }
    }

    /// Put a file in the directory and hand back its path.
    fn file(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.path.join(name);
        std::fs::write(&path, contents).expect("could not write the scratch file");
        path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Draw the session and click a button in it by id.
fn click_in_session(cx: &mut TestAppContext, handle: WindowHandle<Session>, id: &'static str) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(id, cx);
    })
    .unwrap();
}

/// Draw the session and send it a keystroke; something inside it must already
/// hold focus for the keystroke to reach the Session key context.
fn press(cx: &mut TestAppContext, handle: WindowHandle<Session>, keystroke: &str) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press(keystroke, cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn opening_a_sql_file_puts_it_in_its_own_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let scratch = ScratchDir::new();
    let file = scratch.file("report.sql", "SELECT 1;\n");

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-filter", cx);
    })
    .unwrap();
    press(cx, handle, "secondary-o");

    cx.simulate_path_prompt_response(|options| {
        assert!(options.files, "the open dialog should offer files");
        assert!(
            !options.directories,
            "the open dialog should not offer directories"
        );
        Some(vec![file])
    });
    cx.run_until_parked();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(),
                ["Query 1", "report.sql"],
                "the file should open in a tab named after it"
            );
            assert_eq!(session.active_sql(cx), "SELECT 1;\n");
        })
        .unwrap();
}

#[gpui_kit::test]
fn cancelling_the_open_dialog_opens_nothing(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-filter", cx);
    })
    .unwrap();
    press(cx, handle, "secondary-o");

    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();

    handle
        .update(cx, |session, _, _| {
            assert_eq!(session.tab_titles(), ["Query 1"]);
        })
        .unwrap();
}

#[gpui_kit::test]
fn saving_a_tab_with_no_file_asks_where_to_put_it(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let scratch = ScratchDir::new();
    // No extension: saving one should add `.sql` for the user.
    let chosen = scratch.path.join("notes");

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("SELECT 2;", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-s");

    cx.simulate_new_path_selection({
        let chosen = chosen.clone();
        move |_directory| Some(chosen)
    });
    cx.run_until_parked();

    let saved = chosen.with_extension("sql");
    assert_eq!(
        std::fs::read_to_string(&saved).expect("the buffer was not written"),
        "SELECT 2;"
    );
    handle
        .update(cx, |session, _, _| {
            assert_eq!(
                session.tab_titles(),
                ["notes.sql"],
                "the tab should take the name of the file it was saved to"
            );
        })
        .unwrap();

    // Saving again goes straight to the same file.
    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("SELECT 3;", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-s");
    cx.run_until_parked();

    assert!(
        !cx.did_prompt_for_new_path(),
        "saving a tab that already has a file should not ask again"
    );
    assert_eq!(
        std::fs::read_to_string(&saved).expect("the buffer was not written"),
        "SELECT 3;"
    );
}

#[gpui_kit::test]
fn save_as_asks_again_and_follows_the_new_file(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let scratch = ScratchDir::new();
    let first = scratch.path.join("first.sql");
    let second = scratch.path.join("second.sql");

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("SELECT 4;", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-s");
    cx.simulate_new_path_selection({
        let first = first.clone();
        move |_directory| Some(first)
    });
    cx.run_until_parked();

    press(cx, handle, "secondary-shift-s");
    assert!(
        cx.did_prompt_for_new_path(),
        "Save As should ask even when the tab already has a file"
    );
    cx.simulate_new_path_selection({
        let second = second.clone();
        move |_directory| Some(second)
    });
    cx.run_until_parked();

    assert_eq!(
        std::fs::read_to_string(&second).expect("the buffer was not written"),
        "SELECT 4;"
    );
    handle
        .update(cx, |session, _, _| {
            assert_eq!(session.tab_titles(), ["second.sql"]);
        })
        .unwrap();

    // The tab now follows the second file, so a plain save leaves the first.
    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("SELECT 5;", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-s");
    cx.run_until_parked();

    assert_eq!(
        std::fs::read_to_string(&first).expect("the first file was not written"),
        "SELECT 4;"
    );
    assert_eq!(
        std::fs::read_to_string(&second).expect("the buffer was not written"),
        "SELECT 5;"
    );
}

/// Four rows whose original, ascending, and descending orders all differ.
fn scores() -> QueryResult {
    QueryResult {
        columns: vec!["score".into(), "name".into()],
        rows: vec![
            vec![Some("10".into()), Some("ada".into())],
            vec![Some("9".into()), Some("grace".into())],
            vec![None, Some("unknown".into())],
            vec![Some("100".into()), Some("alan".into())],
        ],
        ..QueryResult::default()
    }
}

#[gpui_kit::test]
fn clicking_a_column_header_cycles_the_sort_and_comes_back(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(scores(), cx);
        })
        .unwrap();

    let grid = handle
        .update(cx, |session, _, _| session.active_grid())
        .unwrap()
        .expect("a query tab should have a grid");

    // The sort control sits at the right-hand end of the header cell. Column
    // 0 in the table is the row-pick column, so the first data column's header
    // is the one after it.
    let click_sort = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            let size = window.find(("col-header", 1usize)).bounds().size;
            window.click_at(
                ("col-header", 1usize),
                point(size.width - px(8.), size.height / 2.),
                cx,
            );
        })
        .unwrap();
    };
    let scores_shown =
        |cx: &mut TestAppContext| grid.update(cx, |grid, cx| grid.column_values_for_test(0, cx));

    click_sort(cx);
    assert_eq!(
        scores_shown(cx),
        [
            None,
            Some("100".to_string()),
            Some("10".to_string()),
            Some("9".to_string()),
        ],
        "the first click should sort descending"
    );

    click_sort(cx);
    assert_eq!(
        scores_shown(cx),
        [
            Some("9".to_string()),
            Some("10".to_string()),
            Some("100".to_string()),
            None,
        ],
        "the second click should sort ascending"
    );

    click_sort(cx);
    assert_eq!(
        scores_shown(cx),
        [
            Some("10".to_string()),
            Some("9".to_string()),
            None,
            Some("100".to_string()),
        ],
        "the third click should put the rows back in the order they arrived"
    );
}

#[gpui_kit::test]
fn the_table_view_keeps_its_sort_when_the_page_reloads(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);

    view.update(cx, |view, cx| {
        view.sort_for_test("name", ColumnSort::Descending, cx)
    });
    cx.run_until_parked();

    let sorted = view.update(cx, |view, cx| {
        let grid = view.grid_for_test();
        let grid = grid.read(cx);
        grid.sorted_for_test(cx)
    });

    assert_eq!(
        sorted,
        Some(("name".to_string(), ColumnSort::Descending)),
        "the rows come back already sorted, so the header has to say so: \
         forgetting it restarts the cycle and every click sorts descending"
    );
}

#[gpui_kit::test]
fn a_table_opens_with_the_page_size_from_the_settings(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Settings {
            page_size: 25,
            ..Settings::default()
        })
    });

    let (_database, _handle, view) = table_view(cx);

    view.update(cx, |view, cx| {
        assert_eq!(view.limit_for_test(), 25);
        assert_eq!(
            view.query(cx),
            "select * from items limit 25 offset 0",
            "the page size should reach the statement"
        );
    });
}

#[gpui_kit::test]
fn the_settings_window_is_opened_once(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);

    let opened = cx.update(|cx| {
        settings_window::open(cx);
        cx.windows().len()
    });
    assert_eq!(opened, 1, "the settings window did not open");

    let reopened = cx.update(|cx| {
        settings_window::open(cx);
        cx.windows().len()
    });
    assert_eq!(
        reopened, 1,
        "opening the settings again should raise the window already open"
    );
}

#[gpui_kit::test]
fn the_settings_window_shows_its_pages(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    cx.update(settings_window::open);

    let handle = cx
        .update(|cx| cx.windows().first().copied())
        .expect("the settings window did not open");

    cx.update_window(handle, |_, window, cx| {
        window.draw(cx).clear(cx);
        let settings = window.find("settings");
        assert!(settings.visible(), "the settings pages are not on screen");
        assert!(
            settings.bounds().size.width > px(0.),
            "the settings pages collapsed: {:?}",
            settings.bounds()
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn pinning_the_appearance_overrides_the_system(cx: &mut TestAppContext) {
    cx.update(gpui_kit::component::init);
    // The workspace is what subscribes to the window's appearance.
    let _handle = cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
        Workspace::new(window, cx)
    });

    let mode = |cx: &mut TestAppContext| cx.update(|cx| Theme::global(cx).mode);

    assert_eq!(
        mode(cx),
        ThemeMode::Light,
        "the default setting should follow the system, which reports light here"
    );

    cx.update(|cx| settings::update(cx, |settings| settings.appearance = Appearance::Dark));
    assert_eq!(
        mode(cx),
        ThemeMode::Dark,
        "a pinned appearance should ignore the system"
    );

    cx.update(|cx| settings::update(cx, |settings| settings.appearance = Appearance::Auto));
    assert_eq!(
        mode(cx),
        ThemeMode::Light,
        "back on auto, the system decides"
    );
}

/// A workspace window, which starts on the connection manager.
fn workspace(cx: &mut TestAppContext) -> WindowHandle<Workspace> {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        crate::keymap::bind(cx);
    });

    cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
        Workspace::new(window, cx)
    })
}

/// Open a connection to a database of its own from the active tab, the way
/// the connection manager does when its Connect button succeeds.
fn connect(cx: &mut TestAppContext, handle: WindowHandle<Workspace>) -> TempDatabase {
    let database = runtime::block_on(TempDatabase::new());
    let connection = Arc::new(
        runtime::block_on(Connection::open(database.config(), None))
            .expect("could not open the test database"),
    );

    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");

    welcome.update(cx, |_, cx| cx.emit(WelcomeEvent::Connected(connection)));
    cx.run_until_parked();

    database
}

fn titles(cx: &mut TestAppContext, handle: WindowHandle<Workspace>) -> Vec<String> {
    handle
        .update(cx, |workspace, _, cx| workspace.tab_titles_for_test(cx))
        .unwrap()
}

fn active(cx: &mut TestAppContext, handle: WindowHandle<Workspace>) -> usize {
    handle
        .update(cx, |workspace, _, _| workspace.active_for_test())
        .unwrap()
}

fn click(cx: &mut TestAppContext, handle: WindowHandle<Workspace>, id: &'static str) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(id, cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn each_connection_opens_in_a_tab_of_its_own(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    assert_eq!(
        titles(cx, handle),
        ["New connection"],
        "the window should start on the connection manager"
    );

    let first = connect(cx, handle);
    assert_eq!(
        titles(cx, handle),
        [first.config().display_name()],
        "connecting should turn the tab it was opened from into the session"
    );

    click(cx, handle, "new-connection");
    assert_eq!(
        titles(cx, handle),
        [first.config().display_name(), "New connection".to_string()]
    );
    assert_eq!(active(cx, handle), 1, "a new tab comes to the front");

    let second = connect(cx, handle);
    assert_eq!(
        titles(cx, handle),
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
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("secondary-n", cx);
    })
    .unwrap();

    assert_eq!(
        titles(cx, handle),
        ["New connection", "New connection"],
        "the new-connection shortcut should work before anything has focus"
    );
}

#[gpui_kit::test]
fn switching_connections_leaves_each_session_where_it_was(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _first = connect(cx, handle);

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

    click(cx, handle, "new-connection");
    let _second = connect(cx, handle);
    type_sql(cx, "select 2");

    // Back one tab, and the first session is as it was left.
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("secondary-shift-[", cx);
    })
    .unwrap();

    assert_eq!(active(cx, handle), 0, "the shortcut should step back a tab");
    assert_eq!(sql_of(cx), "select 1");

    cx.update_window(handle.into(), |_, window, cx| {
        window.press("secondary-shift-]", cx);
    })
    .unwrap();

    assert_eq!(active(cx, handle), 1);
    assert_eq!(sql_of(cx), "select 2");
}

#[gpui_kit::test]
fn closing_a_connection_tab_leaves_the_others(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _first = connect(cx, handle);
    click(cx, handle, "new-connection");
    let second = connect(cx, handle);

    click(cx, handle, "close-connection-0");

    assert_eq!(
        titles(cx, handle),
        [second.config().display_name()],
        "closing one connection should leave the other open"
    );
    assert_eq!(active(cx, handle), 0);
}

#[gpui_kit::test]
fn closing_the_last_connection_leaves_the_manager(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, handle);

    click(cx, handle, "close-connection-0");

    assert_eq!(
        titles(cx, handle),
        ["New connection"],
        "the window always has a tab to open a connection from"
    );
}

#[gpui_kit::test]
fn disconnecting_returns_the_tab_to_the_manager(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let first = connect(cx, handle);
    click(cx, handle, "new-connection");
    let _second = connect(cx, handle);

    click(cx, handle, "disconnect");
    cx.run_until_parked();

    assert_eq!(
        titles(cx, handle),
        [first.config().display_name(), "New connection".to_string()],
        "disconnecting should keep the tab and show the manager in it"
    );
    assert_eq!(
        active(cx, handle),
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
    click(cx, handle, "refresh");
    click(cx, handle, "new-query");

    assert_eq!(
        titles(cx, handle),
        ["New connection"],
        "refresh and new-query should have no session to act on"
    );
}

#[gpui_kit::test]
fn the_toolbar_new_query_button_opens_a_tab(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the active tab should be a session");
    assert_eq!(
        session.read_with(cx, |session, _| session.tab_titles()),
        ["Query 1"]
    );

    click(cx, handle, "new-query");

    assert_eq!(
        session.read_with(cx, |session, _| session.tab_titles()),
        ["Query 1", "Query 2"],
        "the toolbar button should behave like the session's own new-tab button"
    );
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

/// Reopen the test database and count the rows in `items`, the way the test
/// above checks that refresh did not run an INSERT sitting in a query tab.
async fn other_count(database: &TempDatabase) -> i64 {
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not reopen the test database");
    let result = connection
        .run_query("select count(*) as n from items")
        .await
        .expect("could not count the rows");
    connection.close().await;
    result.rows[0][0]
        .as_ref()
        .expect("count(*) should not be null")
        .parse()
        .expect("count(*) should be a number")
}

/// Stage `value` in a cell of the open table view's grid.
///
/// The selection and the editor are two separate frames on purpose: focus
/// moves when a frame is drawn, so doing both at once would leave the table
/// holding the focus and blur the editor straight back out again.
fn stage_cell(
    cx: &mut TestAppContext,
    view: &gpui_kit::Entity<crate::ui::table_view::TableView>,
    row: usize,
    col: usize,
    value: &str,
) {
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    // Editing starts from a click in the grid, which focuses it and selects
    // the cell.
    grid.downgrade()
        .update_in(cx, |grid, window, cx| {
            grid.focus_for_test(window, cx);
            grid.select_cell_for_test(row, col, cx);
        })
        .unwrap();
    cx.run_until_parked();

    // A double click on the selected cell opens the editor over it.
    grid.downgrade()
        .update_in(cx, |grid, window, cx| {
            grid.begin_edit_for_test(row, col, window, cx);
            grid.set_editor_value_for_test(value, window, cx);
        })
        .unwrap();
    cx.run_until_parked();
}

/// Put the focus back on the grid itself, the way clicking out of a cell
/// editor and back onto the table does.
fn focus_grid(cx: &mut TestAppContext, view: &gpui_kit::Entity<crate::ui::table_view::TableView>) {
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.downgrade()
        .update_in(cx, |grid, window, cx| {
            grid.commit_editor(cx);
            grid.focus_for_test(window, cx);
        })
        .unwrap();
}

/// The `id` column of the `items` row with this name, read through a second
/// connection.
async fn id_of(database: &TempDatabase, name: &str) -> Option<String> {
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not reopen the test database");
    let result = connection
        .run_query(&format!(
            "select id from items where name = {}",
            crate::db::quote_literal(name)
        ))
        .await
        .expect("could not read the row");
    connection.close().await;
    result.rows.first().and_then(|row| row[0].clone())
}

/// The `name` column of `items` for a row, read through a second connection.
async fn name_of(database: &TempDatabase, id: i64) -> Option<String> {
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not reopen the test database");
    let result = connection
        .run_query(&format!("select name from items where id = {id}"))
        .await
        .expect("could not read the row");
    connection.close().await;
    result.rows[0][0].clone()
}

#[gpui_kit::test]
fn a_table_with_a_primary_key_can_be_edited(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.row_key_for_test(),
            Some(crate::db::RowKey::Columns(vec!["id".to_string()])),
            "items is keyed by its primary key"
        );
        assert!(view.is_editable_for_test());
        assert!(view.grid_for_test().read(cx).editable_for_test(cx));
    });
}

#[gpui_kit::test]
fn leaving_the_row_writes_it_on_an_auto_apply_connection(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view_with_safety(cx, SafetyMode::AutoApply);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "renamed");
    let grid = view.read_with(cx, |view, _| view.grid_for_test());

    // Moving the selection to another row is what applies the edit, the way a
    // form field applies when you tab out of it.
    grid.update(cx, |grid, cx| grid.select_cell_for_test(1, 1, cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("renamed".to_string()),
        "leaving the row should have written it"
    );
    assert!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .is_empty()),
        "a written row should no longer be staged"
    );
}

#[gpui_kit::test]
fn leaving_the_row_keeps_the_edit_on_a_staged_connection(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view_with_safety(cx, SafetyMode::Staged);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "renamed");
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.select_cell_for_test(1, 1, cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "a staged connection must not write until the edit is applied"
    );
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .len()),
        1,
        "the edit should still be waiting"
    );

    // The edit is still there to apply by hand.
    focus_grid(cx, &view);
    press(cx, handle, "secondary-s");
    cx.run_until_parked();
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("renamed".to_string()),
        "applying by hand should write the staged row"
    );
}

#[gpui_kit::test]
fn applying_with_secondary_s_writes_the_row(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "applied");
    focus_grid(cx, &view);
    press(cx, handle, "secondary-s");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("applied".to_string()),
        "the save key should write a table tab's staged edits"
    );
}

#[gpui_kit::test]
fn discarding_reverts_staged_edits_without_writing(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "thrown away");
    focus_grid(cx, &view);
    press(cx, handle, "secondary-z");
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.read_with(cx, |grid, cx| {
        assert!(grid.staged(cx).is_empty(), "discard should drop the edits");
        assert_eq!(
            grid.cell_for_test(0, 1, cx),
            Some("alpha".to_string()),
            "the cell should show the loaded value again"
        );
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "discard must not write anything"
    );
}

#[gpui_kit::test]
fn setting_a_cell_to_null_writes_null(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| {
        grid.select_cell_for_test(0, 1, cx);
        grid.set_null(cx);
    });
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        None,
        "the set null command should store SQL NULL"
    );
}

#[gpui_kit::test]
fn paging_asks_before_it_discards_staged_edits(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");

    // A new page is a new set of rows, so the old ones' edits would go with
    // them; the footer asks instead of turning the page.
    view.update(cx, |view, cx| view.go_for_test(1, cx));
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(view.page_for_test(), 0, "the page should be held back");
        assert_eq!(
            view.grid_for_test().read(cx).staged(cx).len(),
            1,
            "the edit should still be there while the question stands"
        );
    });

    // Saying no leaves everything where it was.
    click_in_session(cx, handle, "keep-edits");
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        assert_eq!(view.page_for_test(), 0);
        assert_eq!(view.grid_for_test().read(cx).staged(cx).len(), 1);
    });

    // Saying yes throws the edit away and turns the page.
    view.update(cx, |view, cx| view.go_for_test(1, cx));
    cx.run_until_parked();
    click_in_session(cx, handle, "discard-and-continue");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(view.page_for_test(), 1, "the page should turn now");
        assert!(view.grid_for_test().read(cx).staged(cx).is_empty());
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "paging away from an edit must not write it"
    );
}

#[gpui_kit::test]
fn refresh_asks_before_it_discards_staged_edits(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");

    handle
        .update(cx, |session, _, cx| session.refresh(cx))
        .unwrap();
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .len()),
        1,
        "refresh should ask rather than reread the rows under the edit"
    );

    click_in_session(cx, handle, "discard-and-continue");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert!(view.grid_for_test().read(cx).staged(cx).is_empty());
        assert_eq!(
            view.grid_for_test().read(cx).cell_for_test(0, 1, cx),
            Some("alpha".to_string()),
            "the page should be back to what the server has"
        );
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "a refused edit must not reach the server"
    );
}

#[gpui_kit::test]
fn sorting_asks_and_puts_the_header_back_when_it_is_refused(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");

    view.update(cx, |view, cx| {
        view.sort_for_test("name", ColumnSort::Descending, cx)
    });
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |view, _| view.pending_for_test()),
        "sorting should be held back while edits are waiting"
    );

    click_in_session(cx, handle, "keep-edits");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.grid_for_test().read(cx).sorted_for_test(cx),
            None,
            "a refused sort should leave the header on the order the rows are in"
        );
        assert_eq!(view.grid_for_test().read(cx).staged(cx).len(), 1);
    });
}

#[gpui_kit::test]
fn a_view_is_read_only(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.set_metadata_for_test(
                vec!["main".to_string()],
                vec![DatabaseObject {
                    schema: None,
                    name: "named_items".into(),
                    kind: ObjectKind::View,
                }],
                cx,
            );
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-named_items", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let view = handle
        .update(cx, |session, _, _| session.active_table_view())
        .unwrap()
        .expect("clicking a view should open a table view");

    view.update(cx, |view, cx| {
        assert!(
            !view.is_editable_for_test(),
            "a view has no rows of its own to write back"
        );
        assert!(!view.grid_for_test().read(cx).editable_for_test(cx));
    });
}

/// Run a statement through a second connection, the way another client would.
fn run_external(database: &TempDatabase, sql: &str) {
    runtime::block_on(async {
        let connection = Connection::open(database.config(), None)
            .await
            .expect("could not open a second connection to the test database");
        connection
            .run_query(sql)
            .await
            .expect("the statement failed");
        connection.close().await;
    });
}

/// Open a table tab on a table the seed data does not have, created here.
fn table_view_on(
    cx: &mut TestAppContext,
    name: &'static str,
    create: &str,
) -> (
    TempDatabase,
    gpui_kit::WindowHandle<Session>,
    gpui_kit::Entity<crate::ui::table_view::TableView>,
) {
    let (database, handle) = session_with_objects(cx);
    run_external(&database, create);

    handle
        .update(cx, |session, _, cx| {
            session.set_metadata_for_test(
                vec!["main".to_string()],
                vec![DatabaseObject {
                    schema: None,
                    name: name.into(),
                    kind: ObjectKind::Table,
                }],
                cx,
            );
        })
        .unwrap();

    let id = gpui_kit::SharedString::from(format!("object-{name}"));
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(id, cx);
    })
    .unwrap();

    let view = handle
        .update(cx, |session, _, _| session.active_table_view())
        .unwrap()
        .expect("clicking a table should open a table view");

    (database, handle, view)
}

#[gpui_kit::test]
fn a_table_without_a_primary_key_is_written_by_rowid(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view_on(
        cx,
        "notes",
        "create table notes (body text); insert into notes values ('first')",
    );
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.row_key_for_test(),
            Some(crate::db::RowKey::RowId("rowid")),
            "a table with no primary key falls back to the engine's row id"
        );
        assert!(view.is_editable_for_test());
        assert_eq!(
            view.query(cx),
            "select rowid, * from notes limit 500 offset 0",
            "the row id has to be asked for by name: select * leaves it out"
        );
        // It is asked for so rows can be addressed, not so it can be shown.
        assert_eq!(
            view.grid_for_test().read(cx).cell_for_test(0, 0, cx),
            Some("first".to_string()),
            "the row id column should be taken back out before the grid sees it"
        );
    });

    stage_cell(cx, &view, 0, 0, "rewritten");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    let body = runtime::block_on(async {
        let connection = Connection::open(database.config(), None)
            .await
            .expect("could not reopen the test database");
        let result = connection
            .run_query("select body from notes")
            .await
            .expect("could not read the row");
        connection.close().await;
        result.rows[0][0].clone()
    });
    assert_eq!(
        body,
        Some("rewritten".to_string()),
        "a row addressed by its row id should still be written"
    );
}

#[gpui_kit::test]
fn a_binary_cell_cannot_be_typed_into(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // `payload` is a BLOB, and the grid only holds a description of it.
    stage_cell(cx, &view, 0, 3, "not a blob");

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.read_with(cx, |grid, cx| {
        assert!(
            grid.staged(cx).is_empty(),
            "a value the grid never read back must not be writable"
        );
        assert_eq!(
            grid.cell_for_test(0, 3, cx),
            Some("<3 bytes>".to_string()),
            "the cell should still show what the driver said about it"
        );
    });
}

#[gpui_kit::test]
fn escape_closes_the_editor_without_staging(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "abandoned");
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.editing_for_test(cx)),
        Some((0, 1)),
        "the editor should be open on the cell when escape arrives"
    );

    press(cx, handle, "escape");
    cx.run_until_parked();

    grid.read_with(cx, |grid, cx| {
        assert!(
            grid.staged(cx).is_empty(),
            "escape should throw away what was being typed"
        );
        assert_eq!(
            grid.cell_for_test(0, 1, cx),
            Some("alpha".to_string()),
            "the cell should show the loaded value again"
        );
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "a cancelled edit must not reach the server"
    );
}

#[gpui_kit::test]
fn a_row_that_changed_underneath_is_reported_rather_than_written(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "too late");
    // The row is gone by the time the write runs, so nothing matches it.
    run_external(&database, "delete from items where id = 1");

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    let error = view
        .read_with(cx, |view, _| view.error_for_test())
        .expect("a write that matched no rows should be reported");
    assert!(
        error.contains("matched 0 rows"),
        "the error should say what the write did: {error}"
    );
    assert!(
        !view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .is_empty()),
        "a write that failed should leave the edit staged to retry"
    );
}

#[gpui_kit::test]
fn typing_null_means_sql_null_only_when_the_setting_says_so(cx: &mut TestAppContext) {
    let (database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // Off by default: an edit means the four characters that were typed.
    stage_cell(cx, &view, 0, 1, "NULL");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("NULL".to_string()),
        "with the setting off, NULL is text"
    );

    cx.update(|cx| settings::update(cx, |settings| settings.coerce_null_literal = true));
    stage_cell(cx, &view, 0, 1, "null");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        None,
        "with the setting on, typing null stores SQL NULL"
    );
}

/// Select a cell the way clicking one does, with the grid holding the focus.
fn select_cell(
    cx: &mut TestAppContext,
    view: &gpui_kit::Entity<crate::ui::table_view::TableView>,
    row: usize,
    col: usize,
) {
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.downgrade()
        .update_in(cx, |grid, window, cx| {
            grid.focus_for_test(window, cx);
            grid.select_cell_for_test(row, col, cx);
        })
        .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn enter_opens_the_editor_on_the_selected_cell(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 0, 1);
    press(cx, handle, "enter");
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .editing_for_test(cx)),
        Some((0, 1)),
        "enter should open the editor on the selected cell"
    );
}

#[gpui_kit::test]
fn applying_while_typing_folds_the_cell_in(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    // The editor keeps the focus here, so the save key has to reach the table
    // view from inside the input rather than from the grid.
    stage_cell(cx, &view, 0, 1, "mid-edit");
    press(cx, handle, "secondary-s");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("mid-edit".to_string()),
        "applying while a cell is open should write what is being typed"
    );
}

#[gpui_kit::test]
fn the_null_shortcut_stages_sql_null(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 0, 1);
    press(cx, handle, "secondary-shift-n");
    cx.run_until_parked();

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        None,
        "the null shortcut should stage SQL NULL"
    );
}

#[gpui_kit::test]
fn a_read_only_connection_cannot_edit_the_grid(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view_with_safety(cx, SafetyMode::ReadOnly);
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert!(
            !view.is_editable_for_test(),
            "a read-only connection writes nothing"
        );
        assert!(!view.grid_for_test().read(cx).editable_for_test(cx));
    });
}

#[gpui_kit::test]
fn a_read_only_connection_reports_what_the_editor_refused(cx: &mut TestAppContext) {
    let (database, handle) = session_with_safety(cx, SafetyMode::ReadOnly);

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test(
                "insert into items values (5, 'five', 5.0, NULL)",
                window,
                cx,
            );
        })
        .unwrap();
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    let status = handle
        .update(cx, |session, _, _| session.active_status_for_test())
        .unwrap();
    assert!(
        status.contains("read-only") && status.contains("INSERT"),
        "the status bar should say what was refused: {status}"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "the seed rows should be untouched"
    );
}

#[gpui_kit::test]
fn a_confirming_connection_shows_the_update_before_it_runs(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view_with_safety(cx, SafetyMode::ConfirmWrites);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "confirmed");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, _| view.confirming_for_test()),
        Some("1 edited row".to_string()),
        "the panel should say what is about to be applied"
    );
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "nothing should have been sent yet"
    );

    // Saying no leaves the edit staged to try again.
    click_in_session(cx, handle, "cancel-write");
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        assert!(view.confirming_for_test().is_none());
        assert_eq!(view.grid_for_test().read(cx).staged(cx).len(), 1);
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "cancelling must not write"
    );

    // Saying yes runs it.
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    click_in_session(cx, handle, "confirm-write");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("confirmed".to_string()),
        "confirming should write the row"
    );
    assert!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .is_empty()),
        "a written row should no longer be staged"
    );
}

#[gpui_kit::test]
fn a_confirming_connection_asks_before_running_a_write_from_the_editor(cx: &mut TestAppContext) {
    let (database, handle) = session_with_safety(cx, SafetyMode::ConfirmWrites);

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test(
                "insert into items values (5, 'five', 5.0, NULL)",
                window,
                cx,
            );
        })
        .unwrap();
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    assert_eq!(
        handle
            .update(cx, |session, _, _| session.active_status_for_test())
            .unwrap(),
        "This statement writes. Run it?"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "the statement should be held until it is confirmed"
    );

    click_in_session(cx, handle, "confirm-run");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        3,
        "confirming should run the statement"
    );
}

#[gpui_kit::test]
fn a_confirming_connection_leaves_a_cancelled_statement_unrun(cx: &mut TestAppContext) {
    let (database, handle) = session_with_safety(cx, SafetyMode::ConfirmWrites);

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("delete from items", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    click_in_session(cx, handle, "cancel-run");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "cancelling must not run the statement"
    );
    assert_eq!(
        handle
            .update(cx, |session, _, _| session.active_status_for_test())
            .unwrap(),
        "Not run"
    );
}

#[gpui_kit::test]
fn a_read_from_the_editor_is_never_confirmed(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_safety(cx, SafetyMode::ConfirmWrites);

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("select * from items", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    let status = handle
        .update(cx, |session, _, _| session.active_status_for_test())
        .unwrap();
    assert!(
        !status.contains("Run it?"),
        "a select should run without being asked about: {status}"
    );
}

/// Click the pick box beside `row_ix`, the way a user checks a row out.
fn pick_row(cx: &mut TestAppContext, handle: WindowHandle<Session>, row_ix: usize) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(("pick", row_ix), cx);
    })
    .unwrap();
}

/// Shift-click `row_ix`'s pick box, which takes everything between it and the
/// last row picked.
fn pick_through(cx: &mut TestAppContext, handle: WindowHandle<Session>, row_ix: usize) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        let position = window.find(("pick", row_ix)).bounds().center();
        let modifiers = Modifiers {
            shift: true,
            ..Default::default()
        };

        window.dispatch_event(
            MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count: 1,
                first_mouse: false,
            }
            .to_platform_input(),
            cx,
        );
        window.dispatch_event(
            MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers,
                click_count: 1,
            }
            .to_platform_input(),
            cx,
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn checking_rows_selects_them_and_shift_takes_the_range(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    pick_row(cx, handle, 0);
    pick_through(cx, handle, 1);

    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .rows_selected_for_test(cx)),
        vec![0, 1],
        "the pick boxes of the two rows should both be checked"
    );

    // Checking one that is already checked takes it back out.
    pick_row(cx, handle, 1);
    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .rows_selected_for_test(cx)),
        vec![0],
        "checking a checked row should uncheck it"
    );
}

#[gpui_kit::test]
fn deleting_rows_marks_them_until_the_changes_are_applied(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    pick_row(cx, handle, 0);
    pick_through(cx, handle, 1);
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.delete_selected(cx));
    cx.run_until_parked();

    // The rows stay on screen, marked, until the changes are applied.
    view.update(cx, |view, cx| {
        assert_eq!(view.grid_for_test().read(cx).deletions(cx), vec![0, 1]);
        assert_eq!(
            view.grid_for_test().read(cx).pending_counts(cx),
            (0, 0, 2),
            "two rows are waiting to be deleted"
        );
    });
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "nothing should be deleted yet"
    );

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        0,
        "applying should delete both rows"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.loaded_rows_for_test()),
        0,
        "the page should be read again once the rows are gone"
    );
}

#[gpui_kit::test]
fn a_marked_row_can_be_restored_or_discarded(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());

    // Restoring takes the mark off the row again.
    pick_row(cx, handle, 0);
    grid.update(cx, |grid, cx| grid.delete_selected(cx));
    grid.update(cx, |grid, cx| grid.restore_selected(cx));
    cx.run_until_parked();
    assert!(grid.read_with(cx, |grid, cx| grid.deletions(cx).is_empty()));

    // So does throwing the staged changes away.
    grid.update(cx, |grid, cx| grid.delete_selected(cx));
    cx.run_until_parked();
    focus_grid(cx, &view);
    press(cx, handle, "secondary-z");
    cx.run_until_parked();

    assert!(grid.read_with(cx, |grid, cx| grid.deletions(cx).is_empty()));
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "a discarded delete must never reach the server"
    );
}

#[gpui_kit::test]
fn the_delete_shortcut_marks_the_selected_rows(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    pick_row(cx, handle, 0);
    pick_through(cx, handle, 1);
    focus_grid(cx, &view);
    press(cx, handle, "secondary-backspace");
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |view, cx| view.grid_for_test().read(cx).deletions(cx)),
        vec![0, 1]
    );

    focus_grid(cx, &view);
    press(cx, handle, "secondary-shift-backspace");
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .deletions(cx)
            .is_empty()),
        "the restore shortcut should take the mark off again"
    );
}

#[gpui_kit::test]
fn a_read_only_connection_deletes_nothing(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view_with_safety(cx, SafetyMode::ReadOnly);
    cx.run_until_parked();

    pick_row(cx, handle, 0);
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.delete_selected(cx));
    cx.run_until_parked();

    assert!(
        grid.read_with(cx, |grid, cx| grid.deletions(cx).is_empty()),
        "a read-only grid marks nothing"
    );
    assert_eq!(runtime::block_on(other_count(&database)), 2);
}

#[gpui_kit::test]
fn a_new_row_is_filled_in_and_inserted(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.row_count_for_test(cx)),
        3,
        "the new row should sit below the ones the server sent"
    );

    // The new row is the third one, after the two the seed data has.
    stage_cell(cx, &view, 2, 0, "7");
    stage_cell(cx, &view, 2, 1, "seven");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 7)),
        Some("seven".to_string()),
        "applying should insert the row"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.loaded_rows_for_test()),
        3,
        "the page should be read again so the row comes back from the server"
    );
}

#[gpui_kit::test]
fn a_column_nobody_typed_into_is_left_to_the_server(cx: &mut TestAppContext) {
    // `items.id` is a SQLite row id, so leaving it out is what gets one.
    let (database, handle, view) = table_view_with_safety(cx, SafetyMode::ConfirmWrites);
    cx.run_until_parked();

    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();
    stage_cell(cx, &view, 2, 1, "eight");
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, _| view.confirming_for_test()),
        Some("1 new row".to_string()),
        "the panel should say what is about to be applied"
    );

    click_in_session(cx, handle, "confirm-write");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        3,
        "confirming should insert the row"
    );
    // The key was left out of the statement, so the server picked one.
    assert_eq!(
        runtime::block_on(id_of(&database, "eight")),
        Some("3".to_string()),
        "the column nobody typed into should come from the server"
    );
}

#[gpui_kit::test]
fn an_empty_new_row_is_reported_rather_than_written(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();
    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    let error = view
        .read_with(cx, |view, _| view.error_for_test())
        .expect("an empty row should be reported");
    assert!(error.contains("empty"), "{error}");
    assert_eq!(runtime::block_on(other_count(&database)), 2);
}

#[gpui_kit::test]
fn a_new_row_can_be_thrown_away(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();
    stage_cell(cx, &view, 2, 1, "never inserted");

    // Its own menu item drops the one row.
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.discard_draft_for_test(2, cx));
    cx.run_until_parked();

    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.row_count_for_test(cx)),
        2,
        "the row should be gone from the grid"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "a discarded row must never reach the server"
    );

    // So does the discard key, for a row left half-filled.
    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();
    focus_grid(cx, &view);
    press(cx, handle, "secondary-z");
    cx.run_until_parked();
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.row_count_for_test(cx)),
        2
    );
}

#[gpui_kit::test]
fn the_insert_shortcut_adds_a_row(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    focus_grid(cx, &view);
    press(cx, handle, "secondary-shift-i");
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .row_count_for_test(cx)),
        3
    );
}

// The real right-click path — pointer over a cell, menu opens, item chosen —
// was checked by hand against a running grid and by a throwaway test: the
// assertions passed, but `PopupMenu` keeps itself alive through the
// subscription its own context menu registers, so every such test ends in the
// harness's leaked-entity panic. The menu's own logic is covered through
// `request_delete_for_test` and `discard_draft_for_test`, which call exactly
// what the items call.

#[gpui_kit::test]
fn changes_of_every_kind_are_counted_and_applied_together(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    // Edit the first row, mark the second for deletion, add a third.
    stage_cell(cx, &view, 0, 1, "edited");
    pick_row(cx, handle, 1);
    focus_grid(cx, &view);
    press(cx, handle, "secondary-backspace");
    click_in_session(cx, handle, "insert-row");
    cx.run_until_parked();
    // Two rows came from the server, so the new one is the third.
    stage_cell(cx, &view, 2, 1, "added");

    view.update(cx, |view, cx| {
        assert_eq!(
            view.grid_for_test().read(cx).pending_counts(cx),
            (1, 1, 1),
            "one edited row, one new row, one deleted row"
        );
    });

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("edited".to_string()),
        "the edit should have been written"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "one row went, one row arrived"
    );
    assert_eq!(
        runtime::block_on(id_of(&database, "added")),
        Some("3".to_string()),
        "the new row should be there"
    );
    assert!(
        view.read_with(cx, |view, cx| view.grid_for_test().read(cx).pending(cx)
            == 0),
        "nothing should be left staged"
    );
}

#[gpui_kit::test]
fn an_edit_on_a_row_marked_for_deletion_goes_with_it(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");
    pick_row(cx, handle, 0);
    focus_grid(cx, &view);
    press(cx, handle, "secondary-backspace");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.grid_for_test().read(cx).pending_counts(cx),
            (0, 0, 1),
            "the row counts once, as a deletion"
        );
    });

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        1,
        "the row should be gone"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.error_for_test()),
        None,
        "updating a row that is being deleted should never be attempted"
    );
}

/// Put one saved SQLite connection in the manager's list.
fn saved_connection(
    cx: &mut TestAppContext,
    handle: WindowHandle<Workspace>,
) -> (TempDatabase, Uuid) {
    let database = runtime::block_on(TempDatabase::new());
    let config = database.config();
    let id = config.id;

    let welcome = handle
        .update(cx, |workspace, _, _| workspace.active_welcome_for_test())
        .unwrap()
        .expect("the active tab should be showing the connection manager");
    welcome.update(cx, |welcome, cx| {
        welcome.set_connections_for_test(vec![config], cx)
    });

    (database, id)
}

#[gpui_kit::test]
fn double_clicking_a_saved_connection_opens_it(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let (_database, id) = saved_connection(cx, handle);
    let target = gpui_kit::SharedString::from(format!("open-{id}"));

    // One click only fills the form in, so the tab is still the manager.
    cx.update_window(handle.into(), |_, window, cx| {
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

    cx.update_window(handle.into(), |_, window, cx| {
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

/// Add a filter to the open table view, the way filling in a line of the bar
/// does, and wait for the page it re-reads.
fn add_filter(
    cx: &mut TestAppContext,
    view: &gpui_kit::Entity<crate::ui::table_view::TableView>,
    column: &str,
    operator: Operator,
    value: &str,
) {
    let filters = view.read_with(cx, |view, _| view.filters_for_test());
    let (column, value) = (column.to_string(), value.to_string());
    filters
        .downgrade()
        .update_in(cx, |filters, window, cx| {
            filters.add_filter_for_test(&column, operator, &value, window, cx);
        })
        .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_filter_narrows_the_rows_the_table_shows(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    add_filter(cx, &view, "name", Operator::Equals, "alpha");

    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where name = ? limit 500 offset 0",
            "the value belongs in a parameter, not in the statement"
        );
        assert_eq!(view.loaded_rows_for_test(), 1, "only one row says alpha");
    });
}

#[gpui_kit::test]
fn filters_cover_the_operators_they_offer(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // `IS NULL` has no value to bind, so nothing is added to the statement.
    add_filter(cx, &view, "name", Operator::IsNull, "");
    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where name is null limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1, "one row has no name");
    });

    let filters = view.read_with(cx, |view, _| view.filters_for_test());
    filters.update(cx, |filters, cx| filters.clear(cx));
    cx.run_until_parked();

    // A pattern is text whatever the column holds, so the column is cast.
    add_filter(cx, &view, "id", Operator::Like, "1%");
    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where cast(id as text) like ? limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1);
    });

    filters.update(cx, |filters, cx| filters.clear(cx));
    cx.run_until_parked();

    add_filter(cx, &view, "id", Operator::GreaterOrEqual, "2");
    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where id >= ? limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1);
    });
}

#[gpui_kit::test]
fn an_in_filter_takes_a_subquery(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // `IN` and `NOT IN` are the one place a value is SQL: it goes into the
    // statement as written so a subquery can do the filtering.
    add_filter(
        cx,
        &view,
        "id",
        Operator::In,
        "select id from items where name is not null",
    );

    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where id in (select id from items where name is not null) \
             limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1, "one row has a name");
        assert_eq!(view.error_for_test(), None);
    });
}

#[gpui_kit::test]
fn several_filters_are_joined_with_and(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    add_filter(cx, &view, "id", Operator::Greater, "0");
    add_filter(cx, &view, "name", Operator::NotEquals, "alpha");

    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where id > ? and name <> ? limit 500 offset 0"
        );
        // The row with a NULL name is not `<> 'alpha'` — SQL says nothing
        // about it — so neither row comes back.
        assert_eq!(view.loaded_rows_for_test(), 0);
    });
}

#[gpui_kit::test]
fn an_unfinished_filter_narrows_nothing(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // A line with no value typed into it yet is still being written.
    add_filter(cx, &view, "name", Operator::Equals, "");

    view.update(cx, |view, cx| {
        assert_eq!(view.query(cx), "select * from items limit 500 offset 0");
        assert_eq!(view.loaded_rows_for_test(), 2);
    });

    let filters = view.read_with(cx, |view, _| view.filters_for_test());
    filters.read_with(cx, |filters, cx| {
        assert_eq!(
            filters.filter_count_for_test(),
            1,
            "the line is still there"
        );
        assert!(
            filters.shows_value_for_test(0, cx),
            "a filter that needs a value should show the box for it"
        );
    });
}

#[gpui_kit::test]
fn a_null_filter_hides_its_value_box(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    add_filter(cx, &view, "name", Operator::IsNotNull, "");

    let filters = view.read_with(cx, |view, _| view.filters_for_test());
    filters.read_with(cx, |filters, cx| {
        assert!(
            !filters.shows_value_for_test(0, cx),
            "IS NOT NULL has nothing to compare against"
        );
    });
}

#[gpui_kit::test]
fn filtering_asks_before_it_discards_staged_edits(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");
    add_filter(cx, &view, "name", Operator::Equals, "alpha");

    view.update(cx, |view, cx| {
        assert!(view.pending_for_test(), "filtering should be held back");
        assert_eq!(view.grid_for_test().read(cx).staged(cx).len(), 1);
    });

    click_in_session(cx, handle, "discard-and-continue");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.loaded_rows_for_test(),
            1,
            "the filter should be in force"
        );
        assert!(view.grid_for_test().read(cx).staged(cx).is_empty());
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "a discarded edit must not reach the server"
    );
}

/// What the grid asked to be shown in the value dialog.
fn pending_value(cx: &mut TestAppContext) -> crate::ui::value_dialog::ValueRequest {
    cx.update(|cx| crate::ui::value_dialog::pending(cx))
        .expect("no value is waiting to be shown")
}

#[gpui_kit::test]
fn a_value_is_handed_to_the_dialog(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    cx.run_until_parked();

    let request = pending_value(cx);
    assert_eq!(request.column, "name");
    assert_eq!(request.text, "alpha");
    assert!(
        request.save.is_some(),
        "a row of a writable table can be edited in the dialog"
    );
}

#[gpui_kit::test]
fn the_value_shortcut_hands_over_the_selected_cell(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    select_cell(cx, &view, 1, 1);
    press(cx, handle, "secondary-shift-v");
    cx.run_until_parked();

    let request = pending_value(cx);
    assert_eq!(request.column, "name");
    assert_eq!(request.text, "NULL", "the second row has no name");
}

#[gpui_kit::test]
fn json_is_laid_out_and_a_query_result_cannot_be_edited(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(
                QueryResult {
                    columns: vec!["document".into()],
                    column_types: vec!["JSONB".into()],
                    rows: vec![vec![Some(r#"{"name":"alpha","tags":[1,2]}"#.into())]],
                    ..QueryResult::default()
                },
                cx,
            );
        })
        .unwrap();

    let grid = handle
        .update(cx, |session, _, _| session.active_grid())
        .unwrap()
        .expect("a query tab has a grid");
    grid.update(cx, |grid, cx| grid.view_cell(0, 0, cx));
    cx.run_until_parked();

    let request = pending_value(cx);
    assert_eq!(request.column, "document");
    assert_eq!(
        request.text, "{\n  \"name\": \"alpha\",\n  \"tags\": [\n    1,\n    2\n  ]\n}",
        "JSON should be laid out over several lines"
    );
    assert!(
        request.save.is_none(),
        "a query result has no owner that could write it back"
    );
}

#[gpui_kit::test]
fn a_value_that_was_never_read_says_so(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // `payload` is a BLOB, so the grid only ever held a description of it.
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 3, cx));
    cx.run_until_parked();

    let request = pending_value(cx);
    assert!(request.text.contains("not read back"));
    assert!(request.save.is_none(), "there is nothing to write back");
}

/// Open a table tab inside a workspace, which is what builds the dialog.
fn workspace_table(
    cx: &mut TestAppContext,
) -> (
    TempDatabase,
    WindowHandle<Workspace>,
    gpui_kit::Entity<crate::ui::table_view::TableView>,
) {
    let handle = workspace(cx);
    let database = connect(cx, handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the active tab should be a session");
    cx.run_until_parked();

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("object-items", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let view = session
        .read_with(cx, |session, _| session.active_table_view())
        .expect("clicking a table should open a table view");

    (database, handle, view)
}

/// Draw the workspace, which is what picks the value up and builds the dialog.
fn draw_workspace(cx: &mut TestAppContext, handle: WindowHandle<Workspace>) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_dialog_saves_with_the_platform_shortcut(cx: &mut TestAppContext) {
    let (database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    draw_workspace(cx, handle);

    let dialog = handle
        .update(cx, |workspace, _, _| workspace.value_dialog_for_test())
        .unwrap()
        .expect("the dialog should be open over the window");
    dialog.read_with(cx, |dialog, cx| {
        assert_eq!(dialog.column_for_test(), "name");
        assert_eq!(dialog.value_for_test(cx), "alpha");
        assert!(dialog.is_editable_for_test());
    });

    dialog
        .downgrade()
        .update_in(cx, |dialog, window, cx| {
            dialog.set_value_for_test("from the dialog", window, cx);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("secondary-enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        handle
            .update(cx, |workspace, _, _| workspace
                .value_dialog_for_test()
                .is_none())
            .unwrap(),
        "saving should close the dialog"
    );
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.cell_for_test(0, 1, cx)),
        Some("from the dialog".to_string()),
        "the saved value should be staged in the cell"
    );

    view.update(cx, |view, cx| view.commit(cx));
    cx.run_until_parked();
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("from the dialog".to_string()),
        "applying should write what the dialog staged"
    );
}

#[gpui_kit::test]
fn escape_closes_the_dialog_without_saving(cx: &mut TestAppContext) {
    let (database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    draw_workspace(cx, handle);

    let dialog = handle
        .update(cx, |workspace, _, _| workspace.value_dialog_for_test())
        .unwrap()
        .expect("the dialog should be open");
    dialog
        .downgrade()
        .update_in(cx, |dialog, window, cx| {
            dialog.set_value_for_test("thrown away", window, cx);
        })
        .unwrap();

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("escape", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        handle
            .update(cx, |workspace, _, _| workspace
                .value_dialog_for_test()
                .is_none())
            .unwrap(),
        "escape should close the dialog"
    );
    assert!(
        grid.read_with(cx, |grid, cx| grid.staged(cx).is_empty()),
        "closing must not stage anything"
    );
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string())
    );
}

#[gpui_kit::test]
fn a_click_inside_the_dialog_never_reaches_the_grid(cx: &mut TestAppContext) {
    let (_database, handle, view) = workspace_table(cx);

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    // Put the grid's selection on the first row, the way a user just had it.
    grid.downgrade()
        .update_in(cx, |grid, window, cx| {
            grid.focus_for_test(window, cx);
            grid.select_cell_for_test(0, 1, cx);
        })
        .unwrap();
    cx.run_until_parked();

    let before = grid.read_with(cx, |grid, cx| grid.selection_for_test(cx));

    grid.update(cx, |grid, cx| grid.view_cell(0, 1, cx));
    draw_workspace(cx, handle);

    // Clicking into the dialog's box would land on whatever grid cell lies
    // under it, if the overlay let the click through.
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("value-dialog-body", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        handle
            .update(cx, |workspace, _, _| workspace
                .value_dialog_for_test()
                .is_some())
            .unwrap(),
        "clicking inside the dialog must not close it"
    );
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.selection_for_test(cx)),
        before,
        "the grid behind the dialog must not receive the click"
    );
}

#[gpui_kit::test]
fn double_clicking_picks_the_editor_or_the_dialog(_cx: &mut TestAppContext) {
    use crate::db::query::{blob, unsupported};
    use crate::ui::data_grid::needs_a_window;

    // Short, plain values are typed into the cell itself.
    assert!(!needs_a_window(&Some("alpha".to_string()), "TEXT"));
    assert!(!needs_a_window(&None, "TEXT"));

    // Everything a cell cannot show goes to the dialog.
    assert!(needs_a_window(&Some("one\ntwo".to_string()), "TEXT"));
    assert!(needs_a_window(&Some("x".repeat(201)), "TEXT"));
    assert!(needs_a_window(&Some(r#"{"a":1}"#.to_string()), "TEXT"));
    assert!(needs_a_window(&Some("[1, 2]".to_string()), "JSONB"));
    assert!(needs_a_window(&blob(b"abc"), "BLOB"));
    assert!(needs_a_window(&unsupported("XML"), "XML"));

    // A value that only looks like JSON is still a value.
    assert!(!needs_a_window(&Some("{not json".to_string()), "TEXT"));
}

/// Put `sql` in the active editor and the caret at `cursor`.
fn prepare_editor(
    cx: &mut TestAppContext,
    handle: WindowHandle<Session>,
    sql: &str,
    cursor: usize,
) {
    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test(sql, window, cx);
            if let Some(editor) = session.active_editor_for_test() {
                editor.update(cx, |editor, cx| editor.set_cursor_for_test(cursor, cx));
            }
        })
        .unwrap();
}

#[gpui_kit::test]
fn the_run_shortcut_sends_only_the_statement_the_caret_is_in(cx: &mut TestAppContext) {
    let (database, handle) = session_with_objects(cx);

    let sql = "insert into items values (3, 'gamma', 3.0, NULL);\n\
               insert into items values (4, 'delta', 4.0, NULL);";
    // The caret sits in the second statement.
    prepare_editor(cx, handle, sql, sql.len());
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        3,
        "only the statement the caret is in should have run"
    );
    assert_eq!(
        runtime::block_on(name_of(&database, 4)),
        Some("delta".to_string())
    );
}

#[gpui_kit::test]
fn a_selection_is_what_runs(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let sql = "select * from items;\nselect 1;";
    prepare_editor(cx, handle, sql, 0);
    handle
        .update(cx, |session, _, cx| {
            let editor = session
                .active_editor_for_test()
                .expect("a query tab has an editor");
            editor.update(cx, |editor, cx| {
                // Select the second statement, caret notwithstanding.
                editor.select_for_test(21..30, cx);
                // A selection runs as written, semicolon and all.
                assert_eq!(editor.statement_for_test(cx).as_deref(), Some("select 1;"));
            });
        })
        .unwrap();
}

#[gpui_kit::test]
fn the_script_shortcut_runs_every_statement(cx: &mut TestAppContext) {
    let (database, handle) = session_with_objects(cx);

    let sql = "insert into items values (3, 'gamma', 3.0, NULL);\n\
               select * from items;\n\
               select name from items where id = 1;";
    prepare_editor(cx, handle, sql, 0);
    press(cx, handle, "secondary-shift-enter");
    cx.run_until_parked();

    assert_eq!(
        runtime::block_on(other_count(&database)),
        3,
        "the insert should have run as part of the script"
    );

    let (results, shown) = handle
        .update(cx, |session, _, _| session.results_for_test())
        .unwrap();
    assert_eq!(results, 3, "one result per statement");
    assert_eq!(shown, 0, "the first one is showing");

    let status = handle
        .update(cx, |session, _, _| session.active_status_for_test())
        .unwrap();
    assert!(
        status.starts_with("3 statements in") && status.contains("result 1 of 3"),
        "the banner should count the statements: {status}"
    );
}

#[gpui_kit::test]
fn a_result_tab_shows_that_statements_rows(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(
        cx,
        handle,
        "select * from items;\nselect name from items where id = 1;",
        0,
    );
    press(cx, handle, "secondary-shift-enter");
    cx.run_until_parked();

    click_in_session(cx, handle, "result-1");
    cx.run_until_parked();

    let (_, shown) = handle
        .update(cx, |session, _, _| session.results_for_test())
        .unwrap();
    assert_eq!(shown, 1, "the second result should be showing");

    let grid = handle
        .update(cx, |session, _, _| session.active_grid())
        .unwrap()
        .expect("a query tab has a grid");
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.column_values_for_test(0, cx)),
        vec![Some("alpha".to_string())],
        "the grid should hold the second statement's rows"
    );
}

#[gpui_kit::test]
fn a_write_reports_the_rows_it_changed(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "update items set score = 9.0", 0);
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    let status = handle
        .update(cx, |session, _, _| session.active_status_for_test())
        .unwrap();
    assert!(
        status.starts_with("2 rows affected in"),
        "a write should say what it changed: {status}"
    );
}

#[gpui_kit::test]
fn cancelling_stops_waiting_on_a_query(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // Something has to hold the caret for a keystroke to reach the session.
    prepare_editor(cx, handle, "select 1", 0);

    // A query finishes before anything can be cancelled under test, so the
    // running state is set by hand and the cancel path driven from there.
    handle
        .update(cx, |session, _, cx| session.mark_running_for_test(cx))
        .unwrap();
    assert!(
        handle
            .update(cx, |session, _, _| session.active_is_running())
            .unwrap()
    );

    press(cx, handle, "secondary-.");
    cx.run_until_parked();

    assert_eq!(
        handle
            .update(cx, |session, _, _| session.active_status_for_test())
            .unwrap(),
        "Cancelled"
    );
    assert!(
        !handle
            .update(cx, |session, _, _| session.active_is_running())
            .unwrap(),
        "the editor should stop saying it is running"
    );
}
