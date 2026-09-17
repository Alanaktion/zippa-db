//! Quick switcher / fuzzy object palette (`Cmd+K`).
//!
//! Provides fast search and navigation across:
//! - Open query and table tabs
//! - Tables and views in the active database schema
//! - Quick actions (New Query Tab, Open SQL File, Refresh)
//! - Available databases on the current connection

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, IntoElement, Render, SharedString, Window, div, px};

use crate::db::{DatabaseObject, ObjectKind};
use crate::ui::session::tab::ObjectViewMode;
use crate::ui::session::{NewTab, OpenFile, Refresh, Session};

/// Actions and destinations selectable from the quick switcher.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SwitcherTarget {
    Tab(usize),
    Object(DatabaseObject),
    NewTab,
    OpenFile,
    Refresh,
    SwitchDatabase(String),
}

pub struct QuickSwitcherView {
    state: Entity<CommandState>,
    session: Entity<Session>,
}

impl QuickSwitcherView {
    pub fn new(session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        Self { state, session }
    }
}

/// Open the quick switcher dialog over the active window.
pub fn open(session: Entity<Session>, window: &mut Window, cx: &mut App) {
    if window.has_active_dialog(cx) {
        return;
    }

    let switcher = cx.new(|cx| QuickSwitcherView::new(session, window, cx));
    let state = switcher.read(cx).state.clone();

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .w(px(580.))
            .margin_top(px(80.))
            .p_0()
            .close_button(false)
            .overlay_closable(true)
            .keyboard(true)
            .child(switcher.clone())
    });

    state.update(cx, |state, cx| state.focus(window, cx));
}

impl Render for QuickSwitcherView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.session.clone();
        let (tabs_info, objects, databases, is_file_based, current_db, active_tab_ix) = {
            let s = self.session.read(cx);
            let tabs: Vec<(SharedString, bool, Option<String>)> = s
                .panels()
                .iter()
                .map(|panel| {
                    let panel = panel.read(cx);
                    (panel.title(), panel.is_query(), panel.file_name())
                })
                .collect();
            let objects = s.objects().to_vec();
            let databases = s.databases().to_vec();
            let is_file_based = s.connection().config.engine.is_file_based();
            let current_db = s.connection().database().to_string();
            let active_tab_ix = s.active_tab_index();
            (
                tabs,
                objects,
                databases,
                is_file_based,
                current_db,
                active_tab_ix,
            )
        };

        let mut targets: Vec<Vec<SwitcherTarget>> = Vec::new();
        let mut groups: Vec<CommandGroup> = Vec::new();

        // 1. Open Tabs
        if !tabs_info.is_empty() {
            let mut tab_targets = Vec::new();
            let mut tab_group = CommandGroup::new().label("Open Tabs");
            for (ix, (title, is_query, path)) in tabs_info.into_iter().enumerate() {
                let icon = if is_query {
                    IconName::SquareTerminal
                } else {
                    IconName::Table
                };
                let mut keywords = vec!["tab", if is_query { "query" } else { "table" }, "open"];
                if let Some(p) = &path {
                    keywords.push(p.as_str());
                }
                tab_group = tab_group.item(
                    CommandItem::new()
                        .label(title)
                        .icon(icon)
                        .checked(ix == active_tab_ix)
                        .keywords(keywords),
                );
                tab_targets.push(SwitcherTarget::Tab(ix));
            }
            targets.push(tab_targets);
            groups.push(tab_group);
        }

        // 2. Database Objects (Tables & Views)
        if !objects.is_empty() {
            let mut obj_targets = Vec::new();
            let mut obj_group = CommandGroup::new().label("Tables & Views");
            for obj in objects {
                let icon = match obj.kind {
                    ObjectKind::Table => IconName::Table,
                    ObjectKind::View => IconName::Eye,
                };
                let kind_str = match obj.kind {
                    ObjectKind::Table => "table",
                    ObjectKind::View => "view",
                };
                let mut keywords = vec![kind_str, "schema", "object"];
                if let Some(schema) = &obj.schema {
                    keywords.push(schema.as_str());
                }
                obj_group = obj_group.item(
                    CommandItem::new()
                        .label(obj.label())
                        .icon(icon)
                        .keywords(keywords),
                );
                obj_targets.push(SwitcherTarget::Object(obj));
            }
            targets.push(obj_targets);
            groups.push(obj_group);
        }

        // 3. Actions / Commands
        {
            let mut act_targets = Vec::new();
            let mut act_group = CommandGroup::new().label("Actions");

            act_group = act_group.item(
                CommandItem::new()
                    .label("New Query Tab")
                    .icon(IconName::Plus)
                    .action(Box::new(NewTab))
                    .keywords(["new", "query", "tab", "sql", "editor", "create"]),
            );
            act_targets.push(SwitcherTarget::NewTab);

            act_group = act_group.item(
                CommandItem::new()
                    .label("Open SQL File...")
                    .icon(IconName::FolderOpen)
                    .action(Box::new(OpenFile))
                    .keywords(["open", "file", "sql", "load", "import"]),
            );
            act_targets.push(SwitcherTarget::OpenFile);

            act_group = act_group.item(
                CommandItem::new()
                    .label("Refresh Schema & Tables")
                    .icon(IconName::RefreshCw)
                    .action(Box::new(Refresh))
                    .keywords(["refresh", "reload", "schema", "tables", "metadata"]),
            );
            act_targets.push(SwitcherTarget::Refresh);

            targets.push(act_targets);
            groups.push(act_group);
        }

        // 4. Databases (if multi-database engine and databases known)
        if !is_file_based && databases.len() > 1 {
            let mut db_targets = Vec::new();
            let mut db_group = CommandGroup::new().label("Switch Database");
            for db in databases {
                let is_current = db == current_db;
                db_group = db_group.item(
                    CommandItem::new()
                        .label(format!("Database: {db}"))
                        .icon(IconName::HardDrive)
                        .checked(is_current)
                        .keywords(["database", "db", "switch"]),
                );
                db_targets.push(SwitcherTarget::SwitchDatabase(db));
            }
            targets.push(db_targets);
            groups.push(db_group);
        }

        let targets = Rc::new(targets);
        let targets_for_confirm = targets.clone();
        let session_for_confirm = session.clone();

        let mut command = Command::new(&self.state)
            .placeholder("Search tables, views, open queries, commands...")
            .bordered(false)
            .max_h(px(360.))
            .empty(|_, _, cx| {
                div()
                    .p_6()
                    .text_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No matching tables, views, queries or commands")
            })
            .on_confirm(move |index_path, window, cx| {
                if let Some(target) = targets_for_confirm
                    .get(index_path.section)
                    .and_then(|s| s.get(index_path.row))
                {
                    let target = target.clone();
                    session_for_confirm.update(cx, |session, cx| match target {
                        SwitcherTarget::Tab(ix) => {
                            session.activate_tab(ix, window, cx);
                        }
                        SwitcherTarget::Object(object) => {
                            session.open_object(&object, ObjectViewMode::Data, window, cx);
                        }
                        SwitcherTarget::SwitchDatabase(db) => {
                            session.switch_database(db, cx);
                        }
                        SwitcherTarget::NewTab
                        | SwitcherTarget::OpenFile
                        | SwitcherTarget::Refresh => {
                            // Handled via the item's dispatched Action.
                        }
                    });
                }
                window.close_dialog(cx);
            })
            .on_cancel(move |window, cx| {
                window.close_dialog(cx);
            });

        for group in groups {
            command = command.group(group);
        }

        command
    }
}
