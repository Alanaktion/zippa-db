//! Reach-ins for the UI tests: setting the buffer and caret, and reading
//! what the editor would send or offer, without driving the window.

use gpui_kit::{Context, Window};

use super::QueryEditor;

impl QueryEditor {
    /// Put the caret in the editor, as clicking it does.
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.focus_handle(cx);
        handle.focus(window, cx);
    }

    /// Replace the buffer contents.
    pub(crate) fn set_sql(&self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_buffer(sql, window, cx);
    }

    /// Put the caret at a byte offset, the way clicking in the buffer does.
    pub(crate) fn set_cursor_for_test(&self, offset: usize, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_selected_range(offset..offset, cx));
    }

    /// Select a range, the way dragging across the buffer does.
    pub(crate) fn select_for_test(&self, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_selected_range(range, cx));
    }

    /// The labels the completion menu would offer with the caret where it is.
    pub(crate) fn completions_for_test(&self, cx: &gpui_kit::App) -> Vec<String> {
        let state = self.state.read(cx);
        self.completions
            .complete(state.text(), state.cursor())
            .map(|completions| completions.items.into_iter().map(|s| s.label).collect())
            .unwrap_or_default()
    }

    /// The statement a run would send, for a test to read.
    pub(crate) fn statement_for_test(&self, cx: &gpui_kit::App) -> Option<String> {
        self.statement(cx)
    }

    /// Whether the analyze button would be disabled, so a test can check the
    /// gate without reading button state out of the window.
    pub(crate) fn analyze_disabled_for_test(&self, cx: &gpui_kit::App) -> bool {
        self.running || self.statement_writes(cx) || !self.analyze_supported()
    }
}
