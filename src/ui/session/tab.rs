//! What a session tab holds, and how the last run of it went.

use std::path::PathBuf;

use gpui_kit::Entity;

use crate::db::query::QueryResult;
use crate::ui::console::ConsoleView;
use crate::ui::data_grid::DataGrid;
use crate::ui::plan_view::PlanView;
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
        }
    }
}

/// What a tab holds: a query editor with its result, or a table opened from
/// the sidebar.
///
/// The query variant is the wide one — it carries the buffer, its results, and
/// the plan — and the others are a single entity handle. Boxing the query to
/// even them out would put an indirection on every accessor for no gain, since
/// a session holds a handful of these at once.
#[allow(clippy::large_enum_variant)]
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
        /// The last plan this tab read, if any, so a plan can be looked at
        /// again after the grid has been.
        plan: Option<Entity<PlanView>>,
        /// Whether the pane is showing the plan rather than the result grid.
        show_plan: bool,
    },
    Table {
        view: Entity<TableView>,
    },
    Schema {
        view: Entity<SchemaView>,
    },
    Console {
        view: Entity<ConsoleView>,
    },
}
