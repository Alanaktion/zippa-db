//! The object sidebar: every table and view in the open database, filtered.
//!
//! TODO.md section 4 starts here — a tree of databases, schemas, tables,
//! views, functions and the rest — so the list and the filter over it are
//! kept apart from the session that owns them.

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, SharedString, Window, div};
use regex::{Regex, RegexBuilder};

use crate::db::{DatabaseObject, ObjectKind};

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
                div()
                    .id("objects")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .w_full()
                            // A button rather than a clickable row: the list
                            // is then reachable by tab, answers the keyboard,
                            // and announces a name of its own.
                            .children(objects.into_iter().map(|object| {
                                let label = object.label();
                                let kind = object.kind;
                                let object = object.clone();
                                let what = match kind {
                                    ObjectKind::View => "view",
                                    ObjectKind::Table => "table",
                                };

                                Button::new(SharedString::from(format!("object-{label}")))
                                    .ghost()
                                    .small()
                                    .w_full()
                                    // The name is a child rather than the
                                    // button's own label so it can sit at the
                                    // left, where a list of names is read
                                    // from; a button's label is centred.
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .min_w_0()
                                            .gap_2()
                                            .justify_between()
                                            .child(div().text_sm().truncate().child(label.clone()))
                                            .when(kind == ObjectKind::View, |this| {
                                                this.child(
                                                    div()
                                                        .flex_none()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child("view"),
                                                )
                                            }),
                                    )
                                    .accessibility_label(format!("Open the {what} {label}"))
                                    .tooltip(format!("Open the {what} {label}"))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_object(&object, window, cx)
                                    }))
                            })),
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
