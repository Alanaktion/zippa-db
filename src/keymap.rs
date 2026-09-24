//! Key bindings.
//!
//! GPUI resolves `secondary` per platform: Cmd on macOS, Ctrl on Linux and
//! Windows. Every shortcut here uses it so the app matches the OS it runs on.

use gpui_kit::{App, KeyBinding, NoAction};

use crate::app::{CloseConnection, NewConnection, NextConnection, PreviousConnection};
use crate::ui::data_grid::{
    ClearRowSelection, CopyValue, CopyWithHeaders, ExtendSelectionDown, ExtendSelectionUp,
    SelectAllRows, ToggleRow, ViewCell,
};
use crate::ui::import_dialog::CloseImport;
use crate::ui::query_editor::{Explain, ExplainAnalyze, RunQuery, RunScript};
use crate::ui::session::{
    CancelQuery, CloseTab, ImportSqlDump, NewTab, NextTab, OpenConsole, OpenFile, OpenProcessList,
    OpenServerVariables, PreviousTab, QuickSwitcher, Refresh, SaveFile, SaveFileAs, SearchSchema,
};
use crate::ui::settings_window::{CloseSettings, OpenSettings};
use crate::ui::shortcuts_dialog::ShowShortcuts;
use crate::ui::table_view::{
    ApplyEdits, CancelEdit, DeleteRows, DiscardEdits, EditCell, InsertRow, RestoreRows, SetNull,
    ToggleRowPanel,
};
use crate::ui::value_dialog::{CloseValue, SaveValue};
use crate::ui::welcome::{EditorClose, EditorConnect};

pub fn bind(cx: &mut App) {
    cx.bind_keys([
        // Quick switcher / fuzzy object palette across tabs, tables, views, databases, and actions.
        KeyBinding::new("secondary-k", QuickSwitcher, Some("Session")),
        KeyBinding::new("secondary-k", QuickSwitcher, Some("QueryEditor > Input")),
        KeyBinding::new("secondary-k", QuickSwitcher, Some("QueryEditor")),
        KeyBinding::new("secondary-k", QuickSwitcher, Some("TableView")),
        KeyBinding::new("secondary-k", QuickSwitcher, Some("TableView > DataTable")),
        KeyBinding::new(
            "secondary-k",
            QuickSwitcher,
            Some("TableView > DataTable > Input"),
        ),
        KeyBinding::new("secondary-k", QuickSwitcher, Some("DataGrid > DataTable")),
        KeyBinding::new("secondary-k", QuickSwitcher, Some("Workspace")),
        // Schema search: every column, index, routine and trigger, not just
        // the names the quick switcher knows. The shifted key of the quick
        // switcher, and bound everywhere it is.
        KeyBinding::new("secondary-shift-o", SearchSchema, Some("Session")),
        KeyBinding::new(
            "secondary-shift-o",
            SearchSchema,
            Some("QueryEditor > Input"),
        ),
        KeyBinding::new("secondary-shift-o", SearchSchema, Some("QueryEditor")),
        KeyBinding::new("secondary-shift-o", SearchSchema, Some("TableView")),
        KeyBinding::new(
            "secondary-shift-o",
            SearchSchema,
            Some("TableView > DataTable"),
        ),
        KeyBinding::new(
            "secondary-shift-o",
            SearchSchema,
            Some("TableView > DataTable > Input"),
        ),
        KeyBinding::new(
            "secondary-shift-o",
            SearchSchema,
            Some("DataGrid > DataTable"),
        ),
        KeyBinding::new("secondary-shift-o", SearchSchema, Some("Workspace")),
        // The editor's own `Input` context already binds `secondary-enter`
        // (it would insert a newline), so this binds the more specific
        // "Input inside a QueryEditor" to win at that node.
        KeyBinding::new("secondary-enter", RunQuery, Some("QueryEditor > Input")),
        // Running the whole buffer is the rarer of the two, so it takes the
        // extra modifier.
        KeyBinding::new(
            "secondary-shift-enter",
            RunScript,
            Some("QueryEditor > Input"),
        ),
        KeyBinding::new("secondary-shift-enter", RunScript, Some("QueryEditor")),
        // Reading the plan, with and without running the statement. The
        // editor's own context leaves both free.
        KeyBinding::new("secondary-e", Explain, Some("QueryEditor > Input")),
        KeyBinding::new("secondary-e", Explain, Some("QueryEditor")),
        KeyBinding::new(
            "secondary-shift-e",
            ExplainAnalyze,
            Some("QueryEditor > Input"),
        ),
        KeyBinding::new("secondary-shift-e", ExplainAnalyze, Some("QueryEditor")),
        // Give up on a query that is taking too long.
        KeyBinding::new("secondary-.", CancelQuery, Some("Session")),
        // Import a SQL dump into the connection. The table view binds the same
        // key for "add a row", which wins while its grid has focus.
        KeyBinding::new("secondary-shift-i", ImportSqlDump, Some("Session")),
        // Giving up on the import dialog: its own `keyboard` handling is off so
        // that `enter` cannot dismiss it, which leaves `escape` to this.
        KeyBinding::new("escape", CloseImport, Some("ImportDialog")),
        // The caret is usually in the editor, whose own context claims keys
        // before the session sees them.
        KeyBinding::new("secondary-.", CancelQuery, Some("QueryEditor > Input")),
        KeyBinding::new("secondary-enter", RunQuery, Some("QueryEditor")),
        KeyBinding::new("secondary-t", NewTab, Some("Session")),
        KeyBinding::new("secondary-w", CloseTab, Some("Session")),
        // Reloads the schema, and the active table's rows; never re-runs a
        // query tab's buffer, so it is safe even if that buffer is a write.
        KeyBinding::new("secondary-r", Refresh, Some("Session")),
        // The console of every statement sent, opened or brought forward.
        // The backtick is the terminal/console toggle in several other
        // editors, which is the habit this borrows.
        KeyBinding::new("secondary-`", OpenConsole, Some("Session")),
        // The process list: who is connected to the server and what they're
        // running, with a way to end one.
        KeyBinding::new("secondary-shift-p", OpenProcessList, Some("Session")),
        // The server's own configuration. Shares its key with viewing a
        // cell's value, the way Import shares its key with adding a row:
        // the more specific context — a grid with the focus — wins there,
        // and this is what answers it everywhere else.
        KeyBinding::new("secondary-shift-v", OpenServerVariables, Some("Session")),
        // Ctrl+Tab is the tab-switching key on every OS, browsers and macOS
        // apps included; Cmd+Shift+] is taken by the connections above.
        // Neither is an editor key, so the session sees them from anywhere.
        KeyBinding::new("ctrl-tab", NextTab, Some("Session")),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some("Session")),
        KeyBinding::new("ctrl-pagedown", NextTab, Some("Session")),
        KeyBinding::new("ctrl-pageup", PreviousTab, Some("Session")),
        // Connections are the outer tabs, so they take the shifted keys; the
        // workspace wraps every screen, so these work from all of them.
        KeyBinding::new("secondary-n", NewConnection, Some("Workspace")),
        // On the welcome screen the same key opens the editor dialog instead
        // of another tab: the more specific context wins.
        KeyBinding::new("secondary-n", NewConnection, Some("Welcome")),
        KeyBinding::new("secondary-shift-w", CloseConnection, Some("Workspace")),
        KeyBinding::new("secondary-shift-]", NextConnection, Some("Workspace")),
        KeyBinding::new("secondary-shift-[", PreviousConnection, Some("Workspace")),
        // The editor's own `Input` context leaves these free, so binding them
        // at the session — which owns the tabs and their files — is enough.
        KeyBinding::new("secondary-o", OpenFile, Some("Session")),
        KeyBinding::new("secondary-s", SaveFile, Some("Session")),
        KeyBinding::new("secondary-shift-s", SaveFileAs, Some("Session")),
        // A table tab has staged edits rather than a file, so the save and
        // undo keys apply and discard them. Both are bound on the table view,
        // which sits below the session, so they win while it has focus.
        KeyBinding::new("secondary-s", ApplyEdits, Some("TableView")),
        // A cell editor is an input of its own, so the save key is bound on
        // that node too: applying mid-edit folds in what is being typed.
        KeyBinding::new(
            "secondary-s",
            ApplyEdits,
            Some("TableView > DataTable > Input"),
        ),
        KeyBinding::new("secondary-z", DiscardEdits, Some("TableView")),
        KeyBinding::new("secondary-\\", ToggleRowPanel, Some("TableView")),
        // The table binds the arrows, tab, and escape for its own selection
        // but leaves `enter` free, so that opens the editor. Both are scoped
        // under the table view, leaving query results read-only.
        KeyBinding::new("enter", EditCell, Some("TableView > DataTable")),
        KeyBinding::new("secondary-shift-n", SetNull, Some("TableView > DataTable")),
        // Bound on the grid, like the other row commands: that is the node
        // that holds the focus while the user is looking at rows.
        KeyBinding::new(
            "secondary-shift-i",
            InsertRow,
            Some("TableView > DataTable"),
        ),
        // Deleting marks the rows rather than writing them away, so the key
        // that takes the mark off again sits beside it.
        KeyBinding::new(
            "secondary-backspace",
            DeleteRows,
            Some("TableView > DataTable"),
        ),
        KeyBinding::new(
            "secondary-shift-backspace",
            RestoreRows,
            Some("TableView > DataTable"),
        ),
        // Looking at a whole value works on any grid, so these are bound on
        // the grid rather than on the table view. So does copying one, picking
        // rows out, and everything else about reading a result — a query tab's
        // grid answers them exactly as a table tab's does.
        KeyBinding::new("secondary-shift-v", ViewCell, Some("DataGrid > DataTable")),
        KeyBinding::new("secondary-c", CopyValue, Some("DataGrid > DataTable")),
        // Copying a whole line per row, with a header, is the spreadsheet-shaped
        // copy; the plain key above remains cell-or-rows without a header.
        KeyBinding::new(
            "secondary-shift-c",
            CopyWithHeaders,
            Some("DataGrid > DataTable"),
        ),
        // The table binds the plain arrows for the selection itself; shifted,
        // they take the rows they pass over with them.
        KeyBinding::new("shift-up", ExtendSelectionUp, Some("DataGrid > DataTable")),
        KeyBinding::new(
            "shift-down",
            ExtendSelectionDown,
            Some("DataGrid > DataTable"),
        ),
        // Space is the box beside the focused row, which is not a tab stop:
        // with one box per row, tabbing through a page of them to reach the
        // grid is worse than a key that picks the row the user is already on.
        KeyBinding::new("space", ToggleRow, Some("DataGrid > DataTable")),
        // A cell editor's own context is deeper than the table's, so without
        // this a space typed into it would pick the row instead of typing the
        // character.
        KeyBinding::new("space", NoAction, Some("DataGrid > DataTable > Input")),
        KeyBinding::new("secondary-a", SelectAllRows, Some("DataGrid > DataTable")),
        KeyBinding::new(
            "secondary-shift-a",
            ClearRowSelection,
            Some("DataGrid > DataTable"),
        ),
        // Escape while typing in a cell closes the editor; without this the
        // input's own escape, or the table's clear-selection, would take it.
        KeyBinding::new("escape", CancelEdit, Some("TableView > DataTable > Input")),
        // No context: the settings belong to the app, not to a screen, and the
        // handler for it is registered on the app itself.
        KeyBinding::new("secondary-,", OpenSettings, None),
        // Literally Ctrl on every OS, including macOS, where Cmd+/ is left to
        // whatever the focused control does with it.
        KeyBinding::new("ctrl-/", ShowShortcuts, None),
        // The settings window is not a `Workspace`, so it gets its own close
        // keys; Alt+F4 is the Windows/Linux habit and harmless elsewhere.
        KeyBinding::new("secondary-w", CloseSettings, Some("SettingsWindow")),
        KeyBinding::new("alt-f4", CloseSettings, Some("SettingsWindow")),
        KeyBinding::new("escape", CloseSettings, Some("SettingsWindow")),
        // The value dialog covers the window while it is open. Enter belongs
        // to its text box, which is multi-line, so saving takes the modifier.
        KeyBinding::new("escape", CloseValue, Some("ValueDialog")),
        KeyBinding::new("secondary-enter", SaveValue, Some("ValueDialog")),
        // The caret is in the text box, whose own context binds this key, so
        // the dialog's binding has to name that node to win there.
        KeyBinding::new("secondary-enter", SaveValue, Some("ValueDialog > Input")),
        // The connection editor dialog: Escape gives up, Cmd+Enter connects.
        // The fields are single-line inputs, but their own context is deeper
        // than the editor's, so both paths are bound.
        KeyBinding::new("escape", EditorClose, Some("ConnectionEditor")),
        KeyBinding::new("escape", EditorClose, Some("ConnectionEditor > Input")),
        KeyBinding::new("secondary-enter", EditorConnect, Some("ConnectionEditor")),
        KeyBinding::new(
            "secondary-enter",
            EditorConnect,
            Some("ConnectionEditor > Input"),
        ),
    ]);
}
