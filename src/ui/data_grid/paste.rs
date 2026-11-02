//! Typing into the grid the way a spreadsheet takes it: a printable key on a
//! selected cell opens its editor with that key in it, and `Cmd+V` stages the
//! clipboard into the cells from the selected one on — a tab-separated,
//! multi-line clip filling the rectangle it describes.
//!
//! Both only stage: what lands is marked like a typed edit and written by the
//! owner's Apply, so the connection's safety mode decides the rest. A grid the
//! owner has not made writable (a query result) ignores both.

use gpui_kit::base::input::Paste;
use gpui_kit::{Context, Focusable, KeyDownEvent, Keystroke, Window};

use crate::db::query::Cell;

use super::{DataGrid, GridEdit, Pasted};

/// What a paste did, counted for the owner's status line.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PasteOutcome {
    /// Cells staged.
    pub staged: usize,
    /// Cells that fell on a cell that cannot be typed into: a binary or
    /// placeholder value, or a row marked for deletion.
    pub read_only: usize,
    /// Cells past the last row or column, which had nowhere to go.
    pub outside: usize,
}

impl PasteOutcome {
    /// The status line's sentence, or `None` when nothing happened worth
    /// saying (an empty clipboard).
    pub fn message(&self) -> Option<String> {
        if self.staged + self.read_only + self.outside == 0 {
            return None;
        }
        let cells = |count: usize| match count {
            1 => "1 cell".to_string(),
            count => format!("{count} cells"),
        };
        let mut message = format!("Pasted {}", cells(self.staged));
        if self.read_only > 0 {
            message.push_str(&format!("; {} read-only, skipped", cells(self.read_only)));
        }
        if self.outside > 0 {
            message.push_str(&format!(
                "; {} past the last row or column, left out",
                cells(self.outside)
            ));
        }
        Some(message)
    }
}

/// The rows and fields of a clip: one row per line, fields split on tabs. A
/// trailing line break (which a spreadsheet's copy always ends with) does not
/// make an extra empty row, and `\r\n` reads as one break.
pub(crate) fn parse_block(text: &str) -> Vec<Vec<&str>> {
    let text = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n')
        .map(|line| {
            line.strip_suffix('\r')
                .unwrap_or(line)
                .split('\t')
                .collect()
        })
        .collect()
}

/// The text a key would type into a cell, if it is one that should start an
/// edit: a printable character, with no modifier but Shift. Space is left to
/// the row pick it is bound to.
pub(crate) fn typed_text(keystroke: &Keystroke) -> Option<&str> {
    let modifiers = &keystroke.modifiers;
    if modifiers.control || modifiers.platform || modifiers.function || modifiers.alt {
        return None;
    }
    let text = keystroke.key_char.as_deref()?;
    if text.is_empty() || text == " " || text.chars().any(char::is_control) {
        return None;
    }
    Some(text)
}

/// A pasted field as a staged value. An empty field is `NULL`: the grid copies
/// a `NULL` as nothing, so pasting a copied row back puts it back as it was,
/// and an empty spreadsheet cell means no value rather than an empty string.
/// Anything else is staged as written — the text `NULL` included, which is
/// what the grid copies for a text cell holding those four letters, so the
/// typed-`NULL` setting does not apply.
fn pasted_value(field: &str) -> Cell {
    (!field.is_empty()).then(|| field.to_string())
}

impl DataGrid {
    /// A key on the table that types a character opens the selected cell's
    /// editor with it, replacing the value — the spreadsheet habit.
    pub(super) fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = typed_text(&event.keystroke) else {
            return;
        };
        // Only while the rows have the keyboard: a key typed in an open
        // editor bubbles up here too, and belongs to it.
        if self.table.read(cx).delegate().editing.is_some()
            || !self.focus_handle(cx).is_focused(window)
        {
            return;
        }
        let Some((row_ix, col_ix)) = self.selected_cell(cx) else {
            return;
        };
        if !self.table.read(cx).delegate().is_editable(row_ix, col_ix) {
            return;
        }
        let text = text.to_string();
        cx.stop_propagation();
        self.begin_edit_seeded(row_ix, col_ix, Some(text), window, cx);
    }

    pub(super) fn on_paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        // An open cell editor takes its own paste; this is the rows'.
        if self.table.read(cx).delegate().editing.is_some() {
            cx.propagate();
            return;
        }
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let outcome = self.paste(&text, window, cx);
        if let Some(message) = outcome.message() {
            cx.emit(Pasted { message });
        }
    }

    /// Stage `text` into the grid from the selected cell on, row by row and
    /// field by field. Nothing happens on a grid that cannot be written to.
    pub fn paste(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PasteOutcome {
        let mut outcome = PasteOutcome::default();
        if !self.is_writable(cx) {
            return outcome;
        }
        let Some((top, left)) = self.selected_cell(cx) else {
            return outcome;
        };
        let block = parse_block(text);
        if block.is_empty() {
            return outcome;
        }

        self.cancel_editor(window, cx);
        self.table.update(cx, |table, cx| {
            let delegate = table.delegate_mut();
            let rows = delegate.rows();
            let columns = delegate.result.columns.len();
            for (dy, fields) in block.iter().enumerate() {
                for (dx, field) in fields.iter().enumerate() {
                    let (row_ix, col_ix) = (top + dy, left + dx);
                    if row_ix >= rows || col_ix >= columns {
                        outcome.outside += 1;
                    } else if !delegate.is_editable(row_ix, col_ix) {
                        outcome.read_only += 1;
                    } else {
                        delegate.stage(row_ix, col_ix, pasted_value(field));
                        outcome.staged += 1;
                    }
                }
            }
            cx.notify();
        });
        if outcome.staged > 0 {
            cx.emit(GridEdit::Staged);
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_splits_on_lines_and_tabs() {
        assert_eq!(
            parse_block("a\tb\tc\r\nd\te\tf\n"),
            [["a", "b", "c"], ["d", "e", "f"]]
        );
        assert_eq!(parse_block("one"), [["one"]]);
        assert_eq!(parse_block("x\t\ty"), [["x", "", "y"]]);
        assert!(parse_block("").is_empty());
        assert!(parse_block("\n").is_empty());
        // An empty line in the middle is a row of one empty field.
        assert_eq!(parse_block("a\n\nb").len(), 3);
    }

    #[test]
    fn only_a_plain_printable_key_starts_an_edit() {
        let key = |source: &str| {
            Keystroke::parse_with_os(source, gpui_kit::OperatingSystem::Linux).expect("a keystroke")
        };
        assert_eq!(typed_text(&key("a->a")), Some("a"));
        assert_eq!(typed_text(&key("shift-a->A")), Some("A"));
        assert_eq!(typed_text(&key("5->5")), Some("5"));
        assert_eq!(typed_text(&key("secondary-a")), None);
        assert_eq!(typed_text(&key("ctrl-a")), None);
        assert_eq!(typed_text(&key("space-> ")), None);
        assert_eq!(typed_text(&key("enter")), None);
        assert_eq!(typed_text(&key("up")), None);
    }

    #[test]
    fn the_outcome_names_what_was_left_out() {
        let outcome = PasteOutcome {
            staged: 4,
            read_only: 1,
            outside: 2,
        };
        assert_eq!(
            outcome.message().as_deref(),
            Some(
                "Pasted 4 cells; 1 cell read-only, skipped; 2 cells past the last row or column, left out"
            )
        );
        assert_eq!(PasteOutcome::default().message(), None);
    }
}
