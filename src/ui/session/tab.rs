//! What a session tab holds, and how the last run of it went.

use std::path::PathBuf;

use gpui_kit::{Entity, SharedString};

use crate::db::DatabaseObject;
use crate::db::query::QueryResult;
use crate::ui::data_grid::DataGrid;
use crate::ui::query_editor::QueryEditor;
use crate::ui::schema_view::SchemaView;
use crate::ui::table_view::TableView;

/// Whether a sidebar object is opened as its rows or its own definition.
///
/// A table's data tab and its structure tab are different tabs, so opening
/// either one has to know which of the two an already-open tab is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObjectViewMode {
    Data,
    Schema,
}

pub(crate) enum Status {
    Idle,
    Running,
    Done(String),
    Error(String),
    /// A statement that writes, held back on a connection that confirms
    /// writes. The buffer it came from is what the user reads; this is the
    /// copy that runs if they say yes.
    Confirm(String),
}

impl Status {
    /// What the status bar says.
    ///
    /// An error names itself rather than relying on the colour it is drawn
    /// in, which is the only thing telling it apart from a summary otherwise.
    pub(crate) fn message(&self) -> String {
        match self {
            Status::Idle => "Ready".to_string(),
            Status::Running => "Running…".to_string(),
            Status::Done(summary) => summary.clone(),
            Status::Error(error) => format!("Error: {error}"),
            Status::Confirm(_) => "This statement writes. Run it?".to_string(),
        }
    }
}

/// What a tab holds: a query editor with its result, or a table opened from
/// the sidebar.
pub(crate) enum TabContent {
    Query {
        editor: Entity<QueryEditor>,
        grid: Entity<DataGrid>,
        status: Status,
        /// The SQL file the buffer was read from or last written to.
        path: Option<PathBuf>,
        /// Every result the last run produced. A script that selects twice
        /// leaves two here, and the grid shows one of them at a time.
        results: Vec<QueryResult>,
        /// Which of them the grid is showing.
        result: usize,
        /// Handle on the run in flight, so it can be given up on.
        running: Option<tokio::task::AbortHandle>,
        /// The buffer's content as last opened or saved, to tell dirty from
        /// clean.
        baseline: String,
    },
    Table {
        view: Entity<TableView>,
    },
    Schema {
        view: Entity<SchemaView>,
    },
}

pub(crate) struct SessionTab {
    pub(crate) title: SharedString,
    pub(crate) content: TabContent,
}

impl SessionTab {
    /// The table this tab shows, if it is a table or structure tab.
    pub(crate) fn object(&self, cx: &gpui_kit::App) -> Option<DatabaseObject> {
        match &self.content {
            TabContent::Table { view } => Some(view.read(cx).object().clone()),
            TabContent::Schema { view } => Some(view.read(cx).object().clone()),
            TabContent::Query { .. } => None,
        }
    }

    /// Which view mode this tab is showing its object in, if it is showing one.
    pub(crate) fn mode(&self) -> Option<ObjectViewMode> {
        match &self.content {
            TabContent::Table { .. } => Some(ObjectViewMode::Data),
            TabContent::Schema { .. } => Some(ObjectViewMode::Schema),
            TabContent::Query { .. } => None,
        }
    }

    /// Whether a query tab's buffer has changed since it was opened or last
    /// saved. A table tab has no buffer, so it is never dirty; a structure
    /// tab is read-only in phase 1, so it never is either.
    pub(crate) fn is_dirty(&self, cx: &gpui_kit::App) -> bool {
        match &self.content {
            TabContent::Query {
                editor, baseline, ..
            } => &editor.read(cx).sql(cx) != baseline,
            TabContent::Table { .. } => false,
            TabContent::Schema { view } => view.read(cx).is_dirty(),
        }
    }
}
