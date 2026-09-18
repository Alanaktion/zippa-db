//! The OS application menu bar (the menu bar at the top of the screen on
//! macOS; a per-window menu bar on Linux and Windows).
//!
//! Reuses the same actions the keymap already binds in `keymap.rs` — a menu
//! item is only another way to reach them, not a second implementation — so
//! each one dims itself exactly where its matching shortcut would do
//! nothing: GPUI checks `is_action_available` against the same context the
//! keybinding is scoped to.

use gpui_kit::{App, KeyBinding, Menu, MenuItem, actions};

use crate::app::{CloseConnection, NewConnection};
use crate::ui::query_editor::{Explain, ExplainAnalyze};
use crate::ui::session::{CloseTab, NewTab, OpenFile, SaveFile, SaveFileAs};
use crate::ui::settings_window::OpenSettings;
use crate::ui::shortcuts_dialog::ShowShortcuts;

// Not bound by the keymap: nothing else in the app reaches for the app-level
// commands the App menu is expected to carry.
actions!(zippa_db, [Quit, Hide, HideOthers, ShowAll]);

pub fn init(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());

    // No context: these apply to the app as a whole, not to whatever has
    // focus, the same way `OpenSettings` is bound in `keymap.rs`.
    cx.bind_keys([
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-h", Hide, None),
    ]);

    cx.set_menus(vec![
        Menu::new("Zippa DB").items(vec![
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Hide Zippa DB", Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action("Quit Zippa DB", Quit),
        ]),
        Menu::new("File").items(vec![
            MenuItem::action("New Connection", NewConnection),
            MenuItem::action("New Query Tab", NewTab),
            MenuItem::separator(),
            MenuItem::action("Open…", OpenFile),
            MenuItem::action("Save", SaveFile),
            MenuItem::action("Save As…", SaveFileAs),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close Connection", CloseConnection),
        ]),
        Menu::new("Query").items(vec![
            MenuItem::action("Explain", Explain),
            MenuItem::action("Explain Analyze", ExplainAnalyze),
        ]),
        Menu::new("Help").items(vec![MenuItem::action("Keyboard Shortcuts", ShowShortcuts)]),
    ]);
}
