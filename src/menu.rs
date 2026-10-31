//! The OS application menu bar: the native bar at the top of the screen on
//! macOS, and a per-window bar drawn by `gpui_component::menu::AppMenuBar`
//! on Linux and Windows, merged into the leading edge of the title bar.
//!
//! Reuses the same actions the keymap already binds in `keymap.rs` — a menu
//! item is only another way to reach them, not a second implementation — so
//! each one dims itself exactly where its matching shortcut would do
//! nothing: GPUI checks `is_action_available` against the same context the
//! keybinding is scoped to. The keystroke shown beside an item comes from the
//! keymap the same way, so the two cannot drift apart.
//!
//! An action that only exists once something has focus — a table tab's edits, a
//! query tab's run — is still listed, dimmed, rather than hidden: the menu is
//! also where a shortcut is discovered, and a menu that grows and shrinks as
//! focus moves is worse at that than one that stays put.

#[cfg(target_os = "macos")]
use gpui_kit::SystemMenuType;
use gpui_kit::base::GlobalState;
use gpui_kit::base::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::{App, KeyBinding, Menu, MenuItem, OsAction, actions};

use crate::app::{CloseConnection, NewConnection, NextConnection, PreviousConnection};
use crate::ui::agents::ToggleAgents;
use crate::ui::data_grid::{
    ClearRowSelection, CopyValue, CopyWithHeaders, SelectAllRows, ViewCell,
};
use crate::ui::query_editor::{
    EditVariables, Explain, ExplainAnalyze, FormatQuery, RunQuery, RunScript,
    RunScriptIgnoringErrors, ToggleComment,
};
use crate::ui::session::{
    BackupDatabase, CancelQuery, CloseTab, Disconnect, ImportSqlDump, NewTab, NewTable, NextTab,
    OpenConnectionInfo, OpenConsole, OpenErd, OpenFile, OpenHistory, OpenMaintenance,
    OpenProcessList, OpenQueryDigest, OpenServerVariables, OpenUserManagement, PreviousTab,
    QuickSwitcher, Reconnect, Refresh, SaveFile, SaveFileAs, SaveQuery, SaveQueryAs, SearchSchema,
    UploadFile,
};
use crate::ui::settings_window::OpenSettings;
use crate::ui::shortcuts_dialog::ShowShortcuts;
use crate::ui::table_view::{
    ApplyEdits, DeleteRows, DiscardEdits, InsertRow, RestoreRows, SetNull, ToggleRowPanel,
};

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

    cx.set_menus(menus());
    // The native bar on macOS reads the menus from the platform; on Linux
    // and Windows the in-window AppMenuBar reads them from the component
    // global instead — see Workspace::render.
    if let Some(menus) = cx.get_menus() {
        GlobalState::global_mut(cx).set_app_menus(menus);
    }
}

/// The menu bar itself.
///
/// Built apart from [`init`] so a test can read the tree back: `set_menus`
/// keeps it and does not hand it out.
fn menus() -> Vec<Menu> {
    let mut menus = Vec::new();

    // macOS puts the application menu first; there is no such convention on
    // Linux and Windows, where Settings and Quit live in File instead.
    #[cfg(target_os = "macos")]
    menus.push(Menu::new("Zippa DB").items(vec![
        MenuItem::action("Settings…", OpenSettings),
        MenuItem::separator(),
        // The Services submenu is the system's to fill in, and belongs
        // here rather than in a menu of our own.
        MenuItem::os_submenu("Services", SystemMenuType::Services),
        MenuItem::separator(),
        MenuItem::action("Hide Zippa DB", Hide),
        MenuItem::action("Hide Others", HideOthers),
        MenuItem::action("Show All", ShowAll),
        MenuItem::separator(),
        MenuItem::action("Quit Zippa DB", Quit),
    ]));

    #[cfg_attr(target_os = "macos", allow(unused_mut))]
    let mut file = vec![
        MenuItem::action("New Connection", NewConnection),
        MenuItem::action("New Query Tab", NewTab),
        MenuItem::separator(),
        MenuItem::action("Open…", OpenFile),
        MenuItem::action("Save", SaveFile),
        MenuItem::action("Save As…", SaveFileAs),
        MenuItem::action("Save to Saved Queries", SaveQuery),
        MenuItem::action("Save as New Saved Query…", SaveQueryAs),
        MenuItem::separator(),
        MenuItem::action("Close Tab", CloseTab),
        MenuItem::action("Close Connection", CloseConnection),
    ];
    #[cfg(not(target_os = "macos"))]
    file.extend([
        MenuItem::separator(),
        MenuItem::action("Settings…", OpenSettings),
        MenuItem::separator(),
        MenuItem::action("Quit", Quit),
    ]);
    menus.push(Menu::new("File").items(file));

    menus.extend([
        // The text boxes' own clipboard and undo, with the native selectors
        // macOS expects them to carry. They answer wherever an input has the
        // focus — the query editor, a cell, a dialog's fields.
        Menu::new("Edit").items(vec![
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("Query").items(vec![
            MenuItem::action("Run Query", RunQuery),
            MenuItem::action("Run All Queries", RunScript),
            MenuItem::action("Run All Queries, Skipping Errors", RunScriptIgnoringErrors),
            MenuItem::separator(),
            MenuItem::action("Explain", Explain),
            MenuItem::action("Explain Analyze", ExplainAnalyze),
            MenuItem::separator(),
            MenuItem::action("Edit Variables…", EditVariables),
            MenuItem::action("Format Query", FormatQuery),
            MenuItem::action("Toggle Comment", ToggleComment),
            MenuItem::separator(),
            MenuItem::action("Cancel Query", CancelQuery),
        ]),
        // Only a table tab answers these; on a query tab's grid they dim.
        Menu::new("Table").items(vec![
            MenuItem::action("Add Row", InsertRow),
            MenuItem::action("Set NULL", SetNull),
            MenuItem::action("Delete Rows", DeleteRows),
            MenuItem::action("Restore Rows", RestoreRows),
            MenuItem::separator(),
            MenuItem::action("Apply Edits", ApplyEdits),
            MenuItem::action("Discard Edits", DiscardEdits),
            MenuItem::separator(),
            MenuItem::action("Copy Cell or Rows", CopyValue),
            MenuItem::action("Copy Rows with Header", CopyWithHeaders),
            MenuItem::action("View Value", ViewCell),
            MenuItem::separator(),
            MenuItem::action("Select All Rows", SelectAllRows),
            MenuItem::action("Clear Row Selection", ClearRowSelection),
        ]),
        // The open connection and what is in it: the commands that used to
        // be icons in the title bar and beside the sidebar's filter, and the
        // tools the sidebar's Tools tab lists.
        Menu::new("Database").items(vec![
            MenuItem::action("Refresh", Refresh),
            MenuItem::action("Reconnect", Reconnect),
            MenuItem::action("Disconnect", Disconnect),
            MenuItem::action("Upload to SSH Host", UploadFile),
            MenuItem::separator(),
            MenuItem::action("New Table…", NewTable),
            MenuItem::action("Import SQL Dump…", ImportSqlDump),
            MenuItem::action("Back Up Database…", BackupDatabase),
            MenuItem::separator(),
            MenuItem::action("Search Schema…", SearchSchema),
            MenuItem::action("ER Diagram", OpenErd),
            MenuItem::separator(),
            MenuItem::action("Console", OpenConsole),
            MenuItem::action("Query History", OpenHistory),
            MenuItem::action("Connection Info", OpenConnectionInfo),
            MenuItem::action("Processes", OpenProcessList),
            MenuItem::action("Server Variables", OpenServerVariables),
            MenuItem::action("Query Digest", OpenQueryDigest),
            MenuItem::action("Users", OpenUserManagement),
            MenuItem::action("Maintenance", OpenMaintenance),
        ]),
        Menu::new("View").items(vec![
            MenuItem::action("Quick Switcher…", QuickSwitcher),
            MenuItem::action("Toggle Row Panel", ToggleRowPanel),
            MenuItem::action("Toggle Agents Panel", ToggleAgents),
            MenuItem::separator(),
            MenuItem::action("Next Tab", NextTab),
            MenuItem::action("Previous Tab", PreviousTab),
            MenuItem::separator(),
            MenuItem::action("Next Connection", NextConnection),
            MenuItem::action("Previous Connection", PreviousConnection),
        ]),
        Menu::new("Help").items(vec![MenuItem::action("Keyboard Shortcuts", ShowShortcuts)]),
    ]);
    menus
}

#[cfg(test)]
mod tests {
    use std::any::TypeId;
    use std::collections::HashSet;

    use gpui_kit::TestAppContext;

    use super::*;

    /// Every action the menus name, with the menu it sits in, so a failure can
    /// say where it was looking.
    fn menu_actions() -> Vec<(String, Box<dyn gpui_kit::Action>)> {
        let mut found = Vec::new();
        for menu in menus() {
            let Menu { name, items, .. } = menu;
            for item in items {
                if let MenuItem::Action {
                    name: item, action, ..
                } = item
                {
                    found.push((format!("{name} > {item}"), action));
                }
            }
        }
        found
    }

    #[test]
    fn every_core_action_has_a_menu_item() {
        let present: HashSet<TypeId> = menu_actions()
            .into_iter()
            .map(|(_, action)| action.as_any().type_id())
            .collect();

        // Every action the keymap binds, plus the app-level commands the OS
        // gives its own keys to. A binding with no menu item is a shortcut the
        // menu bar cannot teach.
        // (Only mutated on macOS, where the application menu adds its own
        // commands below.)
        #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
        let mut expected: Vec<(&str, TypeId)> = vec![
            ("New Connection", TypeId::of::<NewConnection>()),
            ("New Query Tab", TypeId::of::<NewTab>()),
            ("Open", TypeId::of::<OpenFile>()),
            ("Save", TypeId::of::<SaveFile>()),
            ("Save As", TypeId::of::<SaveFileAs>()),
            ("Import SQL Dump", TypeId::of::<ImportSqlDump>()),
            ("Back Up Database", TypeId::of::<BackupDatabase>()),
            ("Close Tab", TypeId::of::<CloseTab>()),
            ("Close Connection", TypeId::of::<CloseConnection>()),
            ("Reconnect", TypeId::of::<Reconnect>()),
            ("Disconnect", TypeId::of::<Disconnect>()),
            ("Upload to SSH Host", TypeId::of::<UploadFile>()),
            ("Run Query", TypeId::of::<RunQuery>()),
            ("Run All Queries", TypeId::of::<RunScript>()),
            (
                "Run All Queries, Skipping Errors",
                TypeId::of::<RunScriptIgnoringErrors>(),
            ),
            ("Explain", TypeId::of::<Explain>()),
            ("Explain Analyze", TypeId::of::<ExplainAnalyze>()),
            ("Toggle Comment", TypeId::of::<ToggleComment>()),
            ("Format Query", TypeId::of::<FormatQuery>()),
            ("Cancel Query", TypeId::of::<CancelQuery>()),
            ("Add Row", TypeId::of::<InsertRow>()),
            ("Set NULL", TypeId::of::<SetNull>()),
            ("Delete Rows", TypeId::of::<DeleteRows>()),
            ("Restore Rows", TypeId::of::<RestoreRows>()),
            ("Apply Edits", TypeId::of::<ApplyEdits>()),
            ("Discard Edits", TypeId::of::<DiscardEdits>()),
            ("Copy Cell or Rows", TypeId::of::<CopyValue>()),
            ("Copy Rows with Header", TypeId::of::<CopyWithHeaders>()),
            ("View Value", TypeId::of::<ViewCell>()),
            ("Select All Rows", TypeId::of::<SelectAllRows>()),
            ("Clear Row Selection", TypeId::of::<ClearRowSelection>()),
            ("Quick Switcher", TypeId::of::<QuickSwitcher>()),
            ("Search Schema", TypeId::of::<SearchSchema>()),
            ("Refresh", TypeId::of::<Refresh>()),
            ("Toggle Row Panel", TypeId::of::<ToggleRowPanel>()),
            ("Toggle Agents Panel", TypeId::of::<ToggleAgents>()),
            ("Console", TypeId::of::<OpenConsole>()),
            ("Query History", TypeId::of::<OpenHistory>()),
            ("Connection Info", TypeId::of::<OpenConnectionInfo>()),
            ("Processes", TypeId::of::<OpenProcessList>()),
            ("Server Variables", TypeId::of::<OpenServerVariables>()),
            ("Query Digest", TypeId::of::<OpenQueryDigest>()),
            ("Users", TypeId::of::<OpenUserManagement>()),
            ("Maintenance", TypeId::of::<OpenMaintenance>()),
            ("ER Diagram", TypeId::of::<OpenErd>()),
            ("Next Tab", TypeId::of::<NextTab>()),
            ("Previous Tab", TypeId::of::<PreviousTab>()),
            ("Next Connection", TypeId::of::<NextConnection>()),
            ("Previous Connection", TypeId::of::<PreviousConnection>()),
            ("Settings", TypeId::of::<OpenSettings>()),
            ("New Table", TypeId::of::<NewTable>()),
            ("Save to Saved Queries", TypeId::of::<SaveQuery>()),
            ("Save as New Saved Query", TypeId::of::<SaveQueryAs>()),
            ("Edit Variables", TypeId::of::<EditVariables>()),
            ("Keyboard Shortcuts", TypeId::of::<ShowShortcuts>()),
            ("Quit", TypeId::of::<Quit>()),
            ("Undo", TypeId::of::<Undo>()),
            ("Redo", TypeId::of::<Redo>()),
            ("Cut", TypeId::of::<Cut>()),
            ("Copy", TypeId::of::<Copy>()),
            ("Paste", TypeId::of::<Paste>()),
            ("Select All", TypeId::of::<SelectAll>()),
        ];
        // The application-menu commands only exist where there is an
        // application menu.
        #[cfg(target_os = "macos")]
        expected.extend([
            ("Hide", TypeId::of::<Hide>()),
            ("Hide Others", TypeId::of::<HideOthers>()),
            ("Show All", TypeId::of::<ShowAll>()),
        ]);
        for (what, id) in expected {
            assert!(present.contains(&id), "{what} is missing from the menu bar");
        }
    }

    #[test]
    fn no_menu_opens_or_closes_on_a_separator() {
        for menu in menus() {
            let separators: Vec<bool> = menu
                .items
                .iter()
                .map(|item| matches!(item, MenuItem::Separator))
                .collect();
            assert!(!separators.is_empty(), "{} has no items", menu.name);
            assert!(!separators[0], "{} opens on a separator", menu.name);
            assert!(
                !separators[separators.len() - 1],
                "{} closes on a separator",
                menu.name
            );
            assert!(
                !separators.windows(2).any(|pair| pair[0] && pair[1]),
                "{} has two separators in a row",
                menu.name
            );
        }
    }

    #[gpui_kit::test]
    fn the_in_window_menu_bar_gets_the_platform_menus(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::component::init(cx);
            crate::keymap::bind(cx);
            crate::menu::init(cx);
        });

        cx.update(|cx| {
            let stored: Vec<String> = GlobalState::global(cx)
                .app_menus()
                .iter()
                .map(|menu| menu.name.to_string())
                .collect();
            let built: Vec<String> = menus()
                .into_iter()
                .map(|menu| menu.name.to_string())
                .collect();
            assert_eq!(
                stored, built,
                "the in-window menu bar must see the same menus the platform got"
            );
        });
    }

    #[gpui_kit::test]
    fn every_menu_item_answers_to_a_keystroke(cx: &mut TestAppContext) {
        // The app-level commands the OS owns the keys for, and which the keymap
        // therefore does not bind: the menu is still where they belong. And
        // the commands that left a toolbar for the menu bar without being
        // frequent enough to spend a chord on: the menu is their keyboard
        // path.
        let unbound = [
            TypeId::of::<HideOthers>(),
            TypeId::of::<ShowAll>(),
            TypeId::of::<NewTable>(),
            TypeId::of::<SaveQueryAs>(),
            TypeId::of::<EditVariables>(),
            TypeId::of::<BackupDatabase>(),
        ];

        cx.update(|cx| {
            gpui_kit::component::init(cx);
            crate::keymap::bind(cx);
            // The app-level keys are bound beside the menu that carries them.
            crate::menu::init(cx);
        });

        cx.update(|cx| {
            let keymap = cx.key_bindings();
            let keymap = keymap.borrow();
            for (where_, action) in menu_actions() {
                if unbound.contains(&action.as_any().type_id()) {
                    continue;
                }
                assert!(
                    keymap.bindings_for_action(action.as_ref()).next().is_some(),
                    "{where_} ({}) shows no keystroke because none is bound",
                    action.name()
                );
            }
        });
    }
}
