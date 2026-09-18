//! Headless UI tests: render the real views in a test window and check the
//! layout facts that are easy to break, such as controls being pushed out of
//! the window or a pane collapsing to zero height.
//!
//! One file per area, with the fixtures they share kept here: a test that
//! opens a session, types into a grid, or drives a dialog reaches for the same
//! few helpers whatever it is about.

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::scroll::ScrollbarMode;
use gpui_kit::component::table::ColumnSort;
use gpui_kit::component::{Root, Theme, ThemeMode};
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext as _, Bounds, Context, Entity, InputEvent as _, Modifiers, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, TestAppContext, Window, WindowHandle,
    point, px, size,
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

mod editing;
mod explain;
mod files;
mod filters;
mod layout;
mod navigation;
mod paging;
mod preferences;
mod quick_switcher;
mod row_panel;
mod rows;
mod running;
mod safety;
mod schema;
mod session;
mod sorting;
mod value_dialog;
mod workspace;

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
        .update(cx, |session, _, cx| session.active_table_view(cx))
        .unwrap()
        .expect("clicking a table should open a table view");

    (database, handle, view)
}

/// Open a structure tab on the seeded `items` table and hand back its view.
fn schema_view(
    cx: &mut TestAppContext,
) -> (
    TempDatabase,
    gpui_kit::WindowHandle<Session>,
    gpui_kit::Entity<crate::ui::schema_view::SchemaView>,
) {
    let (database, handle) = session_with_objects(cx);

    let object = DatabaseObject {
        schema: None,
        name: "items".into(),
        kind: ObjectKind::Table,
    };
    handle
        .update(cx, |session, window, cx| {
            session.open_schema_for_test(&object, window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    let view = handle
        .update(cx, |session, _, cx| session.active_schema_view(cx))
        .unwrap()
        .expect("opening a table's structure should open a schema view");

    (database, handle, view)
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

/// A workspace window, the way `main` opens one: the workspace inside a
/// `Root`, so the layer a dialog opens into is really there.
struct WorkspaceWindow {
    window: WindowHandle<Root>,
    view: Entity<Workspace>,
}

impl WorkspaceWindow {
    /// Reach the workspace itself, with the window the `Root` holds it in.
    fn update<R>(
        &self,
        cx: &mut TestAppContext,
        update: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) -> R,
    ) -> gpui_kit::Result<R> {
        self.window.update(cx, |_, window, cx| {
            self.view
                .update(cx, |workspace, cx| update(workspace, window, cx))
        })
    }
}

fn workspace(cx: &mut TestAppContext) -> WorkspaceWindow {
    cx.update(|cx| {
        gpui_kit::component::init(cx);
        crate::keymap::bind(cx);
    });

    let view = std::cell::RefCell::new(None);
    let window = cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
        let workspace = cx.new(|cx| Workspace::new(window, cx));
        *view.borrow_mut() = Some(workspace.clone());
        Root::new(workspace, window, cx)
    });
    let view = view.into_inner().expect("the workspace is built above");

    WorkspaceWindow { window, view }
}

/// Open a connection to a database of its own from the active tab, the way
/// the connection manager does when its Connect button succeeds.
fn connect(cx: &mut TestAppContext, handle: &WorkspaceWindow) -> TempDatabase {
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

fn titles(cx: &mut TestAppContext, handle: &WorkspaceWindow) -> Vec<String> {
    handle
        .update(cx, |workspace, _, cx| workspace.tab_titles_for_test(cx))
        .unwrap()
}

fn active(cx: &mut TestAppContext, handle: &WorkspaceWindow) -> usize {
    handle
        .update(cx, |workspace, _, _| workspace.active_for_test())
        .unwrap()
}

fn click(cx: &mut TestAppContext, handle: &WorkspaceWindow, id: &'static str) {
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(id, cx);
    })
    .unwrap();
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
        .update(cx, |session, _, cx| session.active_table_view(cx))
        .unwrap()
        .expect("clicking a table should open a table view");

    (database, handle, view)
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

/// The row the grid is highlighting, and the cell the selection sits on.
fn selection(
    cx: &mut TestAppContext,
    view: &gpui_kit::Entity<crate::ui::table_view::TableView>,
) -> (Option<usize>, Option<(usize, usize)>) {
    view.read_with(cx, |view, cx| {
        view.grid_for_test().read(cx).selection_for_test(cx)
    })
}

/// The rows picked out for a row command, in display order.
fn picked(
    cx: &mut TestAppContext,
    view: &gpui_kit::Entity<crate::ui::table_view::TableView>,
) -> Vec<usize> {
    view.read_with(cx, |view, cx| {
        view.grid_for_test().read(cx).rows_selected_for_test(cx)
    })
}

/// Whether the grid itself holds the keyboard, rather than a cell editor or a
/// checkbox that was clicked.
fn grid_focused(
    cx: &mut TestAppContext,
    handle: WindowHandle<Session>,
    view: &gpui_kit::Entity<crate::ui::table_view::TableView>,
) -> bool {
    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    cx.update_window(handle.into(), |_, window, cx| {
        grid.read(cx).is_focused_for_test(window, cx)
    })
    .unwrap()
}

/// What the app last put on the clipboard.
fn clipboard(cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
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

/// Drag from one row's pick box to another's, the way a run of rows is swept.
fn sweep_rows(cx: &mut TestAppContext, handle: WindowHandle<Session>, from: usize, to: usize) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        let from = window.find(("pick", from)).bounds().center();
        let to = window.find(("pick", to)).bounds().center();
        window.drag(from, to, cx);
    })
    .unwrap();
}

/// Click a row away from its pick box — on its first cell — with `modifiers`.
fn click_row(
    cx: &mut TestAppContext,
    handle: WindowHandle<Session>,
    row_ix: usize,
    modifiers: Modifiers,
) {
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        let position = window.find(("row", row_ix)).bounds().center();

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

// The real right-click path — pointer over a cell, menu opens, item chosen —
// was checked by hand against a running grid and by a throwaway test: the
// assertions passed, but `PopupMenu` keeps itself alive through the
// subscription its own context menu registers, so every such test ends in the
// harness's leaked-entity panic. The menu's own logic is covered through
// `request_delete_for_test` and `discard_draft_for_test`, which call exactly
// what the items call.

/// Put one saved SQLite connection in the manager's list.
fn saved_connection(cx: &mut TestAppContext, handle: &WorkspaceWindow) -> (TempDatabase, Uuid) {
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

/// What the grid asked to be shown in the value dialog.
fn pending_value(cx: &mut TestAppContext) -> crate::ui::value_dialog::ValueRequest {
    cx.update(|cx| crate::ui::value_dialog::pending(cx))
        .expect("no value is waiting to be shown")
}

/// Open a table tab inside a workspace, which is what builds the dialog.
fn workspace_table(
    cx: &mut TestAppContext,
) -> (
    TempDatabase,
    WorkspaceWindow,
    gpui_kit::Entity<crate::ui::table_view::TableView>,
) {
    let handle = workspace(cx);
    let database = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("the active tab should be a session");
    cx.run_until_parked();

    cx.update_window(handle.window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("object-items", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let view = session
        .read_with(cx, |session, cx| session.active_table_view(cx))
        .expect("clicking a table should open a table view");

    (database, handle, view)
}

/// Draw the workspace, which is what picks the value up and builds the dialog.
fn draw_workspace(cx: &mut TestAppContext, handle: &WorkspaceWindow) {
    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
    cx.run_until_parked();
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
            if let Some(editor) = session.active_editor_for_test(cx) {
                editor.update(cx, |editor, cx| editor.set_cursor_for_test(cursor, cx));
            }
        })
        .unwrap();
}
