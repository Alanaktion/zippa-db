//! Key bindings.
//!
//! GPUI resolves `secondary` per platform: Cmd on macOS, Ctrl on Linux and
//! Windows. Every shortcut here uses it so the app matches the OS it runs on.

use gpui_kit::{App, KeyBinding};

use crate::ui::query_editor::RunQuery;
use crate::ui::session::{CloseTab, NewTab};

pub fn bind(cx: &mut App) {
    cx.bind_keys([
        // The editor's own `Input` context already binds `secondary-enter`
        // (it would insert a newline), so this binds the more specific
        // "Input inside a QueryEditor" to win at that node.
        KeyBinding::new("secondary-enter", RunQuery, Some("QueryEditor > Input")),
        KeyBinding::new("secondary-enter", RunQuery, Some("QueryEditor")),
        KeyBinding::new("secondary-t", NewTab, Some("Session")),
        KeyBinding::new("secondary-w", CloseTab, Some("Session")),
    ]);
}
