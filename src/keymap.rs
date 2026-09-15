//! Key bindings.
//!
//! GPUI resolves `secondary` per platform: Cmd on macOS, Ctrl on Linux and
//! Windows. Every shortcut here uses it so the app matches the OS it runs on.

use gpui_kit::{App, KeyBinding};

use crate::app::{CloseConnection, NewConnection, NextConnection, PreviousConnection};
use crate::ui::data_grid::ViewCell;
use crate::ui::query_editor::{RunQuery, RunScript};
use crate::ui::session::{
    CancelQuery, CloseTab, NewTab, OpenFile, QuickSwitcher, Refresh, SaveFile, SaveFileAs,
};
use crate::ui::settings_window::OpenSettings;
use crate::ui::table_view::{
    ApplyEdits, CancelEdit, DeleteRows, DiscardEdits, EditCell, InsertRow, RestoreRows, SetNull,
};
use crate::ui::value_dialog::{CloseValue, SaveValue};

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
        // Give up on a query that is taking too long.
        KeyBinding::new("secondary-.", CancelQuery, Some("Session")),
        // The caret is usually in the editor, whose own context claims keys
        // before the session sees them.
        KeyBinding::new("secondary-.", CancelQuery, Some("QueryEditor > Input")),
        KeyBinding::new("secondary-enter", RunQuery, Some("QueryEditor")),
        KeyBinding::new("secondary-t", NewTab, Some("Session")),
        KeyBinding::new("secondary-w", CloseTab, Some("Session")),
        // Reloads the schema, and the active table's rows; never re-runs a
        // query tab's buffer, so it is safe even if that buffer is a write.
        KeyBinding::new("secondary-r", Refresh, Some("Session")),
        // Connections are the outer tabs, so they take the shifted keys; the
        // workspace wraps every screen, so these work from all of them.
        KeyBinding::new("secondary-n", NewConnection, Some("Workspace")),
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
        // the grid rather than on the table view.
        KeyBinding::new("secondary-shift-v", ViewCell, Some("DataGrid > DataTable")),
        // Escape while typing in a cell closes the editor; without this the
        // input's own escape, or the table's clear-selection, would take it.
        KeyBinding::new("escape", CancelEdit, Some("TableView > DataTable > Input")),
        // No context: the settings belong to the app, not to a screen, and the
        // handler for it is registered on the app itself.
        KeyBinding::new("secondary-,", OpenSettings, None),
        // The value dialog covers the window while it is open. Enter belongs
        // to its text box, which is multi-line, so saving takes the modifier.
        KeyBinding::new("escape", CloseValue, Some("ValueDialog")),
        KeyBinding::new("secondary-enter", SaveValue, Some("ValueDialog")),
        // The caret is in the text box, whose own context binds this key, so
        // the dialog's binding has to name that node to win there.
        KeyBinding::new("secondary-enter", SaveValue, Some("ValueDialog > Input")),
    ]);
}
