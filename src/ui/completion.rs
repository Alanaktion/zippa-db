//! The query editor's completion menu, answered from the session's catalog.
//!
//! `gpui-kit`'s editor asks a [`CompletionProvider`] on each keystroke and
//! draws the menu itself; this module is the adapter between that and the
//! pure [`db::completion`](crate::db::completion), which decides what to offer.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use gpui_kit::component::RopeExt as _;
use gpui_kit::component::input::{CompletionProvider, Rope};
use gpui_kit::{App, Task, Window};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Range, TextEdit,
};

use crate::db::completion::{self, Completions, SuggestionKind};
use crate::db::{Catalog, Connection, Engine, runtime};

/// A buffer larger than this is read only around the caret: the completion
/// runs on every keystroke, and copying a multi-MB script out of the rope each
/// time would be a hitch. The statement the caret is in is all it reads.
const WHOLE_BUFFER: usize = 256 * 1024;

/// How much either side of the caret a large buffer is read.
const WINDOW: usize = 32 * 1024;

/// The session's catalog, shared with every query tab's editor.
///
/// The session replaces the snapshot when it reloads (`Refresh`, a database
/// switch); an editor reads whichever one is current when it is asked, so a
/// tab opened before the catalog finished loading still completes once it
/// has. The same sharing carries other databases' catalogs: when the pure
/// completion names a database with none loaded, the editor fetches it on
/// demand in the background, and the next keystroke offers its tables — the
/// way the initial load behaves.
#[derive(Clone, Default)]
pub struct SharedCatalog(Rc<RefCell<SharedCatalogInner>>);

#[derive(Default)]
struct SharedCatalogInner {
    current: Arc<Catalog>,
    /// The current database's name (`main` on SQLite), so `db.` naming it
    /// reuses `current` instead of fetching.
    database: String,
    /// Every database on the server.
    databases: Vec<String>,
    /// Catalogs fetched on demand, keyed by lower-cased database name.
    others: HashMap<String, Arc<Catalog>>,
    /// Lower-cased names with a fetch in flight.
    fetching: HashSet<String>,
    /// Lower-cased names whose fetch failed: not retried until the source
    /// changes, so a database the user cannot read does not cost a query per
    /// keystroke.
    failed: HashSet<String>,
    pending: Vec<(String, runtime::Task<Result<Catalog>>)>,
    connection: Option<Arc<Connection>>,
}

impl SharedCatalog {
    pub fn get(&self) -> Arc<Catalog> {
        self.0.borrow().current.clone()
    }

    pub fn set(&self, catalog: Arc<Catalog>) {
        self.0.borrow_mut().current = catalog;
    }

    /// The connection, database, and database list completions fetch other
    /// databases from. The session calls this on connect, switch, reconnect,
    /// and metadata reload; in-flight and failed fetches belonged to the
    /// previous state and are dropped, while fetched catalogs stay — the
    /// server is the same.
    pub fn set_source(&self, connection: Arc<Connection>, databases: Vec<String>) {
        // SQLite's qualifier is the attached name, `main` for the database
        // the session is bound to; the other engines use the database name.
        let database = match connection.config.engine {
            Engine::Sqlite => "main".to_string(),
            _ => connection.database().to_string(),
        };
        let mut inner = self.0.borrow_mut();
        inner.connection = Some(connection);
        inner.database = database;
        inner.databases = databases;
        inner.fetching.clear();
        inner.failed.clear();
        inner.pending.clear();
    }

    /// Move finished fetches into the catalog cache (or the failed set).
    fn drain_pending(&self) {
        let mut inner = self.0.borrow_mut();
        let mut done = Vec::new();
        for (index, (name, task)) in inner.pending.iter_mut().enumerate() {
            if let Some(result) = task.try_recv() {
                done.push((index, name.clone(), result));
            }
        }
        for (index, name, result) in done.into_iter().rev() {
            inner.pending.remove(index);
            inner.fetching.remove(&name);
            match result {
                Ok(catalog) => {
                    inner.others.insert(name, Arc::new(catalog));
                }
                Err(_) => {
                    inner.failed.insert(name);
                }
            }
        }
    }

    /// Fetch `database`'s schema in the background, unless it is already
    /// loaded, loading, or failed. The next keystroke picks the answer up.
    fn fetch(&self, database: &str) {
        let key = database.to_lowercase();
        let (connection, name) = {
            let mut inner = self.0.borrow_mut();
            if inner.others.contains_key(&key)
                || inner.fetching.contains(&key)
                || inner.failed.contains(&key)
            {
                return;
            }
            let Some(connection) = inner.connection.clone() else {
                return;
            };
            // Fetch under the server's own casing: the lookup can be
            // case-sensitive.
            let name = inner
                .databases
                .iter()
                .find(|known| known.eq_ignore_ascii_case(database))
                .cloned()
                .unwrap_or_else(|| database.to_string());
            inner.fetching.insert(key.clone());
            (connection, name)
        };
        let task = runtime::spawn(async move { connection.catalog_for_database(&name).await });
        self.0.borrow_mut().pending.push((key, task));
    }
}

/// The provider one query editor holds.
pub(crate) struct SqlCompletions {
    pub(crate) catalog: SharedCatalog,
    pub(crate) engine: Engine,
}

impl SqlCompletions {
    /// What to offer with the caret at byte `offset` of `text`.
    pub(crate) fn complete(&self, text: &Rope, offset: usize) -> Option<Completions> {
        self.catalog.drain_pending();
        let (base, end) = if text.len() > WHOLE_BUFFER {
            (
                text.floor_char_boundary(offset.saturating_sub(WINDOW)),
                text.floor_char_boundary((offset + WINDOW).min(text.len())),
            )
        } else {
            (0, text.len())
        };
        let sql = text.slice(base..end).to_string();
        let inner = self.catalog.0.borrow();
        let schemas = completion::Schemas {
            current: &inner.current,
            current_database: (!inner.database.is_empty()).then(|| inner.database.as_str()),
            others: &inner.others,
            databases: &inner.databases,
        };
        let mut completions = completion::complete(&sql, offset - base, &schemas, self.engine)?;
        drop(inner);
        // A keystroke never waits on the server: the fetch runs behind the
        // menu, and the keystroke after it lands offers the tables.
        for database in &completions.missing {
            self.catalog.fetch(database);
        }
        completions.replace = base + completions.replace.start..base + completions.replace.end;
        Some(completions)
    }
}

impl CompletionProvider for SqlCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _trigger: CompletionContext,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let items = self
            .complete(text, offset)
            .map(|completions| items(text, completions))
            .unwrap_or_default();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    /// Asked about every edit, so the menu also closes when what was typed
    /// ends the word (a space, a comma): the answer is then empty. A paste
    /// over several lines is not typing a word.
    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _cx: &mut App) -> bool {
        !new_text.is_empty() && !new_text.contains('\n')
    }
}

/// The editor's menu items for `completions`, each replacing the word typed
/// so far.
fn items(text: &Rope, completions: Completions) -> Vec<CompletionItem> {
    let range = Range::new(
        text.offset_to_position(completions.replace.start),
        text.offset_to_position(completions.replace.end),
    );
    let typed = completions.prefix.chars().count();
    completions
        .items
        .into_iter()
        .map(|suggestion| {
            // The menu highlights as many characters of the label as the
            // filter text has: what was typed.
            let filter: String = suggestion.label.chars().take(typed).collect();
            CompletionItem {
                kind: Some(kind(suggestion.kind)),
                detail: (!suggestion.detail.is_empty()).then_some(suggestion.detail),
                filter_text: Some(filter),
                text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                    range,
                    new_text: suggestion.insert,
                })),
                label: suggestion.label,
                ..Default::default()
            }
        })
        .collect()
}

fn kind(kind: SuggestionKind) -> CompletionItemKind {
    match kind {
        SuggestionKind::Keyword => CompletionItemKind::KEYWORD,
        SuggestionKind::Table | SuggestionKind::View => CompletionItemKind::STRUCT,
        SuggestionKind::Column => CompletionItemKind::FIELD,
        SuggestionKind::Routine => CompletionItemKind::FUNCTION,
        SuggestionKind::Schema => CompletionItemKind::MODULE,
    }
}
