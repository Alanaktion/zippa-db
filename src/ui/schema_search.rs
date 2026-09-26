//! Schema search dialog (`secondary-shift-O`).
//!
//! The sidebar filter and the quick switcher only match object *names*, so
//! "where is `email` stored?" or "what index covers `created_at`?" cannot be
//! answered without opening tables. This dialog searches the whole catalog the
//! session keeps — tables, views, columns, indexes, routines and triggers —
//! and opens the right place for a result.
//!
//! The ranking is [`catalog::search`], a pure function; this view only turns
//! its hits into rows. Because the ranking is ours, the [`Command`] component
//! is told not to filter (`filterable(false)`) and the item list is rebuilt
//! from the ranked hits on every keystroke.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::command::{Command, CommandGroup, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, WindowExt, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, IntoElement, Render, SharedString, Window, div, px};

use crate::db::catalog::{self, Hit, MAX_HITS};
use crate::db::{Catalog, CatalogEntry, CatalogKind, Query};

use super::session::Session;

/// The order groups are listed in. Headings come from [`CatalogKind::group`],
/// so a group is never dropped because its label drifted from this list.
const GROUPS: [CatalogKind; 5] = [
    CatalogKind::Table,
    CatalogKind::Column,
    CatalogKind::Index,
    CatalogKind::Routine,
    CatalogKind::Trigger,
];

/// How long a burst of keystrokes may settle before the catalog is
/// re-scanned. The scan already runs off the UI thread, but re-running it
/// per keystroke is still wasted work.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

pub struct SchemaSearchView {
    state: Entity<CommandState>,
    session: Entity<Session>,
    /// The text the user has typed, mirrored here so a change re-renders this
    /// view and rebuilds the list.
    query: String,
    /// The kind chips that are switched on; empty means every kind.
    kinds: Vec<CatalogKind>,
    /// The ranked hits for `query`, computed on the background executor so
    /// typing never stalls the UI; `generation` drops results a newer
    /// keystroke has already superseded.
    hits: Vec<Hit>,
    matched: usize,
    generation: u64,
}

impl SchemaSearchView {
    pub fn new(session: Entity<Session>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        // The catalog is read in the background as the session opens; when it
        // lands, the "Loading schema…" line is replaced by the results.
        cx.observe(&session, |this, _, cx| this.refresh_search(cx))
            .detach();
        let view = Self {
            state,
            session,
            query: String::new(),
            kinds: Vec::new(),
            hits: Vec::new(),
            matched: 0,
            generation: 0,
        };
        // The first search runs on the next tick: the session opening this
        // dialog is borrowed while `new` runs, so it cannot be read yet.
        cx.spawn(async move |this, cx| {
            this.update_in(cx, |this, _, cx| this.refresh_search(cx))
                .ok();
        })
        .detach();
        view
    }

    /// Re-rank the catalog against the current query off the UI thread.
    ///
    /// A full scan plus sort of up to 200k entries costs tens of milliseconds,
    /// so it runs on the background executor and folds in when it lands.
    fn refresh_search(&mut self, cx: &mut Context<Self>) {
        let catalog = self.session.read(cx).catalog();
        let mut query = Query::parse(&self.query);
        // The chips narrow the query the way the `kind:` prefix does; both can
        // be used at once and a kind either selects is kept.
        for kind in &self.kinds {
            if !query.kinds.contains(kind) {
                query.kinds.push(*kind);
            }
        }
        self.generation += 1;
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            // Let a burst of keystrokes settle before scanning; a newer one
            // supersedes this run and its results are dropped by the
            // generation check below.
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let superseded = this
                .update_in(cx, |this, _, _| this.generation != generation)
                .unwrap_or(true);
            if superseded {
                return;
            }
            let (hits, matched) = cx
                .background_spawn(async move { catalog::search(&catalog.entries, &query) })
                .await;
            this.update_in(cx, |this, _, cx| {
                if this.generation != generation {
                    return;
                }
                this.hits = hits;
                this.matched = matched;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn toggle_kind(&mut self, kind: CatalogKind, cx: &mut Context<Self>) {
        match self.kinds.iter().position(|active| *active == kind) {
            Some(index) => {
                self.kinds.remove(index);
            }
            None => self.kinds.push(kind),
        }
        self.refresh_search(cx);
    }

    /// How many ranked hits are currently shown, for a test to read.
    #[cfg(test)]
    pub(crate) fn hit_count_for_test(&self) -> usize {
        self.hits.len()
    }

    fn render_chips(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let all = self.kinds.is_empty();
        h_flex()
            .flex_wrap()
            .gap_1()
            .child(
                Button::new("schema-search-all")
                    .ghost()
                    .xsmall()
                    .label("All")
                    .toggled(all)
                    .accessibility_label("Show every kind of result")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.kinds.clear();
                        this.refresh_search(cx);
                    })),
            )
            .children(CatalogKind::ALL.into_iter().map(|kind| {
                let toggled = self.kinds.contains(&kind);
                Button::new(SharedString::from(format!(
                    "schema-search-kind-{}",
                    kind.keyword()
                )))
                .ghost()
                .xsmall()
                .label(kind.label())
                .toggled(toggled)
                .accessibility_label(format!("Show only {} results", kind.group()))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_kind(kind, cx)))
            }))
    }
}

/// Open schema search over the active window.
/// Open the schema search dialog, unless one is already open.
///
/// Returns the dialog's view when one was opened.
pub fn open(
    session: Entity<Session>,
    window: &mut Window,
    cx: &mut App,
) -> Option<Entity<SchemaSearchView>> {
    if window.has_active_dialog(cx) {
        return None;
    }

    let view = cx.new(|cx| SchemaSearchView::new(session, window, cx));
    let state = view.read(cx).state.clone();
    let body = view.clone();

    window.open_dialog(cx, move |dialog, _window, _cx| {
        dialog
            .w(px(640.))
            .margin_top(px(80.))
            .p_0()
            .close_button(false)
            .overlay_closable(true)
            .keyboard(true)
            .child(body.clone())
    });

    state.update(cx, |state, cx| state.focus(window, cx));
    Some(view)
}

impl Render for SchemaSearchView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (catalog, loading, error) = {
            let session = self.session.read(cx);
            (
                session.catalog(),
                session.catalog_loading(),
                session.catalog_error().map(str::to_string),
            )
        };

        // The chips narrow the query the way the `kind:` prefix does; both can
        // be used at once and a kind either selects is kept.
        let hits = &self.hits;
        let matched = self.matched;

        // One Command group per heading, with a parallel list of the entries
        // each row stands for, so confirming a path finds its entry.
        let mut groups: Vec<CommandGroup> = Vec::new();
        let mut targets: Vec<Vec<CatalogEntry>> = Vec::new();
        for heading in GROUPS.map(CatalogKind::group) {
            let mut group = CommandGroup::new().label(heading);
            let mut section = Vec::new();
            for hit in hits {
                let entry = &catalog.entries[hit.index];
                if entry.kind.group() != heading {
                    continue;
                }
                group = group.item(item_for(entry));
                section.push(entry.clone());
            }
            if !section.is_empty() {
                groups.push(group);
                targets.push(section);
            }
        }

        let targets = Rc::new(targets);
        let session = self.session.clone();
        let view = cx.entity().downgrade();
        let (empty, danger, muted) = {
            let (message, danger) = empty_state(&catalog, loading, error.as_deref());
            (message, danger, cx.theme().muted_foreground)
        };
        let empty_color = if danger { cx.theme().danger } else { muted };
        let (notice, notice_danger) = notice(&catalog, matched, error.as_deref());
        let notice_color = if notice_danger {
            cx.theme().danger
        } else {
            muted
        };

        let mut command = Command::new(&self.state)
            .placeholder(
                "Search columns, indexes, routines…   kind:column  table:orders  type:uuid",
            )
            .bordered(false)
            .filterable(false)
            .max_h(px(420.))
            .empty(move |_, _, _cx| {
                div()
                    .p_4()
                    .text_center()
                    .text_sm()
                    .text_color(empty_color)
                    .child(empty.clone())
            })
            .on_query(move |query, _window, cx| {
                let query = query.to_string();
                if let Some(view) = view.upgrade() {
                    view.update(cx, |this, cx| {
                        this.query = query;
                        this.refresh_search(cx);
                    });
                }
            })
            .on_confirm(move |index_path, window, cx| {
                if let Some(entry) = targets
                    .get(index_path.section)
                    .and_then(|section| section.get(index_path.row))
                    .cloned()
                {
                    session.update(cx, |session, cx| {
                        session.open_catalog_entry(entry, &mut *window, cx)
                    });
                }
                window.close_dialog(cx);
            })
            .on_cancel(move |window, cx| window.close_dialog(cx));

        for group in groups {
            command = command.group(group);
        }

        v_flex()
            .size_full()
            .child(
                div()
                    .p_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(self.render_chips(cx)),
            )
            .child(command)
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(notice_color)
                    .child(notice),
            )
    }
}

/// One row: the icon, the name, the owner and detail, and the kind in words.
///
/// The kind word is what makes the row readable without the icon, and the
/// owner line is what tells two `id` columns apart.
fn item_for(entry: &CatalogEntry) -> CommandItem {
    let icon = icon_for(entry.kind);
    let name = entry.label();
    let mut meta = entry.owner_label().unwrap_or_default();
    if !entry.detail.is_empty() {
        if meta.is_empty() {
            meta = entry.detail.clone();
        } else {
            meta = format!("{meta} · {}", entry.detail);
        }
    }
    let kind = entry.kind_label();
    let muted = |cx: &App| cx.theme().muted_foreground;

    CommandItem::new()
        .label(name.clone())
        .child(move |_window, cx| {
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .child(Icon::new(icon).size_4().flex_none().text_color(muted(cx)))
                .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                .when(!meta.is_empty(), |this| {
                    this.child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(muted(cx))
                            .child(meta.clone()),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(muted(cx))
                        .child(kind),
                )
        })
}

fn icon_for(kind: CatalogKind) -> IconName {
    match kind {
        CatalogKind::Table => IconName::Table,
        CatalogKind::View => IconName::Eye,
        CatalogKind::Column => IconName::Columns3,
        CatalogKind::Index => IconName::Key,
        CatalogKind::Routine => IconName::SquareFunction,
        CatalogKind::Trigger => IconName::Zap,
    }
}

/// What the empty list says: still loading, an error, or nothing matched.
fn empty_state(catalog: &Catalog, loading: bool, error: Option<&str>) -> (String, bool) {
    if let Some(error) = error {
        return (format!("Error: {error}"), true);
    }
    if loading || catalog.entries.is_empty() {
        return ("Loading schema…".to_string(), false);
    }
    ("No matches".to_string(), false)
}

/// The line under the list: an error, how many matched, and any cap that hid
/// something.
fn notice(catalog: &Catalog, matches: usize, error: Option<&str>) -> (String, bool) {
    let mut notes = Vec::new();
    if let Some(error) = error {
        notes.push(format!("Error: {error} — showing names only"));
    }
    notes.push(match matches {
        1 => "1 match".to_string(),
        count => format!("{count} matches"),
    });
    if matches > MAX_HITS {
        notes.push(format!("showing the first {MAX_HITS}"));
    }
    if let Some(total) = catalog.truncated() {
        notes.push(format!(
            "Showing first {} of {} entries.",
            catalog.entries.len(),
            total
        ));
    }
    (notes.join(" · "), error.is_some())
}
