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
use gpui_kit::{Context, Entity, SharedString, Window, div};
use regex::{Regex, RegexBuilder};

use crate::db::{DatabaseObject, ObjectKind};

use super::tab::ObjectViewMode;
use super::{Session, SessionEvent};

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

    /// Refill the tree from whatever the filter currently lets through, then
    /// point its selection at the active tab again — new items mean a fresh
    /// [`TreeState`], which forgets which one was selected.
    pub(super) fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let items = self
            .visible_objects()
            .into_iter()
            .map(|object| TreeItem::new(object.label(), object.label()))
            .collect::<Vec<_>>();
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

        let notice = match (
            &self.metadata_error,
            self.objects.is_empty(),
            objects.is_empty(),
        ) {
            (Some(error), _, _) => Some((error.clone(), cx.theme().danger)),
            (None, true, _) => Some((
                "No tables or views".to_string(),
                cx.theme().muted_foreground,
            )),
            (None, false, true) => Some((
                "Nothing matches the filter".to_string(),
                cx.theme().muted_foreground,
            )),
            (None, false, false) => None,
        };

        let heading = if filtering {
            format!("TABLES & VIEWS ({}/{})", objects.len(), self.objects.len())
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

/// Turn the filter box's text into a matcher.
///
/// The pattern is a case-insensitive regex; while it is still being typed it is
/// often not valid (`user(`), so an unparseable pattern falls back to matching
/// the text literally rather than showing nothing.
pub(super) fn compile_filter(pattern: &str) -> Option<Regex> {
    if pattern.is_empty() {
        return None;
    }

    let case_insensitive = |pattern: &str| {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .ok()
    };

    case_insensitive(pattern).or_else(|| case_insensitive(&regex::escape(pattern)))
}
