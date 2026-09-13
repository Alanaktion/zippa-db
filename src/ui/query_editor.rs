//! SQL query editor pane.
//!
//! TODO.md section 3. Today: a code editor with SQL highlighting that hands the
//! statement text to the session. Completion, multi-tab, and cancellation come
//! later.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, EventEmitter, Window, actions, div};

use crate::db::statement;
use crate::settings;

actions!(zippa_db, [RunQuery, RunScript]);

pub enum QueryEditorEvent {
    /// The user asked to run one statement: what is selected, or the one the
    /// caret is in.
    Run(String),
    /// The user asked to run the whole buffer, statement by statement.
    RunScript(String),
    /// The user asked to read a SQL file into a tab.
    Open,
    /// The user asked to write this buffer to its file.
    Save,
}

pub struct QueryEditor {
    state: Entity<EditorState>,
    running: bool,
}

impl EventEmitter<QueryEditorEvent> for QueryEditor {}

impl QueryEditor {
    /// Open an editor that already holds `sql`, as when a table is opened from
    /// the sidebar.
    pub fn with_text(sql: impl Into<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let sql = sql.into();
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .placeholder("SELECT * FROM …")
                .default_value(sql)
        });

        Self {
            state,
            running: false,
        }
    }

    /// Put the caret in the editor, as clicking it does.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::Focusable as _;
        let handle = self.state.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }

    /// Replace the buffer contents.
    #[cfg(test)]
    pub fn set_sql(&self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_value(sql.to_string(), window, cx));
    }

    /// Put the caret at a byte offset, the way clicking in the buffer does.
    #[cfg(test)]
    pub(crate) fn set_cursor_for_test(&self, offset: usize, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_selected_range(offset..offset, cx));
    }

    /// Select a range, the way dragging across the buffer does.
    #[cfg(test)]
    pub(crate) fn select_for_test(&self, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_selected_range(range, cx));
    }

    /// The statement a run would send, for a test to read.
    #[cfg(test)]
    pub(crate) fn statement_for_test(&self, cx: &gpui_kit::App) -> Option<String> {
        self.statement(cx)
    }

    /// The statement currently in the buffer.
    pub fn sql(&self, cx: &gpui_kit::App) -> String {
        self.state.read(cx).value().to_string()
    }

    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        self.running = running;
        cx.notify();
    }

    fn run(&mut self, _: &RunQuery, _window: &mut Window, cx: &mut Context<Self>) {
        self.emit_run(cx);
    }

    fn run_script(&mut self, _: &RunScript, _window: &mut Window, cx: &mut Context<Self>) {
        self.emit_run_script(cx);
    }

    /// Run what is selected, or the statement the caret is in.
    fn emit_run(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }

        let Some(sql) = self.statement(cx) else {
            return;
        };
        cx.emit(QueryEditorEvent::Run(sql));
    }

    /// Run the whole buffer, statement by statement.
    fn emit_run_script(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }

        let sql = self.state.read(cx).value().to_string();
        if sql.trim().is_empty() {
            return;
        }

        cx.emit(QueryEditorEvent::RunScript(sql));
    }

    /// The one statement a run should send.
    ///
    /// A selection is taken as written — it may be a fragment, or several
    /// statements, and running it verbatim is what selecting it asked for.
    /// Otherwise the statement the caret is in is the one that runs.
    fn statement(&self, cx: &gpui_kit::App) -> Option<String> {
        let state = self.state.read(cx);

        let selected = state.selected_value().to_string();
        if !selected.trim().is_empty() {
            return Some(selected);
        }

        let sql = state.value().to_string();
        statement::at_cursor(&sql, state.cursor()).map(|statement| statement.text)
    }
}

impl Render for QueryEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .key_context("QueryEditor")
            .on_action(cx.listener(Self::run))
            .on_action(cx.listener(Self::run_script))
            .size_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("QUERY"),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new("open-file")
                                    .ghost()
                                    .small()
                                    .icon(IconName::FolderOpen)
                                    .tooltip("Open a SQL file")
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        cx.emit(QueryEditorEvent::Open)
                                    })),
                            )
                            .child(
                                Button::new("save-file")
                                    .ghost()
                                    .small()
                                    .icon(IconName::HardDrive)
                                    .tooltip("Save to a SQL file")
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        cx.emit(QueryEditorEvent::Save)
                                    })),
                            )
                            .child(
                                Button::new("run-script")
                                    .ghost()
                                    .small()
                                    .icon(IconName::SquareTerminal)
                                    .tooltip("Run every statement in the buffer")
                                    .disabled(self.running)
                                    .on_click(
                                        cx.listener(|this, _, _window, cx| {
                                            this.emit_run_script(cx)
                                        }),
                                    ),
                            )
                            .child(
                                Button::new("run")
                                    .primary()
                                    .small()
                                    .icon(IconName::Play)
                                    .label(if self.running { "Running…" } else { "Run" })
                                    .tooltip("Run the selection, or the statement the caret is in")
                                    .disabled(self.running)
                                    .on_click(
                                        cx.listener(|this, _, _window, cx| this.emit_run(cx)),
                                    ),
                            ),
                    ),
            )
            .child(
                div().flex_1().px_1().pb_1().child(
                    Editor::new(&self.state)
                        .h_full()
                        .appearance(false)
                        // Refines over the editor's own monospace default.
                        .font_family(settings::editor_font(cx)),
                ),
            )
    }
}
