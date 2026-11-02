//! The status line under the grid: paging, the row limit, the staged-change
//! buttons, and the confirmation a careful connection asks for. What the view
//! shows — its structure, an export, the row panel — is on the bar above the
//! grid instead (`toolbar.rs`), so this line carries only state and commits.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::NumberInput;
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, div, px};

use crate::ui::busy::busy_label;

use crate::db::result_label::grouped;

use super::count::RowCount;
use super::{
    ApplyEdits, DiscardEdits, FirstPage, InsertRow, LastPage, NextPage, Pending, PreviousPage,
    TableView, change_summary,
};

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
        // Kept beside whatever the status says — staged changes, a notice —
        // so where the page is in the table is never lost. Left out while
        // the first page is still on its way, when there is no position yet.
        let position = (self.error.is_none() && !(self.loading && self.loaded_rows == 0))
            .then(|| position_label(self.page, self.limit, self.loaded_rows, self.total));

        let (edited, inserted, deleted) = self.grid.read(cx).pending_counts(cx);
        let staged = edited + inserted + deleted;
        let changed = match staged {
            0 => None,
            _ => Some(change_summary(edited, inserted, deleted)),
        };

        // An error says so in words: the colour it is drawn in is the only
        // other thing telling it apart from the row count beside it.
        let busy = self.error.is_none() && (self.loading || self.committing || self.is_exporting());
        let message = match (&self.error, self.loading, self.committing) {
            (Some(error), _, _) => (format!("Error: {error}"), cx.theme().danger),
            (None, _, true) => ("Writing…".to_string(), cx.theme().muted_foreground),
            (None, true, _) => ("Loading…".to_string(), cx.theme().muted_foreground),
            (None, false, false) if let Some(exporting) = &self.exporting => {
                (exporting_label(exporting.rows), cx.theme().muted_foreground)
            }
            // The question comes first: it is the one thing here waiting on
            // an answer.
            (None, false, _) => match (&self.pending, &changed, &self.notice) {
                (Some(Pending::Discard), _, _) => (
                    format!("Discard {}?", change_summary(edited, inserted, deleted)),
                    cx.theme().danger,
                ),
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
                (None, None, None) => (String::new(), cx.theme().muted_foreground),
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
                // The status side gives way when the line is short of room:
                // its text is cut rather than the commands on the right
                // being pushed out of the window.
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .gap_2()
                    .child(
                        Button::new("first-page")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::assets::IconName::ChevronFirst)
                            .accessibility_label("First page")
                            .tooltip_with_action("First page", &FirstPage, Some("TableView"))
                            .disabled(!self.has_previous() || self.loading)
                            .on_click(cx.listener(|this, _, _window, cx| this.go(0, cx))),
                    )
                    .child(
                        Button::new("previous-page")
                            .ghost()
                            .xsmall()
                            .icon(IconName::ChevronLeft)
                            .accessibility_label("Previous page")
                            .tooltip_with_action("Previous page", &PreviousPage, Some("TableView"))
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
                            .tooltip_with_action("Next page", &NextPage, Some("TableView"))
                            .disabled(!self.has_next() || self.loading)
                            .on_click(
                                cx.listener(|this, _, _window, cx| this.go(this.page + 1, cx)),
                            ),
                    )
                    .child(
                        Button::new("last-page")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::assets::IconName::ChevronLast)
                            .accessibility_label("Last page")
                            .tooltip_with_action(
                                "Last page (once the rows are counted)",
                                &LastPage,
                                Some("TableView"),
                            )
                            .disabled(
                                !self.has_next() || self.loading || self.last_page().is_none(),
                            )
                            .on_click(cx.listener(|this, _, _window, cx| {
                                if let Some(last) = this.last_page() {
                                    this.go(last, cx);
                                }
                            })),
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
                    .when_some(position, |this, position| {
                        this.child(
                            div()
                                .id("table-position")
                                .flex_shrink(1.)
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(position),
                        )
                    })
                    .when(self.total.can_count() && !self.loading, |this| {
                        this.child(
                            Button::new("count-rows")
                                .ghost()
                                .xsmall()
                                .label("Count")
                                .tooltip("Count the matching rows exactly; on a large table this can take a while")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.count_rows(true, cx)
                                })),
                        )
                    })
                    .child(if busy {
                        div()
                            .text_xs()
                            .child(busy_label(message.0.clone(), message.1))
                            .into_any_element()
                    } else {
                        div()
                            .text_xs()
                            .text_color(message.1)
                            .child(message.0.clone())
                            .into_any_element()
                    })
                    .when_some(self.error.clone(), |this, error| {
                        this.child(crate::ui::copy_error_button("copy-table-error", error))
                    }),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_2()
                    .when_some(self.read_only_reason(), |this, reason| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Read-only: {reason}")),
                        )
                    })
                    .when(self.is_exporting(), |this| {
                        this.child(
                            Button::new("cancel-export")
                                .ghost()
                                .xsmall()
                                .label("Cancel export")
                                .tooltip("Stop the export and write nothing")
                                .on_click(
                                    cx.listener(|this, _, _window, cx| this.cancel_export(cx)),
                                ),
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
                    ),
            )
    }
}

/// What the footer says while an export is being written.
fn exporting_label(rows: usize) -> String {
    match rows {
        0 => "Exporting…".to_string(),
        1 => "Exporting… 1 row written".to_string(),
        rows => format!("Exporting… {rows} rows written"),
    }
}

/// `Rows 501–1,000 of 48,210 · page 2 of 97`, with as much of it as is
/// known: an estimate reads `of ~48k · page 2 of ~97`, and no count at all
/// leaves the total out rather than guessing.
pub(super) fn position_label(page: usize, limit: usize, loaded: usize, total: RowCount) -> String {
    let limit = limit.max(1);
    if loaded == 0 && page == 0 {
        return "No rows".to_string();
    }
    let first = page * limit;
    let range = match loaded {
        0 => format!("Past row {}", grouped(first as u64)),
        _ => format!(
            "Rows {}–{}",
            grouped(first as u64 + 1),
            grouped((first + loaded) as u64)
        ),
    };
    let pages = |total: u64| total.div_ceil(limit as u64).max(1);
    let shown = page as u64 + 1;
    match total {
        RowCount::Exact(total) => format!(
            "{range} of {} · page {} of {}",
            grouped(total),
            grouped(shown),
            grouped(pages(total))
        ),
        RowCount::Estimated(total) => format!(
            "{range} of ~{} · page {} of ~{}",
            approximately(total),
            grouped(shown),
            approximately(pages(total))
        ),
        RowCount::Unknown | RowCount::Counting | RowCount::NotCounted => {
            format!("{range} · page {}", grouped(shown))
        }
    }
}

/// An estimate to two significant figures, so it does not read as exact:
/// `48k`, `1.2M`.
fn approximately(count: u64) -> String {
    let scaled = |value: u64, unit: u64, suffix: &str| {
        let whole = value as f64 / unit as f64;
        if whole < 10.0 {
            format!("{:.1}{suffix}", whole).replace(".0", "")
        } else {
            format!("{:.0}{suffix}", whole)
        }
    };
    match count {
        0..1_000 => count.to_string(),
        1_000..1_000_000 => scaled(count, 1_000, "k"),
        1_000_000..1_000_000_000 => scaled(count, 1_000_000, "M"),
        _ => scaled(count, 1_000_000_000, "B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_position_says_as_much_as_is_known() {
        assert_eq!(
            position_label(1, 500, 500, RowCount::Exact(48_210)),
            "Rows 501–1,000 of 48,210 · page 2 of 97"
        );
        assert_eq!(
            position_label(0, 500, 500, RowCount::Estimated(48_210)),
            "Rows 1–500 of ~48k · page 1 of ~97"
        );
        assert_eq!(
            position_label(2, 100, 100, RowCount::Unknown),
            "Rows 201–300 · page 3"
        );
        assert_eq!(
            position_label(0, 500, 3, RowCount::Exact(3)),
            "Rows 1–3 of 3 · page 1 of 1"
        );
        assert_eq!(position_label(0, 500, 0, RowCount::Exact(0)), "No rows");
    }

    #[test]
    fn an_estimate_reads_as_one() {
        assert_eq!(approximately(950), "950");
        assert_eq!(approximately(1_000), "1k");
        assert_eq!(approximately(1_250), "1.2k");
        assert_eq!(approximately(48_210), "48k");
        assert_eq!(approximately(1_200_000), "1.2M");
        assert_eq!(approximately(3_000_000_000), "3B");
    }
}
