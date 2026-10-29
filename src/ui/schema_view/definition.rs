//! The structure tab's read-only half: the `Form | SQL` toggle, the SQL
//! panel showing the current database definition, and the Constraints and
//! Triggers sections below the editable forms.
//!
//! The SQL panel never shows the form's pending edits — it is the definition
//! as the server holds it, re-read on every reload — and says so when the
//! form has unsaved changes.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClipboardItem, Context, div};

use crate::db::ObjectKind;
use crate::ui::sql_file;

use super::{Panel, SchemaView, SchemaViewEvent};

impl SchemaView {
    /// The `Form | SQL` toggle. A view has no form, so its button is
    /// disabled rather than hidden — the tab opens on SQL either way.
    pub(super) fn render_panel_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let is_view = self.object.kind == ObjectKind::View;
        let form_tooltip = if is_view {
            "Views have no editable structure"
        } else {
            "Edit the table's columns, indexes, and foreign keys"
        };
        h_flex()
            .gap_1()
            .child(
                Button::new("structure-panel-form")
                    .ghost()
                    .xsmall()
                    .label("Form")
                    .selected(self.panel == Panel::Form)
                    .disabled(is_view)
                    .accessibility_label("Show the structure form")
                    .tooltip(form_tooltip)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.panel = Panel::Form;
                        cx.notify();
                    })),
            )
            .child(
                Button::new("structure-panel-sql")
                    .ghost()
                    .xsmall()
                    .label("SQL")
                    .selected(self.panel == Panel::Sql)
                    .accessibility_label("Show the definition as SQL")
                    .tooltip("Show the table's definition as SQL")
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.panel = Panel::Sql;
                        cx.notify();
                    })),
            )
    }

    /// The note the SQL panel carries when the form has edits the
    /// definition does not reflect.
    pub(super) fn sql_panel_note(&self, cx: &App) -> Option<&'static str> {
        if self.object.kind == ObjectKind::Table && self.has_changes(cx) {
            Some("Shows the current database definition, not your pending changes.")
        } else {
            None
        }
    }

    /// The SQL panel: the definition, read-only, with Copy, Open in query
    /// tab, and Save as .sql. A failed read is an error line in the panel,
    /// not a wiped form.
    pub(super) fn render_sql_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_2()
            .size_full()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("copy-definition")
                            .ghost()
                            .xsmall()
                            .label("Copy")
                            .tooltip("Copy the definition to the clipboard")
                            .disabled(self.definition_error.is_some() || self.loading)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.copy_definition(cx);
                            })),
                    )
                    .child(
                        Button::new("open-definition-in-query-tab")
                            .ghost()
                            .xsmall()
                            .label("Open in query tab")
                            .tooltip("Open the definition in a query tab to edit and run it")
                            .disabled(self.definition_error.is_some() || self.loading)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.open_definition_in_query_tab(cx);
                            })),
                    )
                    .child(
                        Button::new("save-definition")
                            .ghost()
                            .xsmall()
                            .label("Save as .sql…")
                            .tooltip("Save the definition to a .sql file")
                            .disabled(self.definition_error.is_some() || self.loading)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.save_definition(cx);
                            })),
                    ),
            )
            .when_some(self.sql_panel_note(cx), |this, note| {
                this.child(div().text_xs().text_color(cx.theme().warning).child(note))
            })
            .when_some(self.definition_error.clone(), |this, error| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("Error: {error}")),
                )
            })
            .child(
                Textarea::new(&self.definition)
                    .flex_1()
                    .min_h_0()
                    .readonly(true)
                    .font_family(crate::settings::editor_font(cx)),
            )
            .into_any_element()
    }

    pub(super) fn copy_definition(&mut self, cx: &mut Context<Self>) {
        let text = self.definition.read(cx).value().to_string();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.notice = Some(format!("Copied definition of {}", self.object.label()));
        cx.notify();
    }

    fn open_definition_in_query_tab(&mut self, cx: &mut Context<Self>) {
        let text = self.definition.read(cx).value().to_string();
        cx.emit(SchemaViewEvent::OpenInQueryTab { text });
    }

    fn save_definition(&mut self, cx: &mut Context<Self>) {
        let sql = self.definition.read(cx).value().to_string();
        let title = self.object.label();
        let prompt = sql_file::prompt_for_save(None, &title, "sql", cx);
        cx.spawn(async move |this, cx| match prompt.await {
            Ok(Some(path)) => {
                let task = cx.background_spawn(sql_file::write(path.clone(), sql));
                let written = task.await;
                this.update(cx, |this, cx| match written {
                    Ok(()) => {
                        this.notice = Some(format!("Saved {}", path.display()));
                        cx.notify();
                    }
                    Err(error) => {
                        this.error = Some(format!("{error:#}"));
                        cx.notify();
                    }
                })
                .ok();
            }
            Ok(None) => {}
            Err(error) => {
                this.update(cx, |this, cx| {
                    this.error = Some(format!("{error:#}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// The read-only Constraints section: checks, then unique/exclusion
    /// constraints where the engine reports them. Changes go through the
    /// SQL panel or a query tab, not here.
    pub(super) fn render_constraints(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<String> = Vec::new();
        for check in &self.extras.checks {
            if check.name.is_empty() {
                rows.push(check.expression.clone());
            } else {
                rows.push(format!("{}: {}", check.name, check.expression));
            }
        }
        for constraint in &self.extras.constraints {
            rows.push(format!("{}: {}", constraint.name, constraint.definition));
        }
        v_flex()
            .gap_1()
            .child(self.render_section_heading("Constraints", cx))
            .when_some(self.extras_error.clone(), |this, error| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("Error: {error}")),
                )
            })
            .when(rows.is_empty() && self.extras_error.is_none(), |this| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("No check, unique, or exclusion constraints."),
                )
            })
            .children(rows.into_iter().map(|row| {
                div()
                    .px_1()
                    .text_xs()
                    .font_family(crate::settings::editor_font(cx))
                    .child(row)
            }))
            .child(
                div()
                    .px_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Read-only: change constraints through the SQL panel or a query tab."),
            )
            .into_any_element()
    }

    /// The read-only Triggers section: name, timing, and event per trigger,
    /// with the trigger body below it.
    pub(super) fn render_triggers(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_1()
            .child(self.render_section_heading("Triggers", cx))
            .when_some(self.extras.trigger_error.clone(), |this, error| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("Error: {error}")),
                )
            })
            .when_some(self.extras_error.clone(), |this, error| {
                this.child(
                    div()
                        .px_1()
                        .text_xs()
                        .text_color(cx.theme().danger)
                        .child(format!("Error: {error}")),
                )
            })
            .when(
                self.extras.triggers.is_empty()
                    && self.extras.trigger_error.is_none()
                    && self.extras_error.is_none(),
                |this| {
                    this.child(
                        div()
                            .px_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("No triggers."),
                    )
                },
            )
            .children(self.extras.triggers.iter().map(|trigger| {
                let summary = match (
                    trigger.timing.is_empty(),
                    trigger.events.is_empty(),
                    trigger.orientation.is_empty(),
                ) {
                    (true, true, true) => trigger.name.clone(),
                    _ => {
                        let mut parts = vec![trigger.name.clone()];
                        if !trigger.timing.is_empty() {
                            parts.push(trigger.timing.clone());
                        }
                        if !trigger.events.is_empty() {
                            parts.push(trigger.events.clone());
                        }
                        if !trigger.orientation.is_empty() {
                            parts.push(format!("FOR EACH {}", trigger.orientation));
                        }
                        parts.join(" ")
                    }
                };
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .px_1()
                            .text_xs()
                            .font_family(crate::settings::editor_font(cx))
                            .child(summary),
                    )
                    .child(
                        div()
                            .px_1()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .font_family(crate::settings::editor_font(cx))
                            .child(trigger.definition.clone()),
                    )
            }))
            .child(
                div()
                    .px_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Read-only: change triggers through the SQL panel or a query tab."),
            )
            .into_any_element()
    }
}
