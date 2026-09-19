//! The object sidebar: every table and view in the open database, filtered.
//!
//! TODO.md section 4 starts here — a tree of databases, schemas, tables,
//! views, functions and the rest — so the list and the filter over it are
//! kept apart from the session that owns them.

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::list::ListItem;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::tree::{TreeItem, tree};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{ClipboardItem, Context, Entity, SharedString, Window, div};
use regex::Regex;

use crate::db::{DatabaseObject, ObjectKind, StoredKind, StoredObject};
use crate::ui::text_filter;

use super::tab::ObjectViewMode;
use super::{ImportSqlDump, Session, SessionEvent};

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
                Input::new(&self.filter)
                    .id("object-filter")
                    .small()
                    .cleanable(true),
            )
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

    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("sidebar")
            .test_support()
            .size_full()
            .p_2()
            .gap_3()
            .bg(cx.theme().sidebar)
            .border_r_1()
            .border_color(cx.theme().sidebar_border)
            .child(
                Button::new("import-dump")
                    .outline()
                    .small()
                    .w_full()
                    .label("Import SQL dump…")
                    .accessibility_label("Import a SQL dump into this connection")
                    .tooltip_with_action(
                        "Import a SQL dump into this connection",
                        &ImportSqlDump,
                        Some("Session"),
                    )
                    .on_click(cx.listener(|this, _, _window, cx| this.import_dump(cx))),
            )
            .child(self.render_objects(cx))
            .child(
                Button::new("disconnect")
                    .outline()
                    .small()
                    .w_full()
                    .label("Disconnect")
                    .tooltip("Close this connection and return to the connection manager")
                    .on_click(cx.listener(|_this, _, _window, cx| {
                        cx.emit(SessionEvent::Disconnected);
                    })),
            )
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
