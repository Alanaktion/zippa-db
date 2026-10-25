//! The session's tool tabs: the console, the server views, SQLite
//! maintenance, user management, and the ER diagram. A session holds at most
//! one of each, opened from the sidebar's Management tab, the View menu, or
//! the quick switcher, and brought forward rather than duplicated.
//!
//! Each tool's view is built on a connection, follows the session to a new
//! one, can be re-read, and hands the keyboard to its grid or canvas. That
//! shared shape lives here, so adding a tool is one arm in each match below
//! rather than one in every method of `SessionPanel`.

use std::sync::Arc;

use gpui_kit::assets::IconName;
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, FocusHandle, Focusable as _, Window};

use crate::db::{Connection, Engine};
use crate::ui::console::ConsoleView;
use crate::ui::erd::ErdView;
use crate::ui::process_list::ProcessListView;
use crate::ui::query_digest::QueryDigestView;
use crate::ui::server_variables::ServerVariablesView;
use crate::ui::sqlite_maintenance::SqliteMaintenanceView;
use crate::ui::user_management::UserManagementView;
use crate::workspace_state::PanelState;

/// Which tool a tab is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tool {
    Console,
    Processes,
    Variables,
    Digest,
    Maintenance,
    Users,
    Erd,
}

impl Tool {
    /// The tab's title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Tool::Console => "Console",
            Tool::Processes => "Processes",
            Tool::Variables => "Variables",
            Tool::Digest => "Query Digest",
            Tool::Maintenance => "Maintenance",
            Tool::Users => "Users",
            Tool::Erd => "ER diagram",
        }
    }

    /// Whether `engine` has anything for this tool to show. The server views
    /// need a server, which SQLite is not; maintenance is SQLite's own, the
    /// other engines having a server to look after it; the console and the
    /// diagram work everywhere.
    pub(crate) fn offered(self, engine: Engine) -> bool {
        match self {
            Tool::Console | Tool::Erd => true,
            Tool::Maintenance => engine == Engine::Sqlite,
            Tool::Processes | Tool::Variables | Tool::Digest | Tool::Users => {
                engine != Engine::Sqlite
            }
        }
    }

    /// The icon on the tab.
    pub(crate) fn icon(self) -> IconName {
        match self {
            Tool::Console => IconName::ScrollText,
            Tool::Processes => IconName::Activity,
            Tool::Variables => IconName::SlidersHorizontal,
            Tool::Digest => IconName::Gauge,
            Tool::Maintenance => IconName::Wrench,
            Tool::Users => IconName::Users,
            Tool::Erd => IconName::Network,
        }
    }

    /// What the tab shows, in words, for a screen reader.
    pub(crate) fn kind(self) -> &'static str {
        match self {
            Tool::Console => "console",
            Tool::Processes => "process list",
            Tool::Variables => "server variables",
            Tool::Digest => "query digest",
            Tool::Maintenance => "database maintenance",
            Tool::Users => "user management",
            Tool::Erd => "ER diagram",
        }
    }

    /// How `workspace.json` records the tab.
    pub(crate) fn state(self) -> PanelState {
        match self {
            Tool::Console => PanelState::Console,
            Tool::Processes => PanelState::Processes,
            Tool::Variables => PanelState::Variables,
            Tool::Digest => PanelState::Digest,
            Tool::Maintenance => PanelState::Maintenance,
            Tool::Users => PanelState::Users,
            Tool::Erd => PanelState::Erd,
        }
    }

    /// The tool a recorded tab was, if it was one.
    pub(crate) fn from_state(state: &PanelState) -> Option<Self> {
        match state {
            PanelState::Console => Some(Tool::Console),
            PanelState::Processes => Some(Tool::Processes),
            PanelState::Variables => Some(Tool::Variables),
            PanelState::Digest => Some(Tool::Digest),
            PanelState::Maintenance => Some(Tool::Maintenance),
            PanelState::Users => Some(Tool::Users),
            PanelState::Erd => Some(Tool::Erd),
            PanelState::Query { .. }
            | PanelState::Table { .. }
            | PanelState::Schema { .. }
            | PanelState::Dashboard { .. }
            | PanelState::Unknown => None,
        }
    }
}

/// A tool tab's view.
pub(crate) enum ToolView {
    Console(Entity<ConsoleView>),
    Processes(Entity<ProcessListView>),
    Variables(Entity<ServerVariablesView>),
    Digest(Entity<QueryDigestView>),
    Maintenance(Entity<SqliteMaintenanceView>),
    Users(Entity<UserManagementView>),
    Erd(Entity<ErdView>),
}

/// Run `$body` with `$view` bound to whichever entity a [`ToolView`] holds:
/// every view answers the same calls, but each is its own type.
macro_rules! each_view {
    ($tool:expr, $view:ident => $body:expr) => {
        match $tool {
            ToolView::Console($view) => $body,
            ToolView::Processes($view) => $body,
            ToolView::Variables($view) => $body,
            ToolView::Digest($view) => $body,
            ToolView::Maintenance($view) => $body,
            ToolView::Users($view) => $body,
            ToolView::Erd($view) => $body,
        }
    };
}

impl ToolView {
    /// Build `tool`'s view on `connection`; it starts reading at once.
    pub(crate) fn new(
        tool: Tool,
        connection: Arc<Connection>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        match tool {
            Tool::Console => {
                ToolView::Console(cx.new(|cx| ConsoleView::new(connection, window, cx)))
            }
            Tool::Processes => {
                ToolView::Processes(cx.new(|cx| ProcessListView::new(connection, window, cx)))
            }
            Tool::Variables => {
                ToolView::Variables(cx.new(|cx| ServerVariablesView::new(connection, window, cx)))
            }
            Tool::Digest => {
                ToolView::Digest(cx.new(|cx| QueryDigestView::new(connection, window, cx)))
            }
            Tool::Maintenance => ToolView::Maintenance(
                cx.new(|cx| SqliteMaintenanceView::new(connection, window, cx)),
            ),
            Tool::Users => {
                ToolView::Users(cx.new(|cx| UserManagementView::new(connection, window, cx)))
            }
            Tool::Erd => ToolView::Erd(cx.new(|cx| ErdView::new(connection, window, cx))),
        }
    }

    pub(crate) fn tool(&self) -> Tool {
        match self {
            ToolView::Console(_) => Tool::Console,
            ToolView::Processes(_) => Tool::Processes,
            ToolView::Variables(_) => Tool::Variables,
            ToolView::Digest(_) => Tool::Digest,
            ToolView::Maintenance(_) => Tool::Maintenance,
            ToolView::Users(_) => Tool::Users,
            ToolView::Erd(_) => Tool::Erd,
        }
    }

    /// Point the view at the session's new connection — a database switch or
    /// a reconnect reopens the pool.
    pub(crate) fn set_connection(&self, connection: Arc<Connection>, cx: &mut App) {
        each_view!(self, view => view.update(cx, |view, cx| view.set_connection(connection, cx)))
    }

    /// Read what the tool shows again. Maintenance has nothing to re-read: it
    /// only shows what its buttons ran.
    pub(crate) fn refresh(&self, cx: &mut App) {
        match self {
            ToolView::Maintenance(_) => {}
            ToolView::Console(view) => view.update(cx, |view, cx| view.refresh(cx)),
            ToolView::Processes(view) => view.update(cx, |view, cx| view.refresh(cx)),
            ToolView::Variables(view) => view.update(cx, |view, cx| view.refresh(cx)),
            ToolView::Digest(view) => view.update(cx, |view, cx| view.refresh(cx)),
            ToolView::Users(view) => view.update(cx, |view, cx| view.refresh(cx)),
            ToolView::Erd(view) => view.update(cx, |view, cx| view.refresh(cx)),
        }
    }

    /// Where the keyboard goes when the tab is shown: the view's grid or
    /// canvas, for the same reason a table tab answers with its grid.
    pub(crate) fn focus_handle(&self, cx: &App) -> FocusHandle {
        each_view!(self, view => view.read(cx).focus_handle(cx))
    }

    /// The view, to fill the tab with.
    pub(crate) fn element(&self) -> AnyElement {
        each_view!(self, view => view.clone().into_any_element())
    }
}
