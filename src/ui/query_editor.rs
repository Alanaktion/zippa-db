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

use gpui_kit::assets::IconName as AssetIcon;

use crate::db::statement;
use crate::settings;
use crate::ui::session::{OpenFile, SaveFile};

actions!(zippa_db, [RunQuery, RunScript, Explain, ExplainAnalyze]);

pub enum QueryEditorEvent {
    /// The user asked to run one statement: what is selected, or the one the
    /// caret is in.
    Run(String),
    /// The user asked to run the whole buffer, statement by statement.
    RunScript(String),
    /// The user asked for the statement's plan. `analyze` asks the server to
    /// run it so the plan carries actual times.
    Explain { sql: String, analyze: bool },
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
    #[cfg(test)]
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        let handle = self.focus_handle(cx);
        handle.focus(window, cx);
    }

    /// The editor's own focus handle, so its owner can hand it to a dock
    /// panel without going through a window.
    pub fn focus_handle(&self, cx: &gpui_kit::App) -> gpui_kit::FocusHandle {
        use gpui_kit::Focusable as _;
        self.state.read(cx).focus_handle(cx)
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

    /// Whether the analyze button would be disabled, so a test can check the
    /// gate without reading button state out of the window.
    #[cfg(test)]
    pub(crate) fn analyze_disabled_for_test(&self, cx: &gpui_kit::App) -> bool {
        self.running || self.statement_writes(cx)
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

    fn explain(&mut self, _: &Explain, _window: &mut Window, cx: &mut Context<Self>) {
        self.emit_explain(false, cx);
    }

    fn explain_analyze(
        &mut self,
        _: &ExplainAnalyze,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.emit_explain(true, cx);
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

    /// Ask for the current statement's plan.
    ///
    /// Also how the session reaches this from outside the editor's own
    /// shortcut — the quick switcher's Explain items.
    pub(crate) fn emit_explain(&mut self, analyze: bool, cx: &mut Context<Self>) {
        if self.running {
            return;
        }

        let Some(sql) = self.statement(cx) else {
            return;
        };
        cx.emit(QueryEditorEvent::Explain { sql, analyze });
    }

    /// Whether the statement a run would send is one that changes data.
    ///
    /// The plan of a write can be read, but asking the server to run it for
    /// actual times cannot, so the analyze button is disabled and says why.
    pub(crate) fn statement_writes(&self, cx: &gpui_kit::App) -> bool {
        let Some(sql) = self.statement(cx) else {
            return false;
        };
        // A statement with its own `EXPLAIN` header is judged by what it
        // explains, so `EXPLAIN ANALYZE SELECT` still reads as a read.
        let inner = statement::explained(&sql)
            .map(|(inner, _)| inner)
            .unwrap_or(sql);
        statement::first_write(&inner).is_some()
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
        let writes = self.statement_writes(cx);

        v_flex()
            .key_context("QueryEditor")
            .on_action(cx.listener(Self::run))
            .on_action(cx.listener(Self::run_script))
            .on_action(cx.listener(Self::explain))
            .on_action(cx.listener(Self::explain_analyze))
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
                                    .accessibility_label("Open a SQL file")
                                    .tooltip_with_action(
                                        "Open a SQL file",
                                        &OpenFile,
                                        Some("Session"),
                                    )
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        cx.emit(QueryEditorEvent::Open)
                                    })),
                            )
                            .child(
                                Button::new("save-file")
                                    .ghost()
                                    .small()
                                    .icon(IconName::HardDrive)
                                    .accessibility_label("Save to a SQL file")
                                    .tooltip_with_action(
                                        "Save to a SQL file",
                                        &SaveFile,
                                        Some("Session"),
                                    )
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        cx.emit(QueryEditorEvent::Save)
                                    })),
                            )
                            .child(
                                Button::new("explain")
                                    .ghost()
                                    .small()
                                    .icon(AssetIcon::Route)
                                    .accessibility_label("Explain the statement")
                                    .tooltip_with_action(
                                        "Show the statement's plan without running it",
                                        &Explain,
                                        Some("QueryEditor"),
                                    )
                                    .disabled(self.running)
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.emit_explain(false, cx)
                                    })),
                            )
                            .child(
                                Button::new("explain-analyze")
                                    .ghost()
                                    .small()
                                    .icon(AssetIcon::Gauge)
                                    .accessibility_label("Explain the statement and run it")
                                    .tooltip_with_action(
                                        if writes {
                                            "Analyze runs the query; this statement changes data"
                                        } else {
                                            "Explain the statement and run it for actual times"
                                        },
                                        &ExplainAnalyze,
                                        Some("QueryEditor"),
                                    )
                                    .disabled(self.running || writes)
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.emit_explain(true, cx)
                                    })),
                            )
                            .child(
                                Button::new("run-script")
                                    .ghost()
                                    .small()
                                    .icon(IconName::SquareTerminal)
                                    .accessibility_label("Run every statement in the buffer")
                                    .tooltip_with_action(
                                        "Run every statement in the buffer",
                                        &RunScript,
                                        Some("QueryEditor"),
                                    )
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
                                    .tooltip_with_action(
                                        "Run the selection, or the statement the caret is in",
                                        &RunQuery,
                                        Some("QueryEditor"),
                                    )
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
