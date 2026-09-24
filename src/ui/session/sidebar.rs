//! The object sidebar: the open database's tables and views, and the tools for
//! working with the server.
//!
//! Two top tabs share it. **Schema** shows the object list and the filter over
//! it, with the schema search and the SQL dump import inline beside the filter
//! (both are about the objects below them). **Management** holds the
//! DBA-facing views — the console, the process list, the server variables, and
//! the query digest — as icon rows, since they are destinations rather than
//! actions on the object list. SQLite has no server, so it lists the console
//! and its own maintenance tab instead.
//!
//! TODO.md section 4 starts here — a tree of databases, schemas, tables,
//! views, functions and the rest — so the list and the filter over it are
//! kept apart from the session that owns them.

use gpui_kit::assets::IconName;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::tree::{TreeItem, tree};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Action, ClipboardItem, Context, Entity, SharedString, Window, div};
use regex::Regex;

use crate::db::{DatabaseObject, Engine, ObjectKind, StoredKind, StoredObject};
use crate::ui::text_filter;

use super::tab::ObjectViewMode;
use super::{
    ImportSqlDump, OpenConsole, OpenMaintenance, OpenProcessList, OpenQueryDigest,
    OpenServerVariables, SearchSchema, Session,
};

impl Session {
    pub(super) fn on_filter_event(
        &mut self,
        filter: &Entity<InputState>,
        event: &InputEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(event, InputEvent::Change) {
            return;
        }

        let pattern = filter.read(cx).value().trim().to_string();
        self.matcher = compile_filter(&pattern);
        self.rebuild_tree(cx);
        cx.notify();
    }

    /// The objects the filter lets through, in sidebar order.
    pub(super) fn visible_objects(&self) -> Vec<&DatabaseObject> {
        self.objects
            .iter()
            .filter(|object| match &self.matcher {
                Some(matcher) => matcher.is_match(&object.label()),
                None => true,
            })
            .collect()
    }

    /// The functions, procedures and sequences the filter lets through.
    pub(super) fn visible_stored(&self) -> Vec<&StoredObject> {
        self.stored
            .iter()
            .filter(|object| match &self.matcher {
                Some(matcher) => matcher.is_match(&object.label()),
                None => true,
            })
            .collect()
    }

    /// Refill the tree from whatever the filter currently lets through, then
    /// point its selection at the active tab again — new items mean a fresh
    /// [`TreeState`], which forgets which one was selected.
    pub(super) fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let mut items = self
            .visible_objects()
            .into_iter()
            .map(|object| TreeItem::new(object.label(), object.label()))
            .collect::<Vec<_>>();

        // One folder per kind, after the tables and only when it has members.
        let stored = self.visible_stored();
        for kind in [
            StoredKind::Function,
            StoredKind::Procedure,
            StoredKind::Sequence,
        ] {
            let children = stored
                .iter()
                .filter(|object| object.kind == kind)
                .map(|object| TreeItem::new(stored_id(object), object.label()))
                .collect::<Vec<_>>();
            if !children.is_empty() {
                let (id, title) = folder(kind);
                items.push(
                    TreeItem::new(id, format!("{title} ({})", children.len()))
                        .expanded(self.matcher.is_some())
                        .children(children),
                );
            }
        }
        self.objects_tree
            .update(cx, |tree, cx| tree.set_items(items, cx));
        self.sync_tree_selection(cx);
    }

    /// Highlight the table the active tab shows, or nothing for a query tab.
    pub(super) fn sync_tree_selection(&mut self, cx: &mut Context<Self>) {
        let label = self
            .active_panel()
            .and_then(|panel| panel.read(cx).object(cx))
            .map(|object| object.label());
        self.objects_tree.update(cx, |tree, cx| {
            let index = label.and_then(|label| tree.index_of(&SharedString::from(label)));
            tree.set_selected_index(index, cx);
        });
    }

    fn render_objects(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let objects = self.visible_objects();
        let filtering = self.matcher.is_some();

        let stored_hidden = self.visible_stored().is_empty();
        let notice = match (
            &self.metadata_error,
            self.objects.is_empty() && self.stored.is_empty(),
            objects.is_empty() && stored_hidden,
        ) {
            (Some(error), _, _) => Some((error.clone(), cx.theme().danger)),
            (None, true, _) => Some((
                "No tables, views or routines".to_string(),
                cx.theme().muted_foreground,
            )),
            (None, false, true) => Some((
                "Nothing matches the filter".to_string(),
                cx.theme().muted_foreground,
            )),
            (None, false, false) => None,
        };

        let heading = if filtering {
            format!(
                "TABLES & VIEWS ({}/{})",
                objects.len() + self.visible_stored().len(),
                self.objects.len() + self.stored.len()
            )
        } else {
            "TABLES & VIEWS".to_string()
        };

        let session = cx.entity();
        let menu_session = session.clone();

        v_flex()
            .flex_1()
            .min_h_0()
            .gap_1()
            .child(
                div()
                    .px_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(heading),
            )
            .when_some(notice, |this, (message, color)| {
                this.child(div().px_1().text_xs().text_color(color).child(message))
            })
            .child(
                div().id("objects").flex_1().min_h_0().child(
                    tree(
                        &self.objects_tree,
                        move |_ix, entry, selected, _window, cx| {
                            session.update(cx, |this, cx| {
                                let label = entry.item().label.clone();
                                if entry.is_folder() {
                                    return ListItem::new(SharedString::from(format!(
                                        "folder-{}",
                                        entry.item().id
                                    )))
                                    .selected(selected)
                                    .child(div().text_sm().truncate().child(label));
                                }
                                if let Some(stored) = this
                                    .stored
                                    .iter()
                                    .find(|object| stored_id(object) == entry.item().id.as_ref())
                                {
                                    return ListItem::new(SharedString::from(format!(
                                        "stored-{}",
                                        entry.item().id
                                    )))
                                    .selected(selected)
                                    .child(div().text_sm().truncate().child(stored.label()));
                                }
                                let object = this
                                    .objects
                                    .iter()
                                    .find(|object| object.label() == label.as_ref())
                                    .cloned();
                                let kind = object.as_ref().map(|object| object.kind);

                                ListItem::new(SharedString::from(format!("object-{label}")))
                                    .selected(selected)
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .min_w_0()
                                            .gap_2()
                                            .justify_between()
                                            .child(div().text_sm().truncate().child(label.clone()))
                                            .when(kind == Some(ObjectKind::View), |this| {
                                                this.child(
                                                    div()
                                                        .flex_none()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child("view"),
                                                )
                                            }),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(object) = object.clone() {
                                            this.open_object(
                                                &object,
                                                ObjectViewMode::Data,
                                                window,
                                                cx,
                                            );
                                        }
                                    }))
                            })
                        },
                    )
                    .context_menu(move |_ix, entry, menu, _window, cx| {
                        let label = entry.item().label.clone();
                        let stored = menu_session
                            .read(cx)
                            .stored
                            .iter()
                            .find(|object| stored_id(object) == entry.item().id.as_ref())
                            .cloned();
                        if let Some(stored) = stored {
                            return menu.item(PopupMenuItem::new("Copy name").on_click(
                                move |_, _window, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        stored.name.clone(),
                                    ));
                                },
                            ));
                        }
                        let object = menu_session
                            .read(cx)
                            .objects
                            .iter()
                            .find(|object| object.label() == label.as_ref())
                            .cloned();
                        let Some(object) = object else {
                            return menu;
                        };

                        let open_session = menu_session.clone();
                        let open_object = object.clone();
                        let inspect_session = menu_session.clone();
                        let inspect_object = object;

                        menu.item(PopupMenuItem::new("Open").on_click(move |_, window, cx| {
                            open_session.update(cx, |this, cx| {
                                this.open_object(&open_object, ObjectViewMode::Data, window, cx);
                            });
                        }))
                        .item(
                            PopupMenuItem::new("Inspect structure").on_click(
                                move |_, window, cx| {
                                    inspect_session.update(cx, |this, cx| {
                                        this.open_object(
                                            &inspect_object,
                                            ObjectViewMode::Schema,
                                            window,
                                            cx,
                                        );
                                    });
                                },
                            ),
                        )
                    })
                    .size_full(),
                ),
            )
    }

    /// The sidebar: the two top tabs, then whichever tab is showing.
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.sidebar_tab {
            SidebarTab::Schema => self.render_schema_tab(cx).into_any_element(),
            SidebarTab::Management => self.render_management_tab(cx).into_any_element(),
        };

        v_flex()
            .id("sidebar")
            .test_support()
            .size_full()
            .p_1()
            .gap_2()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(content)
            .child(self.render_sidebar_tabs(cx))
    }

    /// The two tabs at the top of the sidebar: the object list and the
    /// server tools. A segmented bar reads as a section switch rather than a
    /// strip of document tabs, which is what these are.
    fn render_sidebar_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        TabBar::new("sidebar-tabs")
            .segmented()
            .small()
            .text_sm()
            .w_full()
            .selected_index(self.sidebar_tab.index())
            .on_click(cx.listener(|this, index: &usize, _window, cx| {
                let tab = SidebarTab::from_index(*index);
                if this.sidebar_tab != tab {
                    this.sidebar_tab = tab;
                    cx.notify();
                }
            }))
            .child(Tab::new().label("Schema").flex_1())
            .child(Tab::new().label("Management").flex_1())
    }

    /// The Schema tab: the filter with its two inline actions, then the list of
    /// tables, views and routines the filter lets through.
    fn render_schema_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .flex_1()
            .min_h_0()
            .gap_2()
            .child(self.render_object_filter(cx))
            .child(self.render_objects(cx))
    }

    /// The filter box, with the schema search and the SQL dump import beside it.
    /// Both open something about the objects below them, so they live on the
    /// filter's line rather than as full-width buttons of their own.
    fn render_object_filter(&self, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .items_center()
            .gap_1()
            .child(
                Input::new(&self.filter)
                    .id("object-filter")
                    .small()
                    .cleanable(true)
                    .flex_1(),
            )
            .child(
                Button::new("search-schema")
                    .ghost()
                    .small()
                    .icon(IconName::Binoculars)
                    .accessibility_label("Find in schema")
                    .tooltip_with_action(
                        "Find things in the current schema",
                        &SearchSchema,
                        Some("Session"),
                    )
                    .on_click(cx.listener(|_this, _, window, cx| {
                        let session = cx.entity().clone();
                        crate::ui::schema_search::open(session, window, cx);
                    })),
            )
            .child(
                Button::new("import-dump")
                    .ghost()
                    .small()
                    .icon(IconName::Import)
                    .accessibility_label("Import SQL dump")
                    .tooltip_with_action(
                        "Import a SQL dump into this connection",
                        &ImportSqlDump,
                        Some("Session"),
                    )
                    .on_click(cx.listener(|this, _, _window, cx| this.import_dump(cx))),
            )
    }

    /// The Management tab: the connection's own tools as icon rows, since each
    /// opens a view rather than acting on an object. SQLite has no server to
    /// ask, so it gets the console and its own maintenance tab instead of the
    /// server views.
    fn render_management_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let server = self.connection.config.engine != Engine::Sqlite;

        v_flex()
            .flex_1()
            .min_h_0()
            .gap_1()
            .child(
                tool_row(
                    "open-console",
                    IconName::SquareTerminal,
                    "Console",
                    "Every statement this connection has sent",
                    &OpenConsole,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_console(window, cx))),
            )
            .when(!server, |this| {
                this.child(
                    tool_row(
                        "open-maintenance",
                        IconName::Wrench,
                        "Maintenance",
                        "Integrity checks, optimize, vacuum",
                        &OpenMaintenance,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.open_maintenance(window, cx))),
                )
            })
            .when(server, |this| {
                this.child(
                    tool_row(
                        "open-process-list",
                        IconName::Activity,
                        "Processes",
                        "Who is connected and what they're doing right now",
                        &OpenProcessList,
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_process_list(window, cx)),
                    ),
                )
                .child(
                    tool_row(
                        "open-server-variables",
                        IconName::SlidersHorizontal,
                        "Variables",
                        "The server's own configuration",
                        &OpenServerVariables,
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_server_variables(window, cx)),
                    ),
                )
                .child(
                    tool_row(
                        "open-query-digest",
                        IconName::Gauge,
                        "Query Digest",
                        "Slow and frequent statements, ranked by mean time",
                        &OpenQueryDigest,
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_query_digest(window, cx)),
                    ),
                )
            })
    }
}

/// One tool row in the Management tab.
///
/// The icon sits in a fixed slot before the label so the labels line up down
/// the panel; the label is left-aligned rather than centred the way a `Button`
/// lays its own content out, because these are navigation destinations read as
/// a list. The tooltip carries the action's shortcut.
fn tool_row(
    id: &'static str,
    icon: IconName,
    label: &'static str,
    tooltip: &'static str,
    action: &dyn Action,
) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .w_full()
        .accessibility_label(label)
        .tooltip_with_action(tooltip, action, Some("Session"))
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .child(Icon::new(icon).small())
                .child(div().min_w_0().truncate().child(label)),
        )
}

/// Which of the sidebar's two tabs is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SidebarTab {
    /// The object list: tables, views and routines, with the filter over it.
    Schema,
    /// The DBA-facing tools built on this connection.
    Management,
}

impl SidebarTab {
    /// The tab's position in the tab bar.
    fn index(self) -> usize {
        match self {
            SidebarTab::Schema => 0,
            SidebarTab::Management => 1,
        }
    }

    /// The tab a click on `index` selects; anything past the last named tab is
    /// the last one, so a click can never land on nothing.
    fn from_index(index: usize) -> Self {
        match index {
            0 => SidebarTab::Schema,
            _ => SidebarTab::Management,
        }
    }
}

/// A tree id for a function, procedure or sequence, which cannot collide with
/// the plain label a table or view uses.
fn stored_id(object: &StoredObject) -> String {
    let (id, _) = folder(object.kind);
    format!("{id}:{}", object.label())
}

/// The folder a kind is listed under: its tree id and its title.
fn folder(kind: StoredKind) -> (&'static str, &'static str) {
    match kind {
        StoredKind::Function => ("folder:functions", "Functions"),
        StoredKind::Procedure => ("folder:procedures", "Procedures"),
        StoredKind::Sequence => ("folder:sequences", "Sequences"),
    }
}

/// Turn the filter box's text into a matcher; see [`text_filter::compile`].
pub(super) fn compile_filter(pattern: &str) -> Option<Regex> {
    text_filter::compile(pattern)
}
