//! The query editor's completion menu, answered from the session's catalog.
//!
//! `gpui-kit`'s editor asks a [`CompletionProvider`] on each keystroke and
//! draws the menu itself; this module is the adapter between that and the
//! pure [`db::completion`](crate::db::completion), which decides what to offer.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::component::RopeExt as _;
use gpui_kit::component::input::{CompletionProvider, Rope};
use gpui_kit::{App, Task, Window};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Range, TextEdit,
};

use crate::db::completion::{self, Completions, SuggestionKind};
use crate::db::{Catalog, Engine};

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
/// has.
#[derive(Clone, Default)]
pub struct SharedCatalog(Rc<RefCell<Arc<Catalog>>>);

impl SharedCatalog {
    pub fn get(&self) -> Arc<Catalog> {
        self.0.borrow().clone()
    }

    pub fn set(&self, catalog: Arc<Catalog>) {
        *self.0.borrow_mut() = catalog;
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
        let (base, end) = if text.len() > WHOLE_BUFFER {
            (
                text.floor_char_boundary(offset.saturating_sub(WINDOW)),
                text.floor_char_boundary((offset + WINDOW).min(text.len())),
            )
        } else {
            (0, text.len())
        };
        let sql = text.slice(base..end).to_string();
        let mut completions =
            completion::complete(&sql, offset - base, &self.catalog.get(), self.engine)?;
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
