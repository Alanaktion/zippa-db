//! Showing in the buffer what a run is about: the statement `Cmd+Enter` would
//! send, framed while the buffer holds more than one, and where the last run
//! failed — underlined, with the server's message on hover, and a *Go to
//! error* that puts the caret there.
//!
//! The editor remembers the buffer ranges each run sent, with a hash of the
//! buffer they were taken from; a failure is only marked against the same
//! buffer, since an edit made while the run was out has moved the text. The
//! mark is the editor's own diagnostic, so the next edit clears it, and the
//! next run clears it too.

use std::ops::Range;

use gpui_kit::base::input::{
    Diagnostic, DiagnosticSeverity, RangeDecoration, RangeDecorationStyle, RopeExt as _,
};
use gpui_kit::component::ActiveTheme;
use gpui_kit::{Context, Window};

use crate::db::statement;

use super::{GoToError, QueryEditor};

/// What a run sent: the buffer ranges of its statements, in order, and the
/// buffer they were taken from.
pub(super) struct Sent {
    key: u64,
    ranges: Vec<Range<usize>>,
}

impl QueryEditor {
    /// A hash of the buffer's text alone.
    fn buffer_key(&self, cx: &gpui_kit::App) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for chunk in self.state.read(cx).text().chunks() {
            chunk.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Note what a run is about to send, clearing the last run's error.
    pub(super) fn note_sent(&mut self, ranges: Vec<Range<usize>>, cx: &mut Context<Self>) {
        self.clear_error(cx);
        self.sent = Some(Sent {
            key: self.buffer_key(cx),
            ranges,
        });
    }

    /// The buffer range a single run sends: the selection, or the statement
    /// the caret is in.
    pub(super) fn single_range(&self, cx: &gpui_kit::App) -> Option<Range<usize>> {
        let state = self.state.read(cx);
        let selected = state.selected_range();
        if !state.selected_value().trim().is_empty() {
            return Some(selected);
        }
        let sql = state.value().to_string();
        statement::at_cursor(&sql, state.cursor(), self.engine)
            .map(|statement| statement.start..statement.end)
    }

    /// Every statement's buffer range, as a script run sends them.
    pub(super) fn script_ranges(&self, cx: &gpui_kit::App) -> Vec<Range<usize>> {
        let sql = self.state.read(cx).value().to_string();
        statement::split(&sql, self.engine)
            .into_iter()
            .map(|statement| statement.start..statement.end)
            .collect()
    }

    /// Mark where statement `index` of the last run failed: at `offset`
    /// (bytes into that statement) when the server said where, otherwise the
    /// whole statement. Nothing is marked if the buffer changed since the run
    /// went out.
    pub(crate) fn mark_error(
        &mut self,
        index: usize,
        offset: Option<usize>,
        message: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(sent) = self.sent.as_ref() else {
            return;
        };
        if sent.key != self.buffer_key(cx) {
            return;
        }
        let Some(range) = sent.ranges.get(index).cloned() else {
            return;
        };
        let at = offset.map(|offset| (range.start + offset).min(range.end));
        let message = format!("Error: {message}");

        let marked = self.state.update(cx, |state, cx| {
            let text = state.text().clone();
            let sql = state.value().to_string();
            // The word the error is at, or one character; a spot at the very
            // end (a missing closing parenthesis) marks the last character.
            let span = match at {
                Some(at) => {
                    let at = if at >= range.end {
                        sql[..range.end]
                            .char_indices()
                            .next_back()
                            .map(|(at, _)| at)
                            .unwrap_or(range.start)
                    } else {
                        at
                    };
                    text.word_range(at)
                        .filter(|word| !word.is_empty())
                        .unwrap_or_else(|| {
                            let width = sql[at..].chars().next().map(char::len_utf8).unwrap_or(0);
                            at..at + width
                        })
                }
                None => range.clone(),
            };
            let start = text.offset_to_position(span.start);
            let end = text.offset_to_position(span.end.max(span.start));
            let Some(diagnostics) = state.diagnostics_mut() else {
                return false;
            };
            diagnostics.clear();
            diagnostics.push(
                Diagnostic::new(start..end, message).with_severity(DiagnosticSeverity::Error),
            );
            cx.notify();
            true
        });
        if marked {
            self.error_at = Some(at.unwrap_or(range.start));
            cx.notify();
        }
    }

    /// Take the error mark away.
    pub(super) fn clear_error(&mut self, cx: &mut Context<Self>) {
        self.error_at = None;
        self.state.update(cx, |state, cx| {
            if let Some(diagnostics) = state.diagnostics_mut()
                && !diagnostics.is_empty()
            {
                diagnostics.clear();
                cx.notify();
            }
        });
    }

    /// Where the marked error is, while the mark is still there: an edit
    /// clears the editor's diagnostics, and with them the error.
    pub(crate) fn error_offset(&self, cx: &gpui_kit::App) -> Option<usize> {
        let marked = self
            .state
            .read(cx)
            .diagnostics()
            .is_some_and(|diagnostics| !diagnostics.is_empty());
        self.error_at.filter(|_| marked)
    }

    /// The one-based line the marked error is on.
    pub(super) fn error_line(&self, cx: &gpui_kit::App) -> Option<usize> {
        let at = self.error_offset(cx)?;
        Some(self.state.read(cx).text().offset_to_position(at).line as usize + 1)
    }

    pub(super) fn go_to_error(
        &mut self,
        _: &GoToError,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_to_error(window, cx);
    }

    /// Put the caret where the marked error is.
    pub(crate) fn move_to_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.error_offset(cx) else {
            return;
        };
        self.state.update(cx, |state, cx| {
            let position = state.text().offset_to_position(at);
            state.set_cursor_position(position, window, cx);
        });
    }

    /// Frame the statement a run would send, when there is more than one to
    /// choose from and nothing is selected. A frame, not a tint, so it is
    /// told by its shape and not its colour alone.
    pub(super) fn sync_statement_mark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.current_statement_range(cx);
        if wanted == self.statement_mark_range {
            return;
        }
        self.statement_mark_range = wanted.clone();
        let color = cx.theme().muted_foreground.opacity(0.45);
        // Changing the decorations notifies the editor, which during a draw
        // would only show on the frame after next; deferred, it shows on the
        // next.
        cx.defer_in(window, move |this, _, cx| {
            let decorations = wanted
                .into_iter()
                .map(|range| {
                    RangeDecoration::new(range)
                        .with_style(RangeDecorationStyle::Frame)
                        .with_color(color)
                })
                .collect::<Vec<_>>();
            match &this.statement_mark {
                Some(collection) => collection.set(decorations, cx),
                None => {
                    let collection = this.state.update(cx, |state, cx| {
                        state.create_range_decorations_collection(decorations, cx)
                    });
                    this.statement_mark = Some(collection);
                }
            }
        });
    }

    /// The range [`Self::sync_statement_mark`] frames, if any.
    pub(crate) fn current_statement_range(&self, cx: &gpui_kit::App) -> Option<Range<usize>> {
        if self.buffer_facts(cx).statements < 2 {
            return None;
        }
        let state = self.state.read(cx);
        if !state.selected_range().is_empty() {
            return None;
        }
        self.cached_statement(cx).2
    }
}
