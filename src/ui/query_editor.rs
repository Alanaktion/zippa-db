//! SQL query editor pane.
//!
//! A code editor with SQL highlighting that hands the statement under the
//! caret, the selection, or the whole buffer to the session, which runs it and
//! can cancel it. Typing offers completions from the session's catalog and a
//! list of SQL keywords (see [`crate::ui::completion`]).

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Editor, EditorState, Enter, IndentInline};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    ClipboardItem, Context, Entity, EventEmitter, KeyDownEvent, Window, actions, div, px,
};

use gpui_kit::assets::IconName as AssetIcon;

use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use crate::db::params::Variable;
use crate::db::{Blocker, Engine, statement, transaction_blocker};
use crate::settings::{self, CompletionKey, Settings};
use crate::ui::completion::{SharedCatalog, SqlCompletions};
use crate::ui::session::{OpenFile, SaveFile};

actions!(
    zippa_db,
    [
        RunQuery,
        RunScript,
        RunScriptIgnoringErrors,
        Explain,
        ExplainAnalyze,
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
    /// The user asked to edit this buffer's `:name` variables.
    EditVariables,
}

pub struct QueryEditor {
    state: Entity<EditorState>,
    /// The provider the editor asks, kept so a test can ask it too.
    #[cfg(test)]
    completions: Rc<SqlCompletions>,
    running: bool,
    /// The `:name` variables for this buffer, edited in the Variables dialog.
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
    /// Memoized statement that would keep the whole buffer out of a
    /// transaction, keyed by a hash of the buffer alone.
    blocker_cache: RefCell<Option<(u64, Option<Blocker>)>>,
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
            blocker_cache: RefCell::new(None),
        }
    }

    /// This buffer's `:name` variables.
    pub(crate) fn variables(&self) -> Vec<Variable> {
        self.variables.clone()
    }

    /// Replace this buffer's `:name` variables, as the Variables dialog does.
    pub(crate) fn set_variables(&mut self, variables: Vec<Variable>, cx: &mut Context<Self>) {
        self.variables = variables;
        cx.notify();
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

    /// The labels the completion menu would offer with the caret where it is.
    #[cfg(test)]
    pub(crate) fn completions_for_test(&self, cx: &gpui_kit::App) -> Vec<String> {
        let state = self.state.read(cx);
        self.completions
            .complete(state.text(), state.cursor())
            .map(|completions| completions.items.into_iter().map(|s| s.label).collect())
            .unwrap_or_default()
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
        self.running || self.statement_writes(cx) || !self.analyze_supported()
    }

    /// Whether this engine can report actual times. SQLite's planner cannot, so
    /// its analyze button is disabled rather than showing the plain plan under
    /// a button that promises timings.
    pub(crate) fn analyze_supported(&self) -> bool {
        !matches!(self.engine, Engine::Sqlite)
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

    /// Comment out the lines the selection touches, or the caret's line, and
    /// run it again to take the comments back off.
    fn toggle_comment(&mut self, _: &ToggleComment, window: &mut Window, cx: &mut Context<Self>) {
        let (text, selected, scroll) = {
            let state = self.state.read(cx);
            (
                state.value().to_string(),
                state.selected_range(),
                state.scroll_offset(),
            )
        };
        let Some((text, selected)) = toggle_comments(&text, selected) else {
            return;
        };
        self.state.update(cx, |state, cx| {
            // `replace_all` keeps the change on the undo stack, unlike
            // `set_value`; it clears the selection and jumps the view to the
            // top, so both are put back.
            state.replace_all(text, window, cx);
            state.set_selected_range(selected, cx);
            state.set_scroll_offset(scroll, cx);
        });
    }

    fn explain_analyze(
        &mut self,
        _: &ExplainAnalyze,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.emit_explain(true, cx);
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
    fn copy_line(&mut self, _: &CopyLine, _window: &mut Window, cx: &mut Context<Self>) {
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
    fn cut_line(&mut self, _: &CutLine, window: &mut Window, cx: &mut Context<Self>) {
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
    fn delete_line(&mut self, _: &DeleteLine, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_buffer(delete_lines, window, cx);
    }

    /// Move the touched lines up one, the selection riding along.
    fn move_line_up(&mut self, _: &MoveLineUp, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_buffer(
            |text, selected| move_lines(text, selected, true),
            window,
            cx,
        );
    }

    /// Move the touched lines down one, the selection riding along.
    fn move_line_down(&mut self, _: &MoveLineDown, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_buffer(
            |text, selected| move_lines(text, selected, false),
            window,
            cx,
        );
    }

    /// Duplicate the touched lines above themselves, the selection moving
    /// onto the new copy.
    fn duplicate_line_up(
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
    fn duplicate_line_down(
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
    fn select_next_occurrence(
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
    fn select_previous_occurrence(
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
    fn select_line(&mut self, _: &SelectLine, _window: &mut Window, cx: &mut Context<Self>) {
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
    fn smart_home(&mut self, _: &SmartHome, _window: &mut Window, cx: &mut Context<Self>) {
        let (text, caret) = {
            let state = self.state.read(cx);
            (state.value().to_string(), state.cursor())
        };
        let target = smart_home_offset(&text, caret);
        self.state
            .update(cx, |state, cx| state.set_selected_range(target..target, cx));
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
    /// a transaction, if there is one — memoized on the buffer's text, since
    /// the toolbar asks on every render.
    pub(crate) fn transaction_blocker(&self, cx: &gpui_kit::App) -> Option<Blocker> {
        let key = {
            let mut hasher = DefaultHasher::new();
            for chunk in self.state.read(cx).text().chunks() {
                chunk.hash(&mut hasher);
            }
            hasher.finish()
        };
        if let Some((cached, blocker)) = self.blocker_cache.borrow().as_ref()
            && *cached == key
        {
            return blocker.clone();
        }
        let sql = self.state.read(cx).value().to_string();
        let blocker = transaction_blocker(self.engine, &statement::split(&sql, self.engine));
        *self.blocker_cache.borrow_mut() = Some((key, blocker.clone()));
        blocker
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

/// Toggle `-- ` line comments over the lines a selection touches — or the
/// caret's line — returning the new buffer and where the selection should sit
/// in it. `None` when there is nothing to change, so a blank caret line is a
/// no-op rather than an empty edit on the undo stack.
///
/// A blank line within the run is left alone; the run is uncommented only when
/// every non-blank line already carries a comment, and commented as a whole
/// otherwise. The comment goes in after the line's indentation, so the marker
/// lines up with the statement rather than the margin. Pure, so the buffer is
/// reshaped in unit tests without a window.
fn toggle_comments(
    text: &str,
    selected: std::ops::Range<usize>,
) -> Option<(String, std::ops::Range<usize>)> {
    let len = text.len();
    let start = selected.start.min(len);
    let end = selected.end.min(len);
    // A selection that ends exactly at the start of a line does not take that
    // line with it, the way dragging down to the next line's margin does not.
    let last = if end > start && text[..end].ends_with('\n') {
        end - 1
    } else {
        end
    };

    // The lines the toggle covers, as byte ranges without their newline.
    let mut spans = Vec::new();
    let last_line_end = line_end(text, last);
    let mut cursor = line_start(text, start);
    loop {
        let end_of_line = line_end(text, cursor);
        spans.push((cursor, end_of_line));
        if end_of_line == last_line_end {
            break;
        }
        cursor = end_of_line + 1;
    }

    let blank = |line: &str| line.trim().is_empty();
    let commented = |line: &str| line.trim_start_matches([' ', '\t']).starts_with("--");
    let non_blank: Vec<&str> = spans
        .iter()
        .map(|&(s, e)| &text[s..e])
        .filter(|line| !blank(line))
        .collect();
    let uncomment = !non_blank.is_empty() && non_blank.iter().all(|line| commented(line));

    let mut out = String::with_capacity(len);
    // Where the text was changed and by how much, so a caret or selection in
    // the old buffer can be carried into the new one.
    let mut edits: Vec<(usize, isize)> = Vec::new();
    let mut prev = 0;
    for &(s, e) in &spans {
        out.push_str(&text[prev..s]);
        let line = &text[s..e];
        if blank(line) {
            out.push_str(line);
        } else {
            let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
            let at = s + indent;
            out.push_str(&line[..indent]);
            if uncomment {
                let rest = &line[indent..];
                // `-- ` is what this writes; a hand-written `--x` comes off
                // without taking the `x` with it.
                let marker = if rest.starts_with("-- ") { 3 } else { 2 };
                out.push_str(&rest[marker..]);
                edits.push((at, -(marker as isize)));
            } else {
                out.push_str("-- ");
                out.push_str(&line[indent..]);
                edits.push((at, 3));
            }
        }
        prev = e;
    }
    out.push_str(&text[prev..]);

    if out == text {
        return None;
    }

    let shift = |offset: usize| -> usize {
        let mut shifted = offset as isize;
        for &(at, delta) in &edits {
            if delta > 0 {
                // Strictly after the marker: an endpoint on the marker's own
                // line start keeps to the left of it, so a selection still
                // covers what it commented.
                if offset > at {
                    shifted += delta;
                }
            } else {
                let removed = (-delta) as usize;
                if offset >= at + removed {
                    shifted += delta;
                } else if offset > at {
                    // Inside the comment marker: land on the new line start.
                    shifted -= (offset - at) as isize;
                }
            }
        }
        shifted.clamp(0, out.len() as isize) as usize
    };
    let selection = shift(start)..shift(end);
    Some((out, selection))
}

/// The byte offset of the first character of the line `offset` is on.
fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map(|at| at + 1).unwrap_or(0)
}

/// The byte offset of the newline that ends the line `offset` is on, or the
/// end of the text when it is the last line.
fn line_end(text: &str, offset: usize) -> usize {
    text[offset..]
        .find('\n')
        .map(|at| offset + at)
        .unwrap_or(text.len())
}

/// Byte spans of every line in the buffer, newlines excluded. A trailing
/// newline leaves a final empty span, which is the empty line after it.
fn all_lines(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            spans.push((start, at));
            start = at + 1;
        }
    }
    spans.push((start, text.len()));
    spans
}

/// The first and last line indices a selection touches, inclusive. A
/// selection that ends exactly at a line's start does not take that line —
/// the way dragging down to the next line's margin does not.
fn touched_lines(
    spans: &[(usize, usize)],
    text: &str,
    selected: std::ops::Range<usize>,
) -> (usize, usize) {
    let len = text.len();
    let start = selected.start.min(len);
    let end = selected.end.min(len);
    let last = if end > start && text[..end].ends_with('\n') {
        end - 1
    } else {
        end
    };
    let line_of = |offset: usize| spans.iter().rposition(|&(s, _)| s <= offset).unwrap_or(0);
    (line_of(start), line_of(last))
}

/// Rebuild the buffer from `order`, a permutation of line indices, joining
/// with newlines. Returns the text and each line's new byte offset, keyed by
/// line index.
fn join_lines(text: &str, spans: &[(usize, usize)], order: &[usize]) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut new_starts = vec![0usize; spans.len()];
    for (at, &line) in order.iter().enumerate() {
        if at > 0 {
            out.push('\n');
        }
        new_starts[line] = out.len();
        let (s, e) = spans[line];
        out.push_str(&text[s..e]);
    }
    (out, new_starts)
}

/// Shift a selection that rides along with a rigidly moved block of lines.
fn shift_selection(
    selected: std::ops::Range<usize>,
    delta: isize,
    len: usize,
) -> std::ops::Range<usize> {
    let shift = |offset: usize| (offset as isize + delta).clamp(0, len as isize) as usize;
    shift(selected.start)..shift(selected.end)
}

/// Delete the lines a selection touches, returning the new buffer and where
/// the caret lands. `None` when there is no line to delete.
fn delete_lines(
    text: &str,
    selected: std::ops::Range<usize>,
) -> Option<(String, std::ops::Range<usize>)> {
    let spans = all_lines(text);
    let (first, last) = touched_lines(&spans, text, selected);
    let (start, _) = spans[first];
    let (_, end) = spans[last];
    // One newline goes with the lines: the one after them, or the one before
    // when they are the last lines.
    let (rm_start, rm_end) = if end < text.len() {
        (start, end + 1)
    } else if start > 0 {
        (start - 1, end)
    } else {
        (start, end)
    };
    if rm_start == rm_end {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..rm_start]);
    out.push_str(&text[rm_end..]);
    let caret = rm_start.min(out.len());
    Some((out, caret..caret))
}

/// Delete exactly the selected range. `None` for a bare caret.
fn delete_range(
    text: &str,
    selected: std::ops::Range<usize>,
) -> Option<(String, std::ops::Range<usize>)> {
    if selected.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..selected.start]);
    out.push_str(&text[selected.end..]);
    Some((out, selected.start..selected.start))
}

/// Move the lines a selection touches up or down one line, the selection
/// riding along. `None` when the lines are already at that edge.
fn move_lines(
    text: &str,
    selected: std::ops::Range<usize>,
    up: bool,
) -> Option<(String, std::ops::Range<usize>)> {
    let spans = all_lines(text);
    let (first, last) = touched_lines(&spans, text, selected.clone());
    let mut order: Vec<usize> = (0..spans.len()).collect();
    if up {
        if first == 0 {
            return None;
        }
        let block: Vec<usize> = order.drain(first..=last).collect();
        for (at, line) in block.into_iter().enumerate() {
            order.insert(first - 1 + at, line);
        }
    } else {
        if last + 1 >= spans.len() {
            return None;
        }
        let block: Vec<usize> = order.drain(first..=last).collect();
        for (at, line) in block.into_iter().enumerate() {
            order.insert(first + 1 + at, line);
        }
    }
    let (out, new_starts) = join_lines(text, &spans, &order);
    let delta = new_starts[first] as isize - spans[first].0 as isize;
    let len = out.len();
    Some((out, shift_selection(selected, delta, len)))
}

/// Duplicate the lines a selection touches above or below themselves, the
/// selection moving onto the new copy the way it does in VS Code.
fn duplicate_lines(
    text: &str,
    selected: std::ops::Range<usize>,
    up: bool,
) -> Option<(String, std::ops::Range<usize>)> {
    let spans = all_lines(text);
    let (first, last) = touched_lines(&spans, text, selected.clone());
    let block: Vec<usize> = (first..=last).collect();
    let mut order: Vec<usize> = (0..spans.len()).collect();
    // Where the copy's first line lands in the rebuilt order. A line index
    // appears twice now, so the copy's start is tracked by position.
    let copy_at = if up { first } else { last + 1 };
    for (at, line) in block.iter().enumerate() {
        order.insert(copy_at + at, *line);
    }
    let mut out = String::with_capacity(text.len() + spans[last].1 - spans[first].0 + 1);
    let mut copy_start = 0;
    for (at, &line) in order.iter().enumerate() {
        if at > 0 {
            out.push('\n');
        }
        if at == copy_at {
            copy_start = out.len();
        }
        let (s, e) = spans[line];
        out.push_str(&text[s..e]);
    }
    let delta = copy_start as isize - spans[first].0 as isize;
    let len = out.len();
    Some((out, shift_selection(selected, delta, len)))
}

/// The text ⌘C copies for the caret's line: the line plus its newline, so
/// pasting it back yields a whole line.
fn line_clip_text(text: &str, caret: usize) -> String {
    let spans = all_lines(text);
    let caret = caret.min(text.len());
    let index = spans.iter().rposition(|&(s, _)| s <= caret).unwrap_or(0);
    let (s, e) = spans[index];
    let mut out = text[s..e].to_string();
    if e < text.len() {
        out.push('\n');
    }
    out
}

/// The range ⌘L selects: the touched lines whole, newlines included. When
/// they are already exactly selected, it extends through the next line.
fn select_line_range(text: &str, selected: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let spans = all_lines(text);
    let (first, last) = touched_lines(&spans, text, selected.clone());
    let with_break = |index: usize| {
        let (_, e) = spans[index];
        if e < text.len() { e + 1 } else { e }
    };
    let full = spans[first].0..with_break(last);
    if !selected.is_empty() && selected == full && last + 1 < spans.len() {
        spans[first].0..with_break(last + 1)
    } else {
        full
    }
}

/// Where Home — or ⌘◀ — moves the caret: the first non-blank character,
/// unless already there, when it goes to the line's start.
fn smart_home_offset(text: &str, caret: usize) -> usize {
    let caret = caret.min(text.len());
    let start = line_start(text, caret);
    let indent = text[start..].len() - text[start..].trim_start_matches([' ', '\t']).len();
    let first_text = start + indent;
    if caret == first_text {
        start
    } else {
        first_text
    }
}

/// Whether a byte is part of a word for occurrence matching: letters,
/// digits, and underscores.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The word under the caret, if the caret touches one.
fn word_at(text: &str, caret: usize) -> Option<std::ops::Range<usize>> {
    let bytes = text.as_bytes();
    let caret = caret.min(bytes.len());
    let mut start = caret;
    while start > 0 && is_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = caret;
    while end < bytes.len() && is_word_byte(bytes[end]) {
        end += 1;
    }
    if start == end { None } else { Some(start..end) }
}

/// The next occurrence of the selected text, wrapping around the buffer.
/// With no selection, the word under the caret is selected first. `None`
/// when there is no other occurrence.
fn next_occurrence(text: &str, selected: std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
    let selected = if selected.is_empty() {
        word_at(text, selected.start)?
    } else {
        selected
    };
    let needle = &text[selected.clone()];
    if needle.is_empty() {
        return None;
    }
    let len = needle.len();
    let mut found = text[selected.end..]
        .find(needle)
        .map(|at| selected.end + at);
    if found.is_none() {
        found = text[..selected.end].find(needle);
    }
    let start = found?;
    let range = start..start + len;
    if range == selected { None } else { Some(range) }
}

/// The previous occurrence of the selected text, wrapping around the
/// buffer. With no selection, the word under the caret is selected first.
/// `None` when there is no other occurrence.
fn previous_occurrence(
    text: &str,
    selected: std::ops::Range<usize>,
) -> Option<std::ops::Range<usize>> {
    let selected = if selected.is_empty() {
        word_at(text, selected.start)?
    } else {
        selected
    };
    let needle = &text[selected.clone()];
    if needle.is_empty() {
        return None;
    }
    let len = needle.len();
    let mut found = text[..selected.start].rfind(needle);
    if found.is_none() {
        found = text[selected.start..]
            .rfind(needle)
            .map(|at| selected.start + at);
    }
    let start = found?;
    let range = start..start + len;
    if range == selected { None } else { Some(range) }
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
            .child(
                h_flex()
                    .px_2()
                    .py_1()
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
                                Button::new("variables")
                                    .ghost()
                                    .small()
                                    .icon(AssetIcon::Variable)
                                    .accessibility_label("Edit query variables")
                                    .tooltip("Edit the :name variables for this buffer")
                                    .disabled(self.running)
                                    .on_click(cx.listener(|_this, _, _window, cx| {
                                        cx.emit(QueryEditorEvent::EditVariables)
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
                                        if !can_analyze {
                                            "SQLite has no EXPLAIN ANALYZE; actual times are not available"
                                        } else if writes {
                                            "Analyze runs the query; this statement changes data"
                                        } else {
                                            "Explain the statement and run it for actual times"
                                        },
                                        &ExplainAnalyze,
                                        Some("QueryEditor"),
                                    )
                                    .disabled(self.running || writes || !can_analyze)
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.emit_explain(true, cx)
                                    })),
                            )
                            .child({
                                // A buffer a transaction cannot hold says so
                                // before it is run, in words and with its
                                // own icon rather than a colour.
                                let (icon, label) = match &blocker {
                                    Some(blocker) => (
                                        IconName::TriangleAlert,
                                        format!(
                                            "Run every statement without a transaction: {}",
                                            blocker.message()
                                        ),
                                    ),
                                    None => (
                                        IconName::SquareTerminal,
                                        "Run every statement in one transaction, asking about \
                                         each error"
                                            .to_string(),
                                    ),
                                };
                                Button::new("run-script")
                                    .ghost()
                                    .small()
                                    .icon(icon)
                                    .accessibility_label(label.clone())
                                    .tooltip_with_action(label, &RunScript, Some("QueryEditor"))
                                    .disabled(self.running)
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.emit_run_script(false, cx)
                                    }))
                            })
                            .child(
                                Button::new("run-script-ignoring-errors")
                                    .ghost()
                                    .small()
                                    .icon(AssetIcon::FastForward)
                                    .accessibility_label(
                                        "Run every statement without a transaction, skipping errors",
                                    )
                                    .tooltip_with_action(
                                        "Run every statement without a transaction, skipping errors",
                                        &RunScriptIgnoringErrors,
                                        Some("QueryEditor"),
                                    )
                                    .disabled(self.running)
                                    .on_click(cx.listener(|this, _, _window, cx| {
                                        this.emit_run_script(true, cx)
                                    })),
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
                Editor::new(&self.state)
                    .h_full()
                    .appearance(false)
                    .border_t_1()
                    .border_color(cx.theme().border)
                    // Refines over the editor's own monospace default.
                    .font_family(settings::editor_font(cx)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Toggle over `text` with `selected` as the buffer selection, which must
    /// change something.
    fn toggle(text: &str, selected: std::ops::Range<usize>) -> (String, std::ops::Range<usize>) {
        toggle_comments(text, selected).expect("there should be a change to make")
    }

    #[test]
    fn the_caret_line_is_commented() {
        assert_eq!(toggle("select 1", 3..3), ("-- select 1".to_string(), 6..6));
    }

    #[test]
    fn commenting_keeps_the_indentation() {
        assert_eq!(
            toggle("    select 1", 6..6),
            ("    -- select 1".to_string(), 9..9)
        );
    }

    #[test]
    fn a_commented_line_comes_back_off() {
        assert_eq!(toggle("-- select 1", 3..3), ("select 1".to_string(), 0..0));
        assert_eq!(
            toggle("  -- select 1", 5..5),
            ("  select 1".to_string(), 2..2)
        );
    }

    #[test]
    fn a_marker_with_no_space_comes_off_cleanly() {
        assert_eq!(toggle("--select 1", 2..2), ("select 1".to_string(), 0..0));
    }

    #[test]
    fn a_selection_comments_every_line_it_touches() {
        let (new, selection) = toggle("select 1\nselect 2\nselect 3", 0..18);
        assert_eq!(new, "-- select 1\n-- select 2\nselect 3");
        assert_eq!(
            selection,
            0..24,
            "the selection should still cover the lines it commented"
        );
    }

    #[test]
    fn a_selection_ending_at_the_next_line_does_not_take_it() {
        let (new, _) = toggle("select 1\nselect 2", 0..9);
        assert_eq!(new, "-- select 1\nselect 2");
    }

    #[test]
    fn a_fully_commented_run_is_uncommented_together() {
        let text = "-- select 1\n  -- select 2";
        let (new, selection) = toggle(text, 0..text.len());
        assert_eq!(new, "select 1\n  select 2");
        assert_eq!(selection, 0..19);
    }

    #[test]
    fn a_mixed_run_is_commented_as_a_whole() {
        let text = "-- select 1\nselect 2";
        let (new, _) = toggle(text, 0..text.len());
        assert_eq!(new, "-- -- select 1\n-- select 2");
    }

    #[test]
    fn blank_lines_are_left_alone() {
        let text = "select 1\n\nselect 2";
        let (new, _) = toggle(text, 0..text.len());
        assert_eq!(new, "-- select 1\n\n-- select 2");
    }

    #[test]
    fn a_blank_caret_line_is_a_no_op() {
        assert_eq!(toggle_comments("   \nselect 1", 1..1), None);
    }

    #[test]
    fn the_selection_survives_the_comments_coming_off() {
        let text = "-- select 1\n-- select 2";
        let (new, selection) = toggle(text, 0..text.len());
        assert_eq!(new, "select 1\nselect 2");
        assert_eq!(selection, 0..17);
    }

    #[test]
    fn delete_lines_removes_the_caret_line_and_its_newline() {
        let (new, caret) = delete_lines("select 1\nselect 2\nselect 3", 3..3).unwrap();
        assert_eq!(new, "select 2\nselect 3");
        assert_eq!(caret, 0..0);
    }

    #[test]
    fn delete_lines_takes_the_newline_before_the_last_line() {
        let (new, caret) = delete_lines("select 1\nselect 2", 12..12).unwrap();
        assert_eq!(new, "select 1");
        assert_eq!(caret, 8..8);
    }

    #[test]
    fn delete_lines_removes_every_line_the_selection_touches() {
        let (new, caret) = delete_lines("a\nb\nc", 0..4).unwrap();
        assert_eq!(new, "c");
        assert_eq!(caret, 0..0);
    }

    #[test]
    fn delete_lines_stops_at_a_line_start() {
        // The selection covers "a\n" and ends where "b" starts: only "a" goes.
        let (new, _) = delete_lines("a\nb\nc", 0..2).unwrap();
        assert_eq!(new, "b\nc");
    }

    #[test]
    fn delete_lines_on_an_empty_buffer_is_a_no_op() {
        assert_eq!(delete_lines("", 0..0), None);
    }

    #[test]
    fn delete_range_removes_the_selection() {
        let (new, caret) = delete_range("select 1", 2..8).unwrap();
        assert_eq!(new, "se");
        assert_eq!(caret, 2..2);
    }

    #[test]
    fn delete_range_with_a_bare_caret_is_a_no_op() {
        assert_eq!(delete_range("select 1", 3..3), None);
    }

    #[test]
    fn move_lines_up_swaps_with_the_line_above() {
        let (new, selection) = move_lines("a\nb\nc", 2..2, true).unwrap();
        assert_eq!(new, "b\na\nc");
        assert_eq!(selection, 0..0, "the caret rides along with its line");
    }

    #[test]
    fn move_lines_down_swaps_with_the_line_below() {
        let (new, selection) = move_lines("a\nb\nc", 2..2, false).unwrap();
        assert_eq!(new, "a\nc\nb");
        assert_eq!(selection, 4..4);
    }

    #[test]
    fn move_lines_moves_a_whole_selection() {
        let (new, selection) = move_lines("a\nb\nc\nd", 2..5, true).unwrap();
        assert_eq!(new, "b\nc\na\nd");
        assert_eq!(selection, 0..3);
    }

    #[test]
    fn move_lines_at_the_edges_is_a_no_op() {
        assert_eq!(move_lines("a\nb", 0..0, true), None);
        assert_eq!(move_lines("a\nb", 2..2, false), None);
    }

    #[test]
    fn move_lines_down_past_a_trailing_newline() {
        let (new, selection) = move_lines("a\nb\n", 2..2, false).unwrap();
        assert_eq!(new, "a\n\nb");
        assert_eq!(selection, 3..3);
    }

    #[test]
    fn duplicate_lines_down_copies_below_and_moves_the_caret_onto_it() {
        let (new, selection) = duplicate_lines("a\nb\nc", 2..2, false).unwrap();
        assert_eq!(new, "a\nb\nb\nc");
        assert_eq!(selection, 4..4);
    }

    #[test]
    fn duplicate_lines_up_copies_above_and_moves_the_caret_onto_it() {
        let (new, selection) = duplicate_lines("a\nb\nc", 2..2, true).unwrap();
        assert_eq!(new, "a\nb\nb\nc");
        assert_eq!(selection, 2..2);
    }

    #[test]
    fn duplicate_lines_down_at_the_end_grows_the_buffer() {
        let (new, selection) = duplicate_lines("a\nb", 2..2, false).unwrap();
        assert_eq!(new, "a\nb\nb");
        assert_eq!(selection, 4..4);
    }

    #[test]
    fn duplicate_lines_copies_a_whole_selection() {
        let (new, selection) = duplicate_lines("a\nb\nc", 0..3, false).unwrap();
        assert_eq!(new, "a\nb\na\nb\nc");
        assert_eq!(selection, 4..7);
    }

    #[test]
    fn line_clip_text_carries_the_newline() {
        assert_eq!(line_clip_text("a\nb\nc", 2), "b\n");
        assert_eq!(line_clip_text("a\nb", 2), "b");
        assert_eq!(line_clip_text("a\nb\nc", 0), "a\n");
    }

    #[test]
    fn select_line_range_takes_the_line_and_its_newline() {
        assert_eq!(select_line_range("a\nb\nc", 2..2), 2..4);
        assert_eq!(select_line_range("a\nb", 2..2), 2..3);
    }

    #[test]
    fn select_line_range_expands_a_partial_selection() {
        assert_eq!(select_line_range("a\nb", 0..1), 0..2);
    }

    #[test]
    fn select_line_range_pressed_twice_reaches_the_next_line() {
        assert_eq!(select_line_range("a\nb\nc", 2..4), 2..5);
        assert_eq!(select_line_range("a\nb\nc\n", 2..4), 2..6);
    }

    #[test]
    fn smart_home_toggles_between_indent_and_line_start() {
        assert_eq!(smart_home_offset("    select 1", 6), 4);
        assert_eq!(smart_home_offset("    select 1", 4), 0);
        assert_eq!(smart_home_offset("    select 1", 0), 4);
        assert_eq!(smart_home_offset("    select 1", 2), 4);
    }

    #[test]
    fn smart_home_on_a_blank_line_stays_put() {
        assert_eq!(smart_home_offset("select 1", 3), 0);
        assert_eq!(smart_home_offset("select 1", 0), 0);
    }

    #[test]
    fn smart_home_counts_tabs_as_indentation() {
        assert_eq!(smart_home_offset("\t\tselect", 5), 2);
        assert_eq!(smart_home_offset("\t\tselect", 2), 0);
    }

    #[test]
    fn word_at_finds_the_word_touching_the_caret() {
        assert_eq!(word_at("foo bar", 1), Some(0..3));
        assert_eq!(word_at("foo bar", 0), Some(0..3));
        // At a word boundary the word to the left is taken, the way ⌘D does.
        assert_eq!(word_at("foo bar", 3), Some(0..3));
        assert_eq!(word_at("foo  bar", 4), None);
        assert_eq!(word_at("foo_bar", 4), Some(0..7));
    }

    #[test]
    fn next_occurrence_selects_the_word_first_then_advances() {
        assert_eq!(next_occurrence("foo bar foo", 1..1), Some(8..11));
        assert_eq!(next_occurrence("foo bar foo", 0..3), Some(8..11));
    }

    #[test]
    fn next_occurrence_wraps_around() {
        assert_eq!(next_occurrence("foo bar foo", 8..11), Some(0..3));
    }

    #[test]
    fn next_occurrence_with_a_lone_match_is_a_no_op() {
        assert_eq!(next_occurrence("foo", 0..3), None);
        assert_eq!(next_occurrence("foo", 1..1), None);
        assert_eq!(next_occurrence("a + b", 2..2), None);
    }

    #[test]
    fn occurrence_matching_is_case_sensitive() {
        assert_eq!(next_occurrence("Foo foo", 0..3), None);
    }

    #[test]
    fn previous_occurrence_goes_backwards_and_wraps() {
        assert_eq!(previous_occurrence("foo bar foo", 8..11), Some(0..3));
        assert_eq!(previous_occurrence("foo bar foo", 0..3), Some(8..11));
    }

    #[test]
    fn previous_occurrence_with_a_lone_match_is_a_no_op() {
        assert_eq!(previous_occurrence("foo", 0..3), None);
        assert_eq!(previous_occurrence("foo bar", 5..5), None);
    }
}
