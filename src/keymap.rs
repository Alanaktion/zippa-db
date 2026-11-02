//! Key bindings.
//!
//! GPUI resolves `secondary` per platform: Cmd on macOS, Ctrl on Linux and
//! Windows. Every shortcut here uses it so the app matches the OS it runs on.

use gpui_kit::base::input::Paste;
use gpui_kit::{Action, App, KeyBinding, KeyBindingContextPredicate, NoAction};

use crate::app::{
    CloseConnection, NewConnection, NextConnection, OpenSqliteFile, PreviousConnection,
};
use crate::ui::agents::{ApproveWrite, RejectWrite, ToggleAgents};
use crate::ui::create_database_dialog::CloseCreateDatabase;
use crate::ui::data_grid::{
    ClearRowSelection, CopyValue, CopyWithHeaders, ExtendSelectionDown, ExtendSelectionUp,
    SelectAllRows, ToggleRow, ViewCell,
};
use crate::ui::import_dialog::CloseImport;
use crate::ui::query_editor::{
    CopyLine, CutLine, DeleteLine, DuplicateLineDown, DuplicateLineUp, Explain, ExplainAnalyze,
    FormatQuery, GoToError, MoveLineDown, MoveLineUp, RunQuery, RunScript, RunScriptIgnoringErrors,
    SelectLine, SelectNextOccurrence, SelectPreviousOccurrence, SmartHome, ToggleComment,
};
use crate::ui::session::dashboard::CloseWidgetDialog;
use crate::ui::session::{
    CancelQuery, CloseTab, ImportSqlDump, NewTab, NextTab, OpenConnectionInfo, OpenConsole,
    OpenErd, OpenFile, OpenHistory, OpenMaintenance, OpenProcessList, OpenQueryDigest,
    OpenServerVariables, OpenUserManagement, PreviousTab, QuickSwitcher, Reconnect, Refresh,
    SaveFile, SaveFileAs, SaveQuery, SearchSchema, ToggleSidebar, UploadFile,
};
use crate::ui::settings_window::{CloseSettings, OpenSettings};
use crate::ui::shortcuts_dialog::ShowShortcuts;
use crate::ui::table_view::{
    ApplyEdits, CancelEdit, DeleteRows, DiscardEdits, EditCell, FirstPage, InsertRow, LastPage,
    NextPage, PreviousPage, RestoreRows, SetNull, ToggleRowPanel,
};
use crate::ui::user_management::CloseUserDialog;
use crate::ui::value_dialog::{CloseValue, SaveValue};
use crate::ui::variables_dialog::CloseVariablesDialog;
use crate::ui::welcome::{EditorClose, EditorConnect};

/// A binding whose `secondary-` is the modifier of the OS the app runs on.
///
/// `KeyBinding::new` parses with no platform in mind and refuses `secondary-`
/// outright; `KeyBinding::load` resolves it against the running platform.
pub(crate) fn key<A: Action>(cx: &App, keys: &str, action: A, context: Option<&str>) -> KeyBinding {
    let context = context.map(|context| {
        KeyBindingContextPredicate::parse(context)
            .unwrap_or_else(|error| panic!("bad key context {context:?}: {error}"))
            .into()
    });
    KeyBinding::load(keys, Box::new(action), context, false, None, cx)
        .unwrap_or_else(|error| panic!("bad keystroke {keys:?}: {error}"))
}

pub fn bind(cx: &mut App) {
    cx.bind_keys([
        // Quick switcher / fuzzy object palette across tabs, tables, views, databases, and actions.
        key(cx, "secondary-k", QuickSwitcher, Some("Session")),
        key(
            cx,
            "secondary-k",
            QuickSwitcher,
            Some("QueryEditor > Input"),
        ),
        key(cx, "secondary-k", QuickSwitcher, Some("QueryEditor")),
        key(cx, "secondary-k", QuickSwitcher, Some("TableView")),
        key(
            cx,
            "secondary-k",
            QuickSwitcher,
            Some("TableView > DataTable"),
        ),
        key(
            cx,
            "secondary-k",
            QuickSwitcher,
            Some("TableView > DataTable > Input"),
        ),
        key(
            cx,
            "secondary-k",
            QuickSwitcher,
            Some("DataGrid > DataTable"),
        ),
        key(cx, "secondary-k", QuickSwitcher, Some("Workspace")),
        // Schema search: every column, index, routine and trigger, not just
        // the names the quick switcher knows. The shifted key of the quick
        // switcher, and bound everywhere it is.
        key(cx, "secondary-shift-o", SearchSchema, Some("Session")),
        key(
            cx,
            "secondary-shift-o",
            SearchSchema,
            Some("QueryEditor > Input"),
        ),
        key(cx, "secondary-shift-o", SearchSchema, Some("QueryEditor")),
        key(cx, "secondary-shift-o", SearchSchema, Some("TableView")),
        key(
            cx,
            "secondary-shift-o",
            SearchSchema,
            Some("TableView > DataTable"),
        ),
        key(
            cx,
            "secondary-shift-o",
            SearchSchema,
            Some("TableView > DataTable > Input"),
        ),
        key(
            cx,
            "secondary-shift-o",
            SearchSchema,
            Some("DataGrid > DataTable"),
        ),
        key(cx, "secondary-shift-o", SearchSchema, Some("Workspace")),
        // The editor's own `Input` context already binds `secondary-enter`
        // (it would insert a newline), so this binds the more specific
        // "Input inside a QueryEditor" to win at that node.
        key(cx, "secondary-enter", RunQuery, Some("QueryEditor > Input")),
        // Running the whole buffer is the rarer of the two, so it takes the
        // extra modifier.
        key(
            cx,
            "secondary-shift-enter",
            RunScript,
            Some("QueryEditor > Input"),
        ),
        key(cx, "secondary-shift-enter", RunScript, Some("QueryEditor")),
        // The whole buffer again, without a transaction and past every
        // failure — re-running a migration that was partly applied before.
        key(
            cx,
            "secondary-alt-shift-enter",
            RunScriptIgnoringErrors,
            Some("QueryEditor > Input"),
        ),
        key(
            cx,
            "secondary-alt-shift-enter",
            RunScriptIgnoringErrors,
            Some("QueryEditor"),
        ),
        // Reading the plan, with and without running the statement. The
        // editor's own context leaves both free.
        key(cx, "secondary-e", Explain, Some("QueryEditor > Input")),
        key(cx, "secondary-e", Explain, Some("QueryEditor")),
        key(
            cx,
            "secondary-shift-e",
            ExplainAnalyze,
            Some("QueryEditor > Input"),
        ),
        key(cx, "secondary-shift-e", ExplainAnalyze, Some("QueryEditor")),
        // Toggle commenting on the caret's line or the selected lines.
        key(
            cx,
            "secondary-/",
            ToggleComment,
            Some("QueryEditor > Input"),
        ),
        key(cx, "secondary-/", ToggleComment, Some("QueryEditor")),
        // Pretty-print the selection, or the whole buffer when nothing is
        // selected.
        key(
            cx,
            "secondary-shift-f",
            FormatQuery,
            Some("QueryEditor > Input"),
        ),
        key(cx, "secondary-shift-f", FormatQuery, Some("QueryEditor")),
        // The line operations, with VS Code's shape: with a selection they act
        // on it, otherwise on the caret's line. Copy and cut take the line
        // plus its newline, so pasting yields a whole line.
        key(cx, "secondary-c", CopyLine, Some("QueryEditor > Input")),
        key(cx, "secondary-c", CopyLine, Some("QueryEditor")),
        key(cx, "secondary-x", CutLine, Some("QueryEditor > Input")),
        key(cx, "secondary-x", CutLine, Some("QueryEditor")),
        key(
            cx,
            "secondary-shift-k",
            DeleteLine,
            Some("QueryEditor > Input"),
        ),
        key(cx, "secondary-shift-k", DeleteLine, Some("QueryEditor")),
        key(cx, "alt-up", MoveLineUp, Some("QueryEditor > Input")),
        key(cx, "alt-up", MoveLineUp, Some("QueryEditor")),
        key(cx, "alt-down", MoveLineDown, Some("QueryEditor > Input")),
        key(cx, "alt-down", MoveLineDown, Some("QueryEditor")),
        key(
            cx,
            "alt-shift-up",
            DuplicateLineUp,
            Some("QueryEditor > Input"),
        ),
        key(cx, "alt-shift-up", DuplicateLineUp, Some("QueryEditor")),
        key(
            cx,
            "alt-shift-down",
            DuplicateLineDown,
            Some("QueryEditor > Input"),
        ),
        key(cx, "alt-shift-down", DuplicateLineDown, Some("QueryEditor")),
        // Occurrence selection. The editor's narrower context wins over the
        // session's disconnect and query digest on these keys, the way the
        // quick switcher already does on its own.
        key(
            cx,
            "secondary-d",
            SelectNextOccurrence,
            Some("QueryEditor > Input"),
        ),
        key(cx, "secondary-d", SelectNextOccurrence, Some("QueryEditor")),
        key(
            cx,
            "secondary-shift-d",
            SelectPreviousOccurrence,
            Some("QueryEditor > Input"),
        ),
        key(
            cx,
            "secondary-shift-d",
            SelectPreviousOccurrence,
            Some("QueryEditor"),
        ),
        // The last run's failure. F8 is the "next problem" key in the editors
        // this borrows from.
        key(cx, "f8", GoToError, Some("QueryEditor > Input")),
        key(cx, "f8", GoToError, Some("QueryEditor")),
        // The caret's whole line; pressing again extends through the next one.
        key(cx, "secondary-l", SelectLine, Some("QueryEditor > Input")),
        key(cx, "secondary-l", SelectLine, Some("QueryEditor")),
        // Home goes to the first non-blank character first, then the line's
        // start. ⌘◀ does the same on macOS; elsewhere `cmd` is the Super key,
        // where the binding is inert, leaving Ctrl+◀ to word movement.
        key(cx, "home", SmartHome, Some("QueryEditor > Input")),
        key(cx, "cmd-left", SmartHome, Some("QueryEditor > Input")),
        // Stop a query that is taking too long.
        key(cx, "secondary-.", CancelQuery, Some("Session")),
        // Import a SQL dump into the connection. The table view binds the same
        // key for "add a row", which wins while its grid has focus.
        key(cx, "secondary-shift-i", ImportSqlDump, Some("Session")),
        // Giving up on the import dialog: its own `keyboard` handling is off so
        // that `enter` cannot dismiss it, which leaves `escape` to this.
        key(cx, "escape", CloseImport, Some("ImportDialog")),
        // The same for the other dialogs that turn their own keys off: each
        // binds `escape` on its body, which the text box inside it passes
        // the key on to.
        key(
            cx,
            "escape",
            CloseCreateDatabase,
            Some("CreateDatabaseDialog"),
        ),
        key(cx, "escape", CloseUserDialog, Some("UserDialog")),
        key(cx, "escape", CloseVariablesDialog, Some("VariablesDialog")),
        key(cx, "escape", CloseWidgetDialog, Some("WidgetDialog")),
        // The caret is usually in the editor, whose own context claims keys
        // before the session sees them.
        key(cx, "secondary-.", CancelQuery, Some("QueryEditor > Input")),
        key(cx, "secondary-enter", RunQuery, Some("QueryEditor")),
        key(cx, "secondary-t", NewTab, Some("Session")),
        key(cx, "secondary-w", CloseTab, Some("Session")),
        // `Disconnect` has no key of its own: `secondary-d` is the editor's
        // Select Next Occurrence, and the same chord ending the whole session
        // whenever the focus was on the grid or the sidebar made it too easy
        // to hit by accident. The menu and the connection picker reach it.
        // Open a fresh pool after the server dropped the connection or
        // stopped answering, keeping every tab where it is.
        key(cx, "secondary-shift-r", Reconnect, Some("Session")),
        // Send the local copy of a SQLite file opened from an SSH host back
        // to it. Not the save key: that saves the query tab's buffer.
        key(cx, "secondary-alt-u", UploadFile, Some("Session")),
        // Reloads the schema, and the active table's rows; never re-runs a
        // query tab's buffer, so it is safe even if that buffer is a write.
        key(cx, "secondary-r", Refresh, Some("Session")),
        // The console of every statement sent, opened or brought forward.
        // The backtick is the terminal/console toggle in several other
        // editors, which is the habit this borrows.
        key(cx, "secondary-`", OpenConsole, Some("Session")),
        // The query history: what was run from query tabs, kept per
        // connection. H for history, as in a browser.
        key(cx, "secondary-shift-h", OpenHistory, Some("Session")),
        // The sidebar, hidden so a wide result has the window. Cmd+B is the
        // sidebar key in most editors; neither the editor nor the grid binds
        // it, so the session hears it from anywhere inside.
        key(cx, "secondary-b", ToggleSidebar, Some("Session")),
        // The Agents panel beside the session: shown or hidden, with the
        // thread kept either way.
        key(cx, "secondary-alt-a", ToggleAgents, Some("Session")),
        // A write the assistant asked for waits on these. Approving takes the
        // modifier so a stray Enter cannot run a statement.
        key(cx, "secondary-enter", ApproveWrite, Some("AgentApproval")),
        key(cx, "escape", RejectWrite, Some("AgentApproval")),
        // The connection's basic facts: engine, version, database, size.
        // Shares its key with viewing a cell's value, the way the server
        // variables share theirs: the more specific context — a grid with
        // the focus — wins there, and this is what answers it everywhere else.
        key(cx, "secondary-shift-c", OpenConnectionInfo, Some("Session")),
        // The process list: who is connected to the server and what they're
        // running, with a way to end one.
        key(cx, "secondary-shift-p", OpenProcessList, Some("Session")),
        // The server's own configuration. Shares its key with viewing a
        // cell's value, the way Import shares its key with adding a row:
        // the more specific context — a grid with the focus — wins there,
        // and this is what answers it everywhere else.
        key(
            cx,
            "secondary-shift-v",
            OpenServerVariables,
            Some("Session"),
        ),
        // The slow/frequent query digest.
        key(cx, "secondary-shift-d", OpenQueryDigest, Some("Session")),
        // Server users and roles: create, password, drop.
        key(cx, "secondary-shift-u", OpenUserManagement, Some("Session")),
        // The ER diagram of the database's tables and foreign keys.
        key(cx, "secondary-shift-g", OpenErd, Some("Session")),
        // SQLite's housekeeping: integrity checks, optimize, vacuum.
        key(cx, "secondary-shift-m", OpenMaintenance, Some("Session")),
        // Ctrl+Tab is the tab-switching key on every OS, browsers and macOS
        // apps included; Cmd+Shift+] is taken by the connections above.
        // Neither is an editor key, so the session sees them from anywhere.
        key(cx, "ctrl-tab", NextTab, Some("Session")),
        key(cx, "ctrl-shift-tab", PreviousTab, Some("Session")),
        key(cx, "ctrl-pagedown", NextTab, Some("Session")),
        key(cx, "ctrl-pageup", PreviousTab, Some("Session")),
        // Connections are the outer tabs, so they take the shifted keys; the
        // workspace wraps every screen, so these work from all of them.
        key(cx, "secondary-n", NewConnection, Some("Workspace")),
        // On the welcome screen the same key opens the editor dialog instead
        // of another tab: the more specific context wins.
        key(cx, "secondary-n", NewConnection, Some("Welcome")),
        // The launcher has no query tab to open a SQL file into, so its open
        // key opens a SQLite database instead.
        key(cx, "secondary-o", OpenSqliteFile, Some("Welcome")),
        key(cx, "secondary-shift-w", CloseConnection, Some("Workspace")),
        key(cx, "secondary-shift-]", NextConnection, Some("Workspace")),
        key(
            cx,
            "secondary-shift-[",
            PreviousConnection,
            Some("Workspace"),
        ),
        // The editor's own `Input` context leaves these free, so binding them
        // at the session — which owns the tabs and their files — is enough.
        key(cx, "secondary-o", OpenFile, Some("Session")),
        key(cx, "secondary-s", SaveFile, Some("Session")),
        key(cx, "secondary-shift-s", SaveFileAs, Some("Session")),
        // Keep the buffer in the sidebar's Saved tab, updating the saved
        // query the tab belongs to in place.
        key(cx, "secondary-alt-s", SaveQuery, Some("Session")),
        // A table tab has staged edits rather than a file, so the save and
        // undo keys apply and discard them. Both are bound on the table view,
        // which sits below the session, so they win while it has focus.
        key(cx, "secondary-s", ApplyEdits, Some("TableView")),
        // A cell editor is an input of its own, so the save key is bound on
        // that node too: applying mid-edit folds in what is being typed.
        key(
            cx,
            "secondary-s",
            ApplyEdits,
            Some("TableView > DataTable > Input"),
        ),
        // Not `secondary-z`: everywhere else that is undo, and throwing away
        // every staged edit is the opposite of undoing the last one.
        key(cx, "secondary-alt-z", DiscardEdits, Some("TableView")),
        key(cx, "secondary-\\", ToggleRowPanel, Some("TableView")),
        // Paging, the browser's back/forward keys; with Alt, all the way.
        // A text box binds the bare keys to outdent/indent, and is the more
        // specific context, so typing in the filter or search boxes keeps
        // them. Page Up/Down stay the grid's own, moving within the page.
        key(cx, "secondary-[", PreviousPage, Some("TableView")),
        key(cx, "secondary-]", NextPage, Some("TableView")),
        key(cx, "secondary-alt-[", FirstPage, Some("TableView")),
        key(cx, "secondary-alt-]", LastPage, Some("TableView")),
        // The table binds the arrows, tab, and escape for its own selection
        // but leaves `enter` free, so that opens the editor. Both are scoped
        // under the table view, leaving query results read-only.
        key(cx, "enter", EditCell, Some("TableView > DataTable")),
        // The text boxes' own paste, bound on a table's rows too: there it
        // stages the clipboard into the cells from the selected one on. A
        // query result is read-only and leaves it alone.
        key(cx, "secondary-v", Paste, Some("TableView > DataTable")),
        key(
            cx,
            "secondary-shift-n",
            SetNull,
            Some("TableView > DataTable"),
        ),
        // Bound on the grid, like the other row commands: that is the node
        // that holds the focus while the user is looking at rows.
        key(
            cx,
            "secondary-shift-i",
            InsertRow,
            Some("TableView > DataTable"),
        ),
        // Deleting marks the rows rather than writing them away, so the key
        // that takes the mark off again sits beside it.
        key(
            cx,
            "secondary-backspace",
            DeleteRows,
            Some("TableView > DataTable"),
        ),
        key(
            cx,
            "secondary-shift-backspace",
            RestoreRows,
            Some("TableView > DataTable"),
        ),
        // Looking at a whole value works on any grid, so these are bound on
        // the grid rather than on the table view. So does copying one, picking
        // rows out, and everything else about reading a result — a query tab's
        // grid answers them exactly as a table tab's does.
        key(
            cx,
            "secondary-shift-v",
            ViewCell,
            Some("DataGrid > DataTable"),
        ),
        key(cx, "secondary-c", CopyValue, Some("DataGrid > DataTable")),
        // Copying a whole line per row, with a header, is the spreadsheet-shaped
        // copy; the plain key above remains cell-or-rows without a header.
        key(
            cx,
            "secondary-shift-c",
            CopyWithHeaders,
            Some("DataGrid > DataTable"),
        ),
        // The table binds the plain arrows for the selection itself; shifted,
        // they take the rows they pass over with them.
        key(
            cx,
            "shift-up",
            ExtendSelectionUp,
            Some("DataGrid > DataTable"),
        ),
        key(
            cx,
            "shift-down",
            ExtendSelectionDown,
            Some("DataGrid > DataTable"),
        ),
        // Space is the box beside the focused row, which is not a tab stop:
        // with one box per row, tabbing through a page of them to reach the
        // grid is worse than a key that picks the row the user is already on.
        key(cx, "space", ToggleRow, Some("DataGrid > DataTable")),
        // A cell editor's own context is deeper than the table's, so without
        // this a space typed into it would pick the row instead of typing the
        // character.
        key(cx, "space", NoAction, Some("DataGrid > DataTable > Input")),
        key(
            cx,
            "secondary-a",
            SelectAllRows,
            Some("DataGrid > DataTable"),
        ),
        key(
            cx,
            "secondary-shift-a",
            ClearRowSelection,
            Some("DataGrid > DataTable"),
        ),
        // Escape while typing in a cell closes the editor; without this the
        // input's own escape, or the table's clear-selection, would take it.
        key(
            cx,
            "escape",
            CancelEdit,
            Some("TableView > DataTable > Input"),
        ),
        // No context: the settings belong to the app, not to a screen, and the
        // handler for it is registered on the app itself.
        key(cx, "secondary-,", OpenSettings, None),
        // Literally Ctrl on every OS, including macOS, where Cmd+/ is left to
        // whatever the focused control does with it.
        key(cx, "ctrl-/", ShowShortcuts, None),
        // The settings window is not a `Workspace`, so it gets its own close
        // keys; Alt+F4 is the Windows/Linux habit and harmless elsewhere.
        key(cx, "secondary-w", CloseSettings, Some("SettingsWindow")),
        key(cx, "alt-f4", CloseSettings, Some("SettingsWindow")),
        key(cx, "escape", CloseSettings, Some("SettingsWindow")),
        // The value dialog covers the window while it is open. Enter belongs
        // to its text box, which is multi-line, so saving takes the modifier.
        key(cx, "escape", CloseValue, Some("ValueDialog")),
        key(cx, "secondary-enter", SaveValue, Some("ValueDialog")),
        // The caret is in the text box, whose own context binds this key, so
        // the dialog's binding has to name that node to win there.
        key(
            cx,
            "secondary-enter",
            SaveValue,
            Some("ValueDialog > Input"),
        ),
        // The connection editor dialog: Escape gives up, Cmd+Enter connects.
        // The fields are single-line inputs, but their own context is deeper
        // than the editor's, so both paths are bound. Enter in a field
        // connects too, the way it submits any form; it is not bound on the
        // editor itself, where it belongs to whichever button has the focus.
        key(cx, "escape", EditorClose, Some("ConnectionEditor")),
        key(cx, "escape", EditorClose, Some("ConnectionEditor > Input")),
        key(
            cx,
            "secondary-enter",
            EditorConnect,
            Some("ConnectionEditor"),
        ),
        key(
            cx,
            "secondary-enter",
            EditorConnect,
            Some("ConnectionEditor > Input"),
        ),
        key(cx, "enter", EditorConnect, Some("ConnectionEditor > Input")),
    ]);
}
