//! SQL query editor pane.
//!
//! A code editor with SQL highlighting that hands the statement under the
//! caret, the selection, or the whole buffer to the session, which runs it and
//! can cancel it. Typing offers completions from the session's catalog and a
//! list of SQL keywords (see [`crate::ui::completion`]).

use gpui_kit::component::button::{Button, ButtonVariants, DropdownButton};
use gpui_kit::component::input::{Editor, EditorState, Enter, IndentInline};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, EventEmitter, KeyDownEvent, SharedString, Window, actions, px};

use gpui_kit::base::input::{Copy, Cut, Paste, SelectAll};

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use crate::db::params::Variable;
use crate::db::{Blocker, Engine, statement, transaction_blocker};
use crate::settings::{self, CompletionKey, Settings};
use crate::ui::completion::{SharedCatalog, SqlCompletions};
use crate::ui::session::{OpenFile, SaveFile, SaveFileAs, SaveQuery, SaveQueryAs};

mod editing;
#[cfg(test)]
mod test_support;
mod text;

actions!(
    zippa_db,
    [
        RunQuery,
        RunScript,
        RunScriptIgnoringErrors,
        Explain,
        ExplainAnalyze,
        FormatQuery,
        ToggleComment,
        CopyLine,
        CutLine,
        DeleteLine,
        MoveLineUp,
        MoveLineDown,
        DuplicateLineUp,
        DuplicateLineDown,
        SelectNextOccurrence,
        SelectPreviousOccurrence,
        SelectLine,
        SmartHome,
        EditVariables,
    ]
);

/// The widest the completion menu grows, in pixels. `gpui-kit`'s default of
/// 320 cuts off a long column name once its type and table are beside it.
const COMPLETION_MENU_WIDTH: f32 = 480.;

pub enum QueryEditorEvent {
    /// The user asked to run one statement: what is selected, or the one the
    /// caret is in.
    Run(String),
    /// The user asked to run the whole buffer, statement by statement.
    RunScript(String),
    /// The same, with no transaction and every failure skipped.
    RunScriptIgnoringErrors(String),
    /// The user asked for the statement's plan. `analyze` asks the server to
    /// run it so the plan carries actual times.
    Explain { sql: String, analyze: bool },
    /// The user asked to read a SQL file into a tab.
    Open,
    /// The user asked to write this buffer to its file.
    Save,
    /// The same, to a file picked first.
    SaveAs,
    /// The user asked to keep this buffer in the sidebar's Saved tab,
    /// updating the saved query it belongs to if it has one.
    SaveQuery,
    /// The same, always under a new name.
    SaveQueryAs,
    /// The user asked to edit this buffer's `:name`/`$name` variables.
    EditVariables,
}

pub struct QueryEditor {
    state: Entity<EditorState>,
    /// The provider the editor asks, kept so a test can ask it too.
    #[cfg(test)]
    completions: Rc<SqlCompletions>,
    running: bool,
    /// The `:name`/`$name` variables for this buffer, edited in the Variables dialog.
    /// The whole buffer shares one mapping: every occurrence of a name, in
    /// every statement, uses the same value.
    variables: Vec<Variable>,
    /// The engine this buffer runs against, so the analyze button can say when
    /// the server has no `EXPLAIN ANALYZE`.
    engine: Engine,
    /// Memoized `statement` plus its write/read judgement. Both clone the
    /// whole buffer and re-split its statements, a hitch on every render
    /// with a multi-MB buffer; the key is a hash of the buffer, cursor, and
    /// selection, so an untouched buffer answers without touching the text.
    statement_cache: RefCell<Option<StatementCache>>,
    /// Memoized facts about the whole buffer the toolbar shows on every
    /// render, keyed by a hash of the buffer alone.
    buffer_cache: RefCell<Option<BufferCache>>,
    /// The saved query this buffer belongs to, so the menu can say which
    /// one Save will update. Kept by the tab, mirrored here.
    saved_name: Option<SharedString>,
    /// Whether the run/save/format toolbar is drawn. Off where the editor
    /// only holds text for an owner that has its own Save, such as a
    /// dashboard widget's dialog.
    toolbar: bool,
}

/// What the toolbar reads off the whole buffer.
#[derive(Clone)]
struct BufferCache {
    key: u64,
    /// The statement that would keep a script run out of a transaction.
    blocker: Option<Blocker>,
    /// How many distinct `:name`/`$name` variables the buffer uses.
    variables: usize,
}

struct StatementCache {
    key: u64,
    statement: Option<String>,
    writes: bool,
}

impl EventEmitter<QueryEditorEvent> for QueryEditor {}

impl QueryEditor {
    /// Open an editor that already holds `sql`, as when a table is opened from
    /// the sidebar. `catalog` is the session's, which completions are read
    /// from.
    pub fn with_text(
        sql: impl Into<String>,
        engine: Engine,
        catalog: SharedCatalog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let sql = sql.into();
        let completions = Rc::new(SqlCompletions { catalog, engine });
        let state = cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("sql")
                .placeholder("SELECT * FROM …")
                .default_value(sql);
            let lsp = state.lsp_mut();
            lsp.completion_provider = Some(completions.clone());
            // Wide enough for a long column name beside its type and table.
            lsp.completion_menu.max_width = px(COMPLETION_MENU_WIDTH);
            state
        });

        Self {
            state,
            #[cfg(test)]
            completions,
            running: false,
            variables: Vec::new(),
            engine,
            statement_cache: RefCell::new(None),
            buffer_cache: RefCell::new(None),
            saved_name: None,
            toolbar: true,
        }
    }

    /// Draw only the text area, without the toolbar. Shortcuts still work.
    pub(crate) fn without_toolbar(mut self) -> Self {
        self.toolbar = false;
        self
    }

    /// This buffer's `:name`/`$name` variables.
    pub(crate) fn variables(&self) -> Vec<Variable> {
        self.variables.clone()
    }

    /// Replace this buffer's `:name`/`$name` variables, as the Variables dialog does.
    pub(crate) fn set_variables(&mut self, variables: Vec<Variable>, cx: &mut Context<Self>) {
        self.variables = variables;
        cx.notify();
    }

    /// The editor's own focus handle, so its owner can hand it to a dock
    /// panel without going through a window.
    pub fn focus_handle(&self, cx: &gpui_kit::App) -> gpui_kit::FocusHandle {
        use gpui_kit::Focusable as _;
        self.state.read(cx).focus_handle(cx)
    }

    /// The statement currently in the buffer.
    pub fn sql(&self, cx: &gpui_kit::App) -> String {
        self.state.read(cx).value().to_string()
    }

    /// Replace the buffer contents, as reusing a pristine proposal does.
    pub(crate) fn set_buffer(&self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.state
            .update(cx, |state, cx| state.set_value(sql.to_string(), window, cx));
    }

    /// Whether this engine can report actual times. SQLite's planner cannot, so
    /// its analyze button is disabled rather than showing the plain plan under
    /// a button that promises timings.
    pub(crate) fn analyze_supported(&self) -> bool {
        !matches!(self.engine, Engine::Sqlite)
    }

    /// Name the saved query this buffer belongs to, for the toolbar menu.
    pub(crate) fn set_saved_name(&mut self, name: Option<SharedString>, cx: &mut Context<Self>) {
        self.saved_name = name;
        cx.notify();
    }

    pub fn set_running(&mut self, running: bool, cx: &mut Context<Self>) {
        self.running = running;
        cx.notify();
    }

    fn run(&mut self, _: &RunQuery, _window: &mut Window, cx: &mut Context<Self>) {
        self.emit_run(cx);
    }

    fn run_script(&mut self, _: &RunScript, _window: &mut Window, cx: &mut Context<Self>) {
        self.emit_run_script(false, cx);
    }

    fn run_script_ignoring_errors(
        &mut self,
        _: &RunScriptIgnoringErrors,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.emit_run_script(true, cx);
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

    /// Whether the completion menu is on screen.
    fn completion_open(&self, cx: &gpui_kit::App) -> bool {
        self.state.read(cx).completion_menu_state().open
    }

    /// Every keystroke, seen before the editor.
    fn on_key_down(&mut self, _: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.follow_caret(window, cx);
    }

    /// Draw the editor once more after the frame that moves the caret, so
    /// the completion menu follows it straight away.
    ///
    /// The menu places itself from the caret's position in the *last* frame
    /// painted, and its new items usually land before the keystroke's own
    /// frame is drawn — so that frame shows it where the caret was, and
    /// nothing redraws it until the caret next blinks. Next-frame callbacks
    /// run before that tick's draw, hence the two levels: the outer one runs
    /// before the keystroke's frame, the inner one after it is painted.
    fn follow_caret(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = cx.entity().downgrade();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |_, cx| {
                let Some(editor) = editor.upgrade() else {
                    return;
                };
                let state = editor.read(cx).state.clone();
                if state.read(cx).completion_menu_state().open {
                    state.update(cx, |_, cx| cx.notify());
                }
            });
        });
    }

    /// `Tab`, seen before the editor indents: with Tab chosen to accept a
    /// suggestion and the menu open, it takes the highlighted one instead.
    fn accept_with_tab(&mut self, _: &IndentInline, window: &mut Window, cx: &mut Context<Self>) {
        if Settings::global(cx).accept_completion != CompletionKey::Tab || !self.completion_open(cx)
        {
            return;
        }
        // The menu answers `Enter` by taking the highlighted suggestion.
        let accept = Box::new(Enter {
            secondary: false,
            shift: false,
        });
        self.state.update(cx, |state, cx| {
            state.route_overlay_action(accept, window, cx)
        });
        cx.stop_propagation();
    }

    /// `Enter`, seen before the menu: with Tab chosen to accept, Enter closes
    /// the menu and goes on to start a new line.
    fn enter_with_menu_open(&mut self, action: &Enter, _: &mut Window, cx: &mut Context<Self>) {
        if action.secondary
            || Settings::global(cx).accept_completion != CompletionKey::Tab
            || !self.completion_open(cx)
        {
            return;
        }
        self.state
            .update(cx, |state, cx| state.dismiss_lsp_overlays(cx));
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

    /// Run the whole buffer, statement by statement — in a transaction that
    /// asks about each failure, or, `ignoring_errors`, without one and past
    /// every failure.
    fn emit_run_script(&mut self, ignoring_errors: bool, cx: &mut Context<Self>) {
        if self.running {
            return;
        }

        let sql = self.state.read(cx).value().to_string();
        if sql.trim().is_empty() {
            return;
        }

        cx.emit(if ignoring_errors {
            QueryEditorEvent::RunScriptIgnoringErrors(sql)
        } else {
            QueryEditorEvent::RunScript(sql)
        });
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
        self.cached_statement(cx).1
    }

    /// The one statement a run should send.
    ///
    /// A selection is taken as written — it may be a fragment, or several
    /// statements, and running it verbatim is what selecting it asked for.
    /// Otherwise the statement the caret is in is the one that runs.
    fn statement(&self, cx: &gpui_kit::App) -> Option<String> {
        self.cached_statement(cx).0
    }

    /// The statement a run would send, plus whether it writes — memoized.
    ///
    /// Finding the statement clones the whole buffer and re-splits it, a
    /// hitch on every render with a multi-MB buffer. The key hashes the
    /// buffer's chunks (borrowed, never copied) together with the cursor
    /// and selection, so an untouched buffer answers from the cache.
    fn cached_statement(&self, cx: &gpui_kit::App) -> (Option<String>, bool) {
        let key = {
            let state = self.state.read(cx);
            let mut hasher = DefaultHasher::new();
            for chunk in state.text().chunks() {
                chunk.hash(&mut hasher);
            }
            state.cursor().hash(&mut hasher);
            state.selected_range().hash(&mut hasher);
            hasher.finish()
        };
        if let Some(cached) = self.statement_cache.borrow().as_ref()
            && cached.key == key
        {
            return (cached.statement.clone(), cached.writes);
        }
        let statement = self.compute_statement(cx);
        let writes = statement.as_ref().is_some_and(|sql| {
            // A statement with its own `EXPLAIN` header is judged by what it
            // explains, so `EXPLAIN ANALYZE SELECT` still reads as a read.
            let inner = statement::explained(sql, self.engine)
                .map(|(inner, _)| inner)
                .unwrap_or_else(|| sql.clone());
            statement::first_write(&inner, self.engine).is_some()
        });
        *self.statement_cache.borrow_mut() = Some(StatementCache {
            key,
            statement: statement.clone(),
            writes,
        });
        (statement, writes)
    }

    /// The statement that would keep a script run of the whole buffer out of
    /// a transaction, if there is one.
    pub(crate) fn transaction_blocker(&self, cx: &gpui_kit::App) -> Option<Blocker> {
        self.buffer_facts(cx).blocker
    }

    /// How many distinct `:name`/`$name` variables the buffer uses, which is
    /// what decides whether the toolbar offers the Variables button.
    pub(crate) fn variable_count(&self, cx: &gpui_kit::App) -> usize {
        self.buffer_facts(cx).variables
    }

    /// The toolbar's facts about the whole buffer — memoized on its text,
    /// since the toolbar asks on every render.
    fn buffer_facts(&self, cx: &gpui_kit::App) -> BufferCache {
        let key = {
            let mut hasher = DefaultHasher::new();
            for chunk in self.state.read(cx).text().chunks() {
                chunk.hash(&mut hasher);
            }
            hasher.finish()
        };
        if let Some(cached) = self.buffer_cache.borrow().as_ref()
            && cached.key == key
        {
            return cached.clone();
        }
        let sql = self.state.read(cx).value().to_string();
        let blocker = transaction_blocker(self.engine, &statement::split(&sql, self.engine));
        let names: std::collections::HashSet<String> = statement::placeholders(&sql, self.engine)
            .into_iter()
            .map(|placeholder| placeholder.name)
            .collect();
        let facts = BufferCache {
            key,
            blocker,
            variables: names.len(),
        };
        *self.buffer_cache.borrow_mut() = Some(facts.clone());
        facts
    }

    /// The uncached statement: the selection verbatim, else the statement
    /// the caret is in.
    fn compute_statement(&self, cx: &gpui_kit::App) -> Option<String> {
        let state = self.state.read(cx);

        let selected = state.selected_value().to_string();
        if !selected.trim().is_empty() {
            return Some(selected);
        }

        let sql = state.value().to_string();
        statement::at_cursor(&sql, state.cursor(), self.engine).map(|statement| statement.text)
    }
}

impl QueryEditor {
    fn edit_variables(&mut self, _: &EditVariables, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.running {
            cx.emit(QueryEditorEvent::EditVariables);
        }
    }

    /// The bar above the buffer: the three ways to run it on the leading
    /// edge, in the order they are reached for, and everything about the
    /// buffer itself — its file, its variables, its layout — behind one
    /// menu on the trailing edge. Every button says what it does in words;
    /// the menus show the shortcuts.
    fn render_toolbar(
        &self,
        writes: bool,
        can_analyze: bool,
        blocker: Option<Blocker>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let editor = cx.entity().downgrade();
        let running = self.running;
        let saved_name = self.saved_name.clone();
        let variables = self.variable_count(cx);

        // A buffer a transaction cannot hold says so before it is run, in
        // words and with its own icon rather than a colour.
        let script_label = match &blocker {
            Some(blocker) => format!(
                "Run every statement without a transaction: {}",
                blocker.message()
            ),
            None if Settings::global(cx).wrap_scripts_in_transaction => {
                "Run every statement in one transaction, asking about each error".to_string()
            }
            None => {
                "Run every statement without a transaction, asking about each error".to_string()
            }
        };

        h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .justify_between()
            .child(
                h_flex()
                    .gap_1()
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
                            .on_click(cx.listener(|this, _, _window, cx| this.emit_run(cx))),
                    )
                    .child(
                        DropdownButton::new("run-script-split")
                            .ghost()
                            .small()
                            .disabled(self.running)
                            .menu_accessibility_label("More ways to run all queries")
                            .button(
                                Button::new("run-script")
                                    .when(blocker.is_some(), |this| {
                                        this.icon(IconName::TriangleAlert)
                                    })
                                    .label("Run all queries")
                                    .accessibility_label(script_label.clone())
                                    .tooltip_with_action(
                                        script_label,
                                        &RunScript,
                                        Some("QueryEditor"),
                                    )
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.emit_run_script(false, cx)
                                    })),
                            )
                            .dropdown_menu({
                                let editor = editor.clone();
                                move |menu, _window, _cx| {
                                    let editor = editor.clone();
                                    menu.item(
                                        PopupMenuItem::new("Run all queries, skipping errors")
                                            .action(Box::new(RunScriptIgnoringErrors))
                                            .on_click(move |_, _window, cx| {
                                                editor
                                                    .update(cx, |editor, cx| {
                                                        editor.emit_run_script(true, cx)
                                                    })
                                                    .ok();
                                            }),
                                    )
                                }
                            }),
                    )
                    .child({
                        let explain = Button::new("explain")
                            .label("Explain")
                            .tooltip_with_action(
                                "Show the plan for the statement the caret is in",
                                &Explain,
                                Some("QueryEditor"),
                            )
                            .on_click(
                                cx.listener(|this, _, _window, cx| this.emit_explain(false, cx)),
                            );
                        // SQLite has no EXPLAIN ANALYZE, so it gets the plain
                        // button rather than a menu holding one dead item.
                        if !can_analyze {
                            explain
                                .ghost()
                                .small()
                                .disabled(self.running)
                                .into_any_element()
                        } else {
                            let editor = editor.clone();
                            DropdownButton::new("explain-split")
                                .ghost()
                                .small()
                                .disabled(self.running)
                                .menu_accessibility_label("More ways to explain")
                                .button(explain)
                                .dropdown_menu(move |menu, _window, _cx| {
                                    let editor = editor.clone();
                                    // Analyzing a write would run it.
                                    menu.item(
                                        PopupMenuItem::new("Explain analyze")
                                            .action(Box::new(ExplainAnalyze))
                                            .disabled(writes)
                                            .on_click(move |_, _window, cx| {
                                                editor
                                                    .update(cx, |editor, cx| {
                                                        editor.emit_explain(true, cx)
                                                    })
                                                    .ok();
                                            }),
                                    )
                                })
                                .into_any_element()
                        }
                    }),
            )
            .child(
                h_flex()
                    .gap_1()
                    // Only a buffer that uses variables needs a way to fill
                    // them in on sight; the menu still offers it otherwise.
                    .when(variables > 0, |this| {
                        this.child(
                            Button::new("variables")
                                .ghost()
                                .small()
                                .label(format!("Variables ({variables})"))
                                .tooltip("Fill in the :name/$name variables this buffer uses")
                                .disabled(self.running)
                                .on_click(cx.listener(|_this, _, _window, cx| {
                                    cx.emit(QueryEditorEvent::EditVariables)
                                })),
                        )
                    })
                    .child(
                        Button::new("query-actions")
                            .ghost()
                            .small()
                            .icon(IconName::Ellipsis)
                            .accessibility_label("More query actions")
                            .dropdown_menu({
                                let editor = editor.clone();
                                move |menu, _window, _cx| {
                                    // Each item names its action only for the
                                    // shortcut shown beside it; the click itself
                                    // goes to this editor, the way the buttons'
                                    // do.
                                    let item = |label: SharedString,
                                                action: Box<dyn gpui_kit::Action>,
                                                run: fn(
                                        &mut QueryEditor,
                                        &mut Window,
                                        &mut Context<QueryEditor>,
                                    )| {
                                        let editor = editor.clone();
                                        PopupMenuItem::new(label).action(action).on_click(
                                            move |_, window, cx| {
                                                editor
                                                    .update(cx, |editor, cx| {
                                                        run(editor, window, cx)
                                                    })
                                                    .ok();
                                            },
                                        )
                                    };
                                    menu.item(item(
                                        "Open File…".into(),
                                        Box::new(OpenFile),
                                        |_, _, cx| cx.emit(QueryEditorEvent::Open),
                                    ))
                                    .item(item("Save".into(), Box::new(SaveFile), |_, _, cx| {
                                        cx.emit(QueryEditorEvent::Save)
                                    }))
                                    .item(item(
                                        "Save As…".into(),
                                        Box::new(SaveFileAs),
                                        |_, _, cx| cx.emit(QueryEditorEvent::SaveAs),
                                    ))
                                    .map(|menu| match &saved_name {
                                        // A buffer that belongs to a saved
                                        // query updates it in one step; a
                                        // new name is the second choice.
                                        Some(name) => menu
                                            .item(item(
                                                format!("Update Saved Query “{name}”").into(),
                                                Box::new(SaveQuery),
                                                |_, _, cx| cx.emit(QueryEditorEvent::SaveQuery),
                                            ))
                                            .item(item(
                                                "Save as New Saved Query…".into(),
                                                Box::new(SaveQueryAs),
                                                |_, _, cx| cx.emit(QueryEditorEvent::SaveQueryAs),
                                            )),
                                        None => menu.item(item(
                                            "Save to Saved Queries…".into(),
                                            Box::new(SaveQuery),
                                            |_, _, cx| cx.emit(QueryEditorEvent::SaveQuery),
                                        )),
                                    })
                                    .separator()
                                    .item(
                                        item(
                                            "Edit Variables…".into(),
                                            Box::new(EditVariables),
                                            |editor, window, cx| {
                                                editor.edit_variables(&EditVariables, window, cx)
                                            },
                                        )
                                        .disabled(running),
                                    )
                                    .item(item(
                                        "Format Query".into(),
                                        Box::new(FormatQuery),
                                        |editor, window, cx| {
                                            editor.format_query(&FormatQuery, window, cx)
                                        },
                                    ))
                                    .item(item(
                                        "Toggle Comment".into(),
                                        Box::new(ToggleComment),
                                        |editor, window, cx| {
                                            editor.toggle_comment(&ToggleComment, window, cx)
                                        },
                                    ))
                                }
                            }),
                    ),
            )
    }
}

impl Render for QueryEditor {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let writes = self.statement_writes(cx);
        let can_analyze = self.analyze_supported();
        let blocker = self.transaction_blocker(cx);

        v_flex()
            .key_context("QueryEditor")
            .on_action(cx.listener(Self::run))
            .on_action(cx.listener(Self::run_script))
            .on_action(cx.listener(Self::run_script_ignoring_errors))
            .on_action(cx.listener(Self::explain))
            .on_action(cx.listener(Self::explain_analyze))
            .on_action(cx.listener(Self::format_query))
            .on_action(cx.listener(Self::edit_variables))
            .on_action(cx.listener(Self::toggle_comment))
            .on_action(cx.listener(Self::copy_line))
            .on_action(cx.listener(Self::cut_line))
            .on_action(cx.listener(Self::delete_line))
            .on_action(cx.listener(Self::move_line_up))
            .on_action(cx.listener(Self::move_line_down))
            .on_action(cx.listener(Self::duplicate_line_up))
            .on_action(cx.listener(Self::duplicate_line_down))
            .on_action(cx.listener(Self::select_next_occurrence))
            .on_action(cx.listener(Self::select_previous_occurrence))
            .on_action(cx.listener(Self::select_line))
            .on_action(cx.listener(Self::smart_home))
            .capture_key_down(cx.listener(Self::on_key_down))
            .capture_action(cx.listener(Self::accept_with_tab))
            .capture_action(cx.listener(Self::enter_with_menu_open))
            .size_full()
            .border_b_1()
            .border_color(cx.theme().border)
            .when(self.toolbar, |this| {
                this.child(self.render_toolbar(writes, can_analyze, blocker, cx))
            })
            .child(
                Editor::new(&self.state)
                    .h_full()
                    .appearance(false)
                    .when(self.toolbar, |editor| {
                        editor.border_t_1().border_color(cx.theme().border)
                    })
                    // Refines over the editor's own monospace default.
                    .font_family(settings::editor_font(cx))
                    // The built-in menu is replaced wholesale, so the
                    // standard items are rebuilt here with the format
                    // command leading them.
                    .context_menu(|menu, _, _| {
                        menu.menu("Format query", Box::new(FormatQuery))
                            .separator()
                            .menu("Cut", Box::new(Cut))
                            .menu("Copy", Box::new(Copy))
                            .menu("Paste", Box::new(Paste))
                            .separator()
                            .menu("Select All", Box::new(SelectAll))
                    }),
            )
    }
}
