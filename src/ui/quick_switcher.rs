//! Quick switcher / command palette (`Cmd+K`).
//!
//! Provides fast search and navigation across:
//! - Open query and table tabs
//! - Tables and views in the active database schema
//! - Quick actions (New Query Tab, Open SQL File, Refresh)
//! - Available databases on the current connection
//!
//! Matching is done here, not by the `Command` component: every keystroke
//! re-ranks the rows with [`fuzzy::match_score`] — exact matches first,
//! then prefix, substring, and fuzzy subsequence hits — so the highlight
//! always sits on the best match.

use std::rc::Rc;

use gpui_kit::assets::IconName;
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, IntoElement, Render, SharedString, Window, div, px};

use crate::db::{DatabaseObject, ObjectKind};
use crate::ui::session::Session;
use crate::ui::session::tab::ObjectViewMode;

use super::fuzzy;

/// Actions and destinations selectable from the quick switcher.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SwitcherTarget {
    Tab(usize),
    Object(DatabaseObject),
    NewTab,
    OpenFile,
    Refresh,
    Explain,
    ExplainAnalyze,
    SwitchDatabase(String),
    SearchSchema,
    OpenConsole,
    OpenProcessList,
    OpenServerVariables,
    OpenQueryDigest,
    OpenMaintenance,
}

/// One palette row before it becomes a `CommandItem`: the label and keywords
/// the fuzzy matcher scores, and everything the row is built from.
struct PaletteItem {
    target: SwitcherTarget,
    label: String,
    icon: IconName,
    checked: bool,
    keywords: Vec<String>,
}

/// A palette row for a fixed action: label, icon and keywords only.
fn action(target: SwitcherTarget, label: &str, icon: IconName, keywords: &[&str]) -> PaletteItem {
    PaletteItem {
        target,
        label: label.to_string(),
        icon,
        checked: false,
        keywords: keywords.iter().map(|keyword| keyword.to_string()).collect(),
    }
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

    /// Do what the chosen item asks for.
    ///
    /// The items name a destination rather than carrying an action to dispatch:
    /// the dialog's focus path runs to the workspace, and the session and editor
    /// an action would have to reach are a sibling branch of it, so a dispatched
    /// action finds no listener — which is what left these items inert. Every
    /// destination is driven through the session here instead.
    fn choose(&self, target: SwitcherTarget, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.session.clone();
        match target {
            SwitcherTarget::Tab(ix) => {
                session.update(cx, |session, cx| session.activate_tab(ix, window, cx))
            }
            SwitcherTarget::Object(object) => session.update(cx, |session, cx| {
                session.open_object(&object, ObjectViewMode::Data, window, cx)
            }),
            SwitcherTarget::SwitchDatabase(database) => session.update(cx, |session, cx| {
                session.request_switch_database(database, window, cx)
            }),
            SwitcherTarget::NewTab => {
                session.update(cx, |session, cx| session.new_query_tab(window, cx))
            }
            SwitcherTarget::OpenFile => session.update(cx, |session, cx| session.open_file(cx)),
            SwitcherTarget::Refresh => session.update(cx, |session, cx| session.refresh(cx)),
            SwitcherTarget::Explain => {
                session.update(cx, |session, cx| session.explain_active(false, cx))
            }
            SwitcherTarget::ExplainAnalyze => {
                session.update(cx, |session, cx| session.explain_active(true, cx))
            }
            // A dialog of its own, and only one can be open at a time: the
            // caller has closed this one already.
            SwitcherTarget::SearchSchema => {
                let _ = crate::ui::schema_search::open(session, window, cx);
            }
            SwitcherTarget::OpenConsole => {
                session.update(cx, |session, cx| session.open_console(window, cx))
            }
            SwitcherTarget::OpenProcessList => {
                session.update(cx, |session, cx| session.open_process_list(window, cx))
            }
            SwitcherTarget::OpenServerVariables => {
                session.update(cx, |session, cx| session.open_server_variables(window, cx))
            }
            SwitcherTarget::OpenQueryDigest => {
                session.update(cx, |session, cx| session.open_query_digest(window, cx))
            }
            SwitcherTarget::OpenMaintenance => {
                session.update(cx, |session, cx| session.open_maintenance(window, cx))
            }
        }
    }

    /// Do what a chosen item asks for, for a test to drive.
    #[cfg(test)]
    pub(crate) fn choose_for_test(
        &self,
        target: SwitcherTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.choose(target, window, cx);
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
        let (tabs_info, objects, databases, is_file_based, current_db, active_tab_ix) = {
            let s = self.session.read(cx);
            let tabs: Vec<(SharedString, IconName, bool, Option<String>)> = s
                .panels()
                .iter()
                .map(|panel| {
                    let panel = panel.read(cx);
                    (
                        panel.title(),
                        panel.icon(),
                        panel.is_query(),
                        panel.file_name(),
                    )
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

        let mut tab_items = Vec::new();

        // 1. Open Tabs
        if !tabs_info.is_empty() {
            for (ix, (title, icon, is_query, path)) in tabs_info.into_iter().enumerate() {
                let mut keywords = vec![
                    "tab".to_string(),
                    if is_query { "query" } else { "table" }.to_string(),
                    "open".to_string(),
                ];
                if let Some(p) = &path {
                    keywords.push(p.clone());
                }
                tab_items.push(PaletteItem {
                    target: SwitcherTarget::Tab(ix),
                    label: title.to_string(),
                    icon,
                    checked: ix == active_tab_ix,
                    keywords,
                });
            }
        }

        // 2. Database Objects (Tables & Views)
        let mut obj_items = Vec::new();
        if !objects.is_empty() {
            for obj in objects {
                let icon = match obj.kind {
                    ObjectKind::Table => IconName::Table,
                    ObjectKind::View => IconName::Eye,
                };
                let kind_str = match obj.kind {
                    ObjectKind::Table => "table",
                    ObjectKind::View => "view",
                };
                let mut keywords = vec![
                    kind_str.to_string(),
                    "schema".to_string(),
                    "object".to_string(),
                ];
                if let Some(schema) = &obj.schema {
                    keywords.push(schema.clone());
                }
                let label = obj.label();
                obj_items.push(PaletteItem {
                    target: SwitcherTarget::Object(obj),
                    label,
                    icon,
                    checked: false,
                    keywords,
                });
            }
        }

        // 3. Actions / Commands
        let mut act_items = vec![
            action(
                SwitcherTarget::NewTab,
                "New Query Tab",
                IconName::Plus,
                &["new", "query", "tab", "sql", "editor", "create"],
            ),
            action(
                SwitcherTarget::OpenFile,
                "Open SQL File...",
                IconName::FolderOpen,
                &["open", "file", "sql", "load", "import"],
            ),
            action(
                SwitcherTarget::Refresh,
                "Refresh Schema & Tables",
                IconName::RefreshCw,
                &["refresh", "reload", "schema", "tables", "metadata"],
            ),
            action(
                SwitcherTarget::SearchSchema,
                "Search Schema...",
                IconName::Search,
                &[
                    "search", "find", "schema", "column", "index", "routine", "trigger",
                ],
            ),
            action(
                SwitcherTarget::Explain,
                "Explain Query",
                IconName::Route,
                &["explain", "plan", "query", "cost", "tree"],
            ),
            action(
                SwitcherTarget::ExplainAnalyze,
                "Explain Query & Analyze",
                IconName::Gauge,
                &["explain", "analyze", "plan", "query", "timing"],
            ),
            action(
                SwitcherTarget::OpenConsole,
                "Console",
                IconName::SquareTerminal,
                &["console", "log", "statements", "history"],
            ),
        ];
        if is_file_based {
            act_items.push(action(
                SwitcherTarget::OpenMaintenance,
                "Maintenance",
                IconName::Wrench,
                &[
                    "maintenance",
                    "integrity",
                    "check",
                    "optimize",
                    "vacuum",
                    "analyze",
                ],
            ));
        } else {
            act_items.extend([
                action(
                    SwitcherTarget::OpenProcessList,
                    "Processes",
                    IconName::Activity,
                    &["processes", "connections", "activity", "kill"],
                ),
                action(
                    SwitcherTarget::OpenServerVariables,
                    "Variables",
                    IconName::SlidersHorizontal,
                    &["variables", "settings", "configuration", "server"],
                ),
                action(
                    SwitcherTarget::OpenQueryDigest,
                    "Query Digest",
                    IconName::Gauge,
                    &["digest", "slow", "queries", "performance"],
                ),
            ]);
        }

        // 4. Databases (if multi-database engine and databases known)
        let mut db_items = Vec::new();
        if !is_file_based && databases.len() > 1 {
            for db in databases {
                let is_current = db == current_db;
                db_items.push(PaletteItem {
                    target: SwitcherTarget::SwitchDatabase(db.clone()),
                    label: format!("Database: {db}"),
                    icon: IconName::HardDrive,
                    checked: is_current,
                    keywords: vec![
                        "database".to_string(),
                        "db".to_string(),
                        "switch".to_string(),
                    ],
                });
            }
        }

        // Rank every group against the query: exact matches first, then
        // prefix, substring, and fuzzy hits. The component's own substring
        // filter stays off — it would only hide what was already ranked.
        let query_text = self.state.read(cx).query(cx);
        let query = query_text.trim();

        let mut targets: Vec<Vec<SwitcherTarget>> = Vec::new();
        let mut command = Command::new(&self.state)
            .placeholder("Search tables, views, open queries, commands...")
            .bordered(false)
            .max_h(px(360.))
            .filterable(false)
            .empty(|_, _, cx| {
                div()
                    .p_6()
                    .text_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No matching tables, views, queries or commands")
            });

        // Re-rank on every keystroke.
        let view = cx.weak_entity();
        command = command.on_query(move |_, _, cx| {
            if let Some(view) = view.upgrade() {
                view.update(cx, |_, cx| cx.notify());
            }
        });

        for (group_label, items) in [
            ("Open Tabs", tab_items),
            ("Tables & Views", obj_items),
            ("Actions", act_items),
            ("Switch Database", db_items),
        ] {
            let mut scored: Vec<(i64, PaletteItem)> = items
                .into_iter()
                .filter_map(|item| {
                    let score = item
                        .keywords
                        .iter()
                        .fold(fuzzy::match_score(query, &item.label), |best, keyword| {
                            best.max(fuzzy::match_score(query, keyword))
                        });
                    score.map(|score| (score, item))
                })
                .collect();
            if scored.is_empty() {
                continue;
            }
            // Stable: ties keep the group's original order.
            scored.sort_by_key(|item| std::cmp::Reverse(item.0));

            let mut group_targets = Vec::with_capacity(scored.len());
            let mut group = CommandGroup::new().label(group_label);
            for (_, item) in scored {
                let PaletteItem {
                    target,
                    label,
                    icon,
                    checked,
                    keywords,
                } = item;
                group_targets.push(target);
                group = group.item(
                    CommandItem::new()
                        .label(label)
                        .icon(icon)
                        .checked(checked)
                        .keywords(keywords),
                );
            }
            targets.push(group_targets);
            command = command.group(group);
        }

        let targets = Rc::new(targets);
        let targets_for_confirm = targets.clone();
        let view_for_confirm = cx.weak_entity();

        command = command
            .on_confirm(move |index_path, window, cx| {
                let target = targets_for_confirm
                    .get(index_path.section)
                    .and_then(|section| section.get(index_path.row))
                    .cloned();

                // Taken before the dialog is closed: closing drops the last
                // strong reference to this view, and the choice still has to be
                // carried through it.
                let view = view_for_confirm.upgrade();

                // The dialog goes first. Several of these open a dialog of
                // their own — the file picker, the schema search, the
                // confirmation an analysed write asks for — and closing after
                // would take that one off the screen instead. Doing it first
                // also leaves the focus where the action puts it rather than
                // back on the grid.
                window.close_dialog(cx);

                let (Some(target), Some(view)) = (target, view) else {
                    return;
                };
                view.update(cx, |view, cx| view.choose(target, window, cx));
            })
            .on_cancel(move |window, cx| {
                window.close_dialog(cx);
            });

        command
    }
}
