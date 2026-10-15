//! The editing commands bound in the editor: formatting, comment toggling,
//! and the line operations (copy, cut, delete, move, duplicate, select) plus
//! the caret moves. Each one reads the buffer, hands it to a pure function in
//! [`super::text`], and writes back what that returns.

use gpui_kit::{ClipboardItem, Context, Window};

use super::text::{
    delete_lines, delete_range, duplicate_lines, format_sql, line_clip_text, move_lines,
    next_occurrence, previous_occurrence, select_line_range, smart_home_offset, toggle_comments,
};
use super::{
    CopyLine, CutLine, DeleteLine, DuplicateLineDown, DuplicateLineUp, FormatQuery, MoveLineDown,
    MoveLineUp, QueryEditor, SelectLine, SelectNextOccurrence, SelectPreviousOccurrence, SmartHome,
    ToggleComment,
};
use crate::db::Engine;

impl QueryEditor {
    /// Pretty-print the selection, or the whole buffer when nothing is
    /// selected. A no-op when there is nothing to format, so the buffer —
    /// and the undo stack — is left alone.
    pub(super) fn format_query(
        &mut self,
        _: &FormatQuery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialect = match self.engine {
            Engine::Postgres => sqlformat::Dialect::PostgreSql,
            Engine::MySql | Engine::Sqlite => sqlformat::Dialect::Generic,
        };
        self.edit_buffer(
            |text, selected| format_sql(text, selected, dialect),
            window,
            cx,
        );
    }

    /// Comment out the lines the selection touches, or the caret's line, and
    /// run it again to take the comments back off.
    pub(super) fn toggle_comment(
        &mut self,
        _: &ToggleComment,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_buffer(toggle_comments, window, cx);
    }

    /// Run a pure text edit over the buffer: the shared shape of the line
    /// operations below. `replace_all` keeps the change on the undo stack,
    /// unlike `set_value`; it clears the selection and jumps the view to the
    /// top, so both are put back.
    fn edit_buffer(
        &mut self,
        edit: impl FnOnce(&str, std::ops::Range<usize>) -> Option<(String, std::ops::Range<usize>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, selected, scroll) = {
            let state = self.state.read(cx);
            (
                state.value().to_string(),
                state.selected_range(),
                state.scroll_offset(),
            )
        };
        let Some((text, selected)) = edit(&text, selected) else {
            return;
        };
        self.state.update(cx, |state, cx| {
            state.replace_all(text, window, cx);
            state.set_selected_range(selected, cx);
            state.set_scroll_offset(scroll, cx);
        });
    }

    /// Copy the selection, or the caret's whole line when nothing is
    /// selected — the way ⌘C behaves in VS Code and Zed.
    pub(super) fn copy_line(&mut self, _: &CopyLine, _window: &mut Window, cx: &mut Context<Self>) {
        let text = {
            let state = self.state.read(cx);
            let selected = state.selected_range();
            if selected.is_empty() {
                line_clip_text(&state.value(), selected.start)
            } else {
                state.selected_value().to_string()
            }
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// Cut the selection, or the caret's whole line when nothing is selected.
    pub(super) fn cut_line(&mut self, _: &CutLine, window: &mut Window, cx: &mut Context<Self>) {
        self.copy_line(&CopyLine, window, cx);
        self.edit_buffer(
            |text, selected| {
                if selected.is_empty() {
                    delete_lines(text, selected)
                } else {
                    delete_range(text, selected)
                }
            },
            window,
            cx,
        );
    }

    /// Delete the lines the selection touches, or the caret's line.
    pub(super) fn delete_line(
        &mut self,
        _: &DeleteLine,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_buffer(delete_lines, window, cx);
    }

    /// Move the touched lines up one, the selection riding along.
    pub(super) fn move_line_up(
        &mut self,
        _: &MoveLineUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_buffer(
            |text, selected| move_lines(text, selected, true),
            window,
            cx,
        );
    }

    /// Move the touched lines down one, the selection riding along.
    pub(super) fn move_line_down(
        &mut self,
        _: &MoveLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_buffer(
            |text, selected| move_lines(text, selected, false),
            window,
            cx,
        );
    }

    /// Duplicate the touched lines above themselves, the selection moving
    /// onto the new copy.
    pub(super) fn duplicate_line_up(
        &mut self,
        _: &DuplicateLineUp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_buffer(
            |text, selected| duplicate_lines(text, selected, true),
            window,
            cx,
        );
    }

    /// Duplicate the touched lines below themselves, the selection moving
    /// onto the new copy.
    pub(super) fn duplicate_line_down(
        &mut self,
        _: &DuplicateLineDown,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_buffer(
            |text, selected| duplicate_lines(text, selected, false),
            window,
            cx,
        );
    }

    /// Select the word under the caret, or move the selection to the next
    /// occurrence of the selected text, wrapping around the buffer.
    pub(super) fn select_next_occurrence(
        &mut self,
        _: &SelectNextOccurrence,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, selected) = {
            let state = self.state.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        if let Some(range) = next_occurrence(&text, selected) {
            self.state
                .update(cx, |state, cx| state.set_selected_range(range, cx));
        }
    }

    /// Select the word under the caret, or move the selection to the previous
    /// occurrence of the selected text, wrapping around the buffer.
    pub(super) fn select_previous_occurrence(
        &mut self,
        _: &SelectPreviousOccurrence,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, selected) = {
            let state = self.state.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        if let Some(range) = previous_occurrence(&text, selected) {
            self.state
                .update(cx, |state, cx| state.set_selected_range(range, cx));
        }
    }

    /// Select the touched lines whole, newlines included; pressing again
    /// extends through the next line.
    pub(super) fn select_line(
        &mut self,
        _: &SelectLine,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, selected) = {
            let state = self.state.read(cx);
            (state.value().to_string(), state.selected_range())
        };
        let range = select_line_range(&text, selected);
        self.state
            .update(cx, |state, cx| state.set_selected_range(range, cx));
    }

    /// Home, or ⌘◀ on macOS: the first non-blank character first, then the
    /// line's start.
    pub(super) fn smart_home(
        &mut self,
        _: &SmartHome,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, caret) = {
            let state = self.state.read(cx);
            (state.value().to_string(), state.cursor())
        };
        let target = smart_home_offset(&text, caret);
        self.state
            .update(cx, |state, cx| state.set_selected_range(target..target, cx));
    }
}
