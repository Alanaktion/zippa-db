//! Key bindings.
//!
//! GPUI resolves `secondary` per platform: Cmd on macOS, Ctrl on Linux and
//! Windows. Every shortcut here uses it so the app matches the OS it runs on.

use gpui_kit::{App, KeyBinding};

use crate::app::{CloseConnection, NewConnection, NextConnection, PreviousConnection};
use crate::ui::query_editor::RunQuery;
use crate::ui::session::{CloseTab, NewTab, OpenFile, SaveFile, SaveFileAs};
use crate::ui::settings_window::OpenSettings;

pub fn bind(cx: &mut App) {
    cx.bind_keys([
        // The editor's own `Input` context already binds `secondary-enter`
        // (it would insert a newline), so this binds the more specific
        // "Input inside a QueryEditor" to win at that node.
        KeyBinding::new("secondary-enter", RunQuery, Some("QueryEditor > Input")),
        KeyBinding::new("secondary-enter", RunQuery, Some("QueryEditor")),
        KeyBinding::new("secondary-t", NewTab, Some("Session")),
        KeyBinding::new("secondary-w", CloseTab, Some("Session")),
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
        // No context: the settings belong to the app, not to a screen, and the
        // handler for it is registered on the app itself.
        KeyBinding::new("secondary-,", OpenSettings, None),
    ]);
}
