//! The status line under the grid: paging, the row limit, the staged-change
//! buttons, and the confirmation a careful connection asks for.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::NumberInput;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, div, px};

use crate::db::export::Format;

use super::{ApplyEdits, DiscardEdits, InsertRow, TableView, ToggleRowPanel, change_summary};

impl TableView {
    /// The changes waiting for an answer, above the footer.
    ///
    /// The statements themselves are not shown: the rows on screen already
    /// say what will happen to them, in the colours they are drawn in.
    pub(super) fn render_confirm(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let question = self
            .confirming
            .as_ref()
            .map(|write| write.question.clone())
            .unwrap_or_default();

        h_flex()
            .p_2()
            .gap_2()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().secondary)
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(format!("Apply {question}?")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("cancel-write")
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .tooltip("Leave the changes as they are")
                            .on_click(cx.listener(|this, _, _window, cx| this.cancel_write(cx))),
                    )
                    .child(
                        Button::new("confirm-write")
                            .primary()
                            .xsmall()
                            .label("Apply")
                            .tooltip("Write the changes to the server")
                            .on_click(cx.listener(|this, _, _window, cx| this.confirm_write(cx))),
                    ),
            )
    }

    pub(super) fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let first_row = self.page * self.limit;
        let range = if self.loaded_rows == 0 {
            "No rows".to_string()
        } else {
            format!("Rows {}–{}", first_row + 1, first_row + self.loaded_rows)
        };

        let (edited, inserted, deleted) = self.grid.read(cx).pending_counts(cx);
        let staged = edited + inserted + deleted;
        let changed = match staged {
            0 => None,
            _ => Some(change_summary(edited, inserted, deleted)),
        };

        // An error says so in words: the colour it is drawn in is the only
        // other thing telling it apart from the row count beside it.
        let message = match (&self.error, self.loading, self.committing) {
            (Some(error), _, _) => (format!("Error: {error}"), cx.theme().danger),
            (None, _, true) => ("Writing…".to_string(), cx.theme().muted_foreground),
            (None, true, _) => ("Loading…".to_string(), cx.theme().muted_foreground),
            // The question comes first: it is the one thing here waiting on
            // an answer.
            (None, false, _) => match (&self.pending, &changed, &self.notice) {
                (Some(action), _, _) => (
                    format!(
                        "{} discards {}",
                        action.label(),
                        change_summary(edited, inserted, deleted)
                    ),
                    cx.theme().danger,
                ),
                (None, Some(changed), _) => (changed.clone(), cx.theme().warning),
                (None, None, Some(notice)) => (notice.clone(), cx.theme().muted_foreground),
                (None, None, None) => (range, cx.theme().muted_foreground),
            },
        };

        h_flex()
            .flex_none()
            .px_3()
            .py_1()
            .gap_3()
            .justify_between()
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("previous-page")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronLeft)
                            .accessibility_label("Previous page")
                            .tooltip("Previous page")
                            .disabled(!self.has_previous() || self.loading)
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.go(this.page.saturating_sub(1), cx)
                            })),
                    )
                    .child(
                        Button::new("next-page")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronRight)
                            .accessibility_label("Next page")
                            .tooltip("Next page")
                            .disabled(!self.has_next() || self.loading)
                            .on_click(
                                cx.listener(|this, _, _window, cx| this.go(this.page + 1, cx)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Limit"),
                    )
                    .child(
                        div()
                            .w(px(88.))
                            .child(NumberInput::new(&self.limit_input).xsmall()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(message.1)
                            .child(message.0.clone()),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .when_some(self.read_only_reason(), |this, reason| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Read-only: {reason}")),
                        )
                    })
                    .when_some(self.pending.clone(), |this, _| {
                        this.child(
                            Button::new("keep-edits")
                                .ghost()
                                .xsmall()
                                .label("Keep editing")
                                .tooltip("Leave the page as it is")
                                .on_click(cx.listener(|this, _, _window, cx| this.keep_edits(cx))),
                        )
                        .child(
                            Button::new("discard-and-continue")
                                .danger()
                                .xsmall()
                                .label("Discard")
                                .tooltip("Throw the edits away and carry on")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.discard_and_continue(cx)
                                })),
                        )
                    })
                    .when(
                        self.is_editable() && self.pending.is_none() && self.confirming.is_none(),
                        |this| {
                            this.child(
                                Button::new("insert-row")
                                    .ghost()
                                    .xsmall()
                                    .label("New row")
                                    .tooltip_with_action(
                                        "Add a row to fill in",
                                        &InsertRow,
                                        Some("TableView > DataTable"),
                                    )
                                    .disabled(self.committing)
                                    .on_click(
                                        cx.listener(|this, _, _window, cx| this.insert_row(cx)),
                                    ),
                            )
                        },
                    )
                    // Hidden while a write is waiting to be confirmed: the
                    // confirm banner above already asks about the same
                    // changes, so showing both looks like clicking Apply did
                    // nothing.
                    .when(
                        staged > 0 && self.pending.is_none() && self.confirming.is_none(),
                        |this| {
                            this.child(
                                Button::new("discard-edits")
                                    .ghost()
                                    .xsmall()
                                    .label("Discard")
                                    .tooltip_with_action(
                                        "Throw away the staged edits",
                                        &DiscardEdits,
                                        Some("TableView"),
                                    )
                                    .disabled(self.committing)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.on_discard_edits(&DiscardEdits, window, cx)
                                    })),
                            )
                            .child(
                                Button::new("apply-edits")
                                    .primary()
                                    .xsmall()
                                    .label("Apply")
                                    .tooltip_with_action(
                                        "Write the staged edits",
                                        &ApplyEdits,
                                        Some("TableView"),
                                    )
                                    .disabled(self.committing)
                                    .on_click(cx.listener(|this, _, _window, cx| this.commit(cx))),
                            )
                        },
                    )
                    .child({
                        let view = cx.entity().downgrade();
                        Button::new("export-table")
                            .ghost()
                            .xsmall()
                            .label("Export")
                            .dropdown_caret(true)
                            .tooltip("Write the whole table to a file")
                            .disabled(self.loading || self.committing)
                            .dropdown_menu(move |mut menu, _window, _cx| {
                                for format in Format::FILE {
                                    let view = view.clone();
                                    menu =
                                        menu.item(
                                            PopupMenuItem::new(format!(
                                                "Export as {}…",
                                                format.label()
                                            ))
                                            .on_click(move |_, _window, cx| {
                                                if let Some(view) = view.upgrade() {
                                                    view.update(cx, |view, cx| {
                                                        view.export(format, cx)
                                                    });
                                                }
                                            }),
                                        );
                                }
                                menu
                            })
                    })
                    .child(
                        Button::new("view-structure")
                            .ghost()
                            .xsmall()
                            .label("View structure")
                            .on_click(cx.listener(|this, _, _window, cx| this.view_structure(cx))),
                    )
                    .child(
                        Button::new("toggle-row-panel")
                            .ghost()
                            .xsmall()
                            .icon(IconName::PanelRightOpen)
                            .accessibility_label("Toggle row detail panel")
                            .tooltip_with_action(
                                "Show the focused row as fields",
                                &ToggleRowPanel,
                                Some("TableView"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_toggle_row_panel(&ToggleRowPanel, window, cx)
                            })),
                    ),
            )
    }
}
