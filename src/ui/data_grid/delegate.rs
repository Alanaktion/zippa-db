//! The table's view of a result, and the overlays staged on top of it.
//!
//! [`ResultDelegate`] is what the table component asks for rows, columns, and
//! cells. Beside the result it holds everything the user has done to it but
//! not written yet — typed cells, rows being built by hand, rows marked for
//! deletion, the row-level selection — so the grid itself stays a thin view
//! over one place that knows what a cell currently says.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Div, Entity, Focusable as _, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent,
    Pixels, SharedString, Stateful, Window, div, px,
};

use crate::db::export::Format;
use crate::db::query::{self, Cell, QueryResult};
use crate::settings::Settings;

use super::layout::MIN_COLUMN_WIDTH;

use super::{
    ChangeReporter, CopyReporter, CopyValue, ExportReporter, NavReporter, SortReporter, Sorting,
    ViewReporter,
};

pub(super) struct ResultDelegate {
    pub(super) result: QueryResult,
    /// Row indices in display order. Sorting reorders this rather than the
    /// rows themselves, so the order the rows arrived in is never lost.
    pub(super) order: Vec<usize>,
    pub(super) widths: Vec<Pixels>,
    /// Row the keyboard is on — the one holding the selected cell — so the
    /// whole row can be marked while the selection itself stays on one column.
    /// Kept apart from [`Self::rows_selected`], which is the set a row command
    /// acts on: focus is where the next keystroke lands, selection is what it
    /// would be about.
    pub(super) focused_row: Option<usize>,
    pub(super) font: SharedString,
    pub(super) sorting: Sorting,
    /// Column the rows are sorted by, for the header arrow. Held by name, so
    /// it survives a result whose columns moved.
    pub(super) sorted_by: Option<(String, ColumnSort)>,
    pub(super) report_sort: SortReporter,
    /// Values typed into cells but not written yet, keyed by the row's index
    /// into the result and the column. Absent means unchanged.
    pub(super) edits: HashMap<(usize, usize), Cell>,
    /// Whether the owner can write this result back at all.
    pub(super) editable: bool,
    /// Rows picked out for a row-level action, in display coordinates. This
    /// is the grid's own selection, separate from the table's selected cell.
    pub(super) rows_selected: HashSet<usize>,
    /// Row a sweep started on, so a move can select the range between.
    pub(super) anchor: Option<usize>,
    /// Set while the pointer is dragging across the pick boxes: `true` while
    /// the drag is picking rows out, `false` while it is putting them back.
    /// `None` between drags, so a stray move with a button held does nothing.
    pub(super) sweeping: Option<bool>,
    /// Row the last right click landed on, for the menu the grid opens.
    pub(super) menu_row: Option<usize>,
    /// Cell it landed on, when it landed on one at all.
    pub(super) menu_cell: Option<(usize, usize)>,
    /// Rows marked for deletion, by their index into the result. They stay on
    /// screen, struck through, until the owner writes them away.
    pub(super) deletions: HashSet<usize>,
    pub(super) report_change: ChangeReporter,
    pub(super) report_view: ViewReporter,
    pub(super) report_navigate: NavReporter,
    pub(super) report_copy: CopyReporter,
    /// Asks the owner to export the picked rows in a format.
    pub(super) report_export: ExportReporter,
    /// Whether the owner can export the picked rows at all. Only a table view
    /// can, since only it knows which table the rows came from; a query
    /// result's grid leaves this false so its menu offers no dead command.
    pub(super) exportable: bool,
    /// Column indices with a usable single-column foreign key, set by the
    /// table view once it has read the table's own schema. Empty for an
    /// ad-hoc query result, which has no owner that could look one up.
    pub(super) foreign_keys: HashSet<usize>,
    /// Rows the user is building by hand, each one the cells typed into it so
    /// far. They sit after the result's own rows and are written by an
    /// `INSERT`, so a column nobody typed into is left out and takes whatever
    /// default the server has for it.
    pub(super) drafts: Vec<HashMap<usize, Cell>>,
    /// Cell the text editor is open on, in display coordinates.
    pub(super) editing: Option<(usize, usize)>,
    /// The editor itself, shared with the grid so it can be focused and read.
    pub(super) editor: Entity<InputState>,
}

/// Stands in for a cell a short row does not have.
static MISSING: Cell = None;

/// The cell at `row_ix`/`col_ix`, or `NULL` where the row is short.
fn cell_at(rows: &[Vec<Cell>], row_ix: usize, col_ix: usize) -> &Cell {
    rows.get(row_ix)
        .and_then(|row| row.get(col_ix))
        .unwrap_or(&MISSING)
}

/// Compare two cells: numbers numerically, everything else as text, NULLs last.
fn compare(left: &Cell, right: &Cell) -> Ordering {
    match (left, right) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(left), Some(right)) => match (left.parse::<f64>(), right.parse::<f64>()) {
            (Ok(left), Ok(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
            _ => left.cmp(right),
        },
    }
}

/// The column of checkboxes the table draws before the result's own.
const PICK_COLUMN: usize = 0;

/// Width of that column.
const PICK_WIDTH: f32 = 34.;

/// Width of the bar down the leading edge of the row the keyboard is on.
const FOCUS_BAR_WIDTH: f32 = 2.;

/// How much of the selection tint has to be there for a row to read as marked.
const MIN_SELECTION_ALPHA: f32 = 0.25;

/// Keeps the table from marking the cell a right click landed on.
///
/// The table outlines that cell while its own menu is up and clears the mark
/// when the next click lands outside — but it only registers that click-out
/// handler for a right click on a *row*, so a right click on a cell leaves the
/// outline behind for good. The grid's menu is its own (see `row_menu`) and
/// does not need the table's mark at all, so the click is stopped before the
/// table's handler sees it: that handler stops the event itself, one listener
/// later, and the grid's own menu — whose listener sits at the root and runs
/// before either of them — has already opened by then.
fn stop_right_click(_: &MouseDownEvent, _: &mut Window, cx: &mut App) {
    cx.stop_propagation();
}

/// A theme is free to make its table-selection tint nearly transparent — the
/// bundled dark themes that set no table colours inherit a list tint of about
/// an eighth — and over a dark background that leaves a picked row looking
/// like every other one. The hue stays the theme's; only the alpha is raised.
fn readable(tint: Hsla) -> Hsla {
    Hsla {
        a: tint.a.max(MIN_SELECTION_ALPHA),
        ..tint
    }
}

impl TableDelegate for ResultDelegate {
    fn columns_count(&self, _: &App) -> usize {
        // The checkbox column sits in front of the result's own columns.
        self.result.columns.len() + 1
    }

    fn rows_count(&self, _: &App) -> usize {
        // The rows being built by hand sit after the ones the server sent.
        self.rows()
    }

    fn column(&self, col_ix: usize, _: &App) -> Column {
        let Some(data_ix) = self.data_column(col_ix) else {
            // Picking rows is not a value, so its column neither sorts nor
            // resizes.
            return Column::new("", "").width(px(PICK_WIDTH)).resizable(false);
        };

        let name = self
            .result
            .columns
            .get(data_ix)
            .cloned()
            .unwrap_or_default();
        let width = self
            .widths
            .get(data_ix)
            .copied()
            .unwrap_or(px(MIN_COLUMN_WIDTH));

        let sort = match &self.sorted_by {
            Some((sorted, sort)) if *sorted == name => *sort,
            _ => ColumnSort::Default,
        };

        Column::new(name.clone(), name)
            .width(width)
            .resizable(true)
            .sortable()
            .sort(sort)
    }

    /// Answer a header click.
    ///
    /// The table cycles a column through descending, ascending, and back to
    /// unsorted, and hands the state it settled on here.
    fn perform_sort(
        &mut self,
        col_ix: usize,
        sort: ColumnSort,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(data_ix) = self.data_column(col_ix) else {
            return;
        };
        let Some(column) = self.result.columns.get(data_ix).cloned() else {
            return;
        };

        self.sorted_by = match sort {
            ColumnSort::Default => None,
            sort => Some((column.clone(), sort)),
        };

        match self.sorting {
            Sorting::InPlace => {
                self.reorder(data_ix, sort);
                cx.notify();
            }
            Sorting::Delegated => {
                let report = self.report_sort.clone();
                cx.defer(move |cx| report(column, sort, cx));
            }
        }
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> Stateful<Div> {
        let picked = self.rows_selected.contains(&row_ix);
        let draft = self.draft(row_ix).is_some();
        let deleted = self.is_deleted(row_ix);
        let focused = self.focused_row == Some(row_ix);

        // A row's own background is the table's, with the stripe over it on
        // every other row. The table paints that itself, but a background set
        // here *replaces* it rather than covering it, so the tints below are
        // blended onto it by hand: a translucent tint painted straight on
        // would drop the stripe and end up reading as an unmarked row.
        let theme = cx.theme();
        let accent = readable(*theme.tokens.table_active);
        let border = theme.table_active_border;
        let (danger, info) = (theme.danger, theme.info);
        let mut backdrop = *theme.tokens.table;
        if Settings::global(cx).stripe_rows && !row_ix.is_multiple_of(2) {
            backdrop = backdrop.blend(*theme.tokens.table_even);
        }

        // Strongest state first: picked out for a row command, then what will
        // happen to the row — blue for one being added, red for one being
        // deleted. An edited cell carries its own green, since the rest of the
        // row is untouched.
        let background = if picked {
            Some(backdrop.blend(accent))
        } else if deleted {
            Some(backdrop.blend(danger.opacity(0.15)))
        } else if draft {
            Some(backdrop.blend(info.opacity(0.15)))
        } else if focused {
            Some(backdrop.blend(accent.opacity(0.45)))
        } else {
            None
        };

        div()
            .id(("row", row_ix))
            .relative()
            .when_some(background, |this, background| this.bg(background))
            // Where the keyboard is, told by a bar down the row's leading edge
            // rather than by its tint alone. A theme is free to make the
            // selection tint as faint as it likes — several make it barely
            // visible — and the row the arrows are on has to be findable in
            // all of them, as well as tellable from the rows picked out for a
            // command, whose boxes are checked.
            .when(focused, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(FOCUS_BAR_WIDTH))
                        .bg(border),
                )
            })
            // The table answers a right click on a cell itself and stops it
            // there, so the row this one landed on is noted in the capture
            // phase, before that happens. The menu the grid opens reads it.
            // Picking rows with the pointer rides along here for the same
            // reason: the table stops a click on a cell before it bubbles.
            .capture_any_mouse_down(cx.listener(move |table, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Right {
                    let delegate = table.delegate_mut();
                    delegate.menu_row = Some(row_ix);
                    // The cell is filled in by the cell's own handler, which
                    // runs after this one. A click beside the cells — the row
                    // header, the space after the last column — leaves it
                    // empty, so the menu is about the row alone rather than
                    // about whatever was clicked last time.
                    delegate.menu_cell = None;
                    return;
                }

                if event.button != MouseButton::Left {
                    return;
                }

                // Shift takes the range from the last row picked, and the
                // platform's own modifier picks one row out without losing the
                // others — the way a file manager does, anywhere in the row
                // rather than only on its box.
                if event.modifiers.shift {
                    table.delegate_mut().pick(row_ix, true);
                    cx.notify();
                } else if event.modifiers.secondary() {
                    table.delegate_mut().pick(row_ix, false);
                    cx.notify();
                }
            }))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(col_ix) = self.data_column(col_ix) else {
            return self.render_pick(row_ix, cx).into_any_element();
        };

        if self.editing == Some((row_ix, col_ix)) {
            // The table paints its selected-cell tint over this, so the editor
            // carries its own background to stay readable underneath it.
            return div()
                .size_full()
                .bg(cx.theme().background)
                .child(
                    Input::new(&self.editor)
                        .id(("cell-editor", col_ix))
                        .xsmall()
                        .appearance(false)
                        .bordered(false)
                        .font_family(self.font.clone()),
                )
                .into_any_element();
        }

        // The menu is built on the frame after the click, but an event from
        // the table only reaches its owner after that, so the cell the menu is
        // about is noted here — in the capture phase, before the table answers
        // the click itself.
        let cell = div()
            .font_family(self.font.clone())
            .text_xs()
            .capture_any_mouse_down(cx.listener(move |table, event: &MouseDownEvent, _, _cx| {
                if event.button == MouseButton::Right {
                    table.delegate_mut().menu_cell = Some((row_ix, col_ix));
                }
            }))
            .on_mouse_down(MouseButton::Right, stop_right_click);
        let deleted = self.is_deleted(row_ix);
        let cell = if deleted {
            // A deleted row is going whatever its cells hold, so the value is
            // shown as the server still has it, crossed out.
            cell.line_through().text_color(cx.theme().muted_foreground)
        } else if self.is_staged(row_ix, col_ix) && self.draft(row_ix).is_none() {
            // Underlined as well as tinted: the tint alone is the only thing
            // saying this cell is not what the server has, and a tint is not
            // something everyone can see.
            cell.bg(cx.theme().success.opacity(0.2)).underline()
        } else {
            cell
        };

        // A draft cell nobody has typed into is not NULL: it is whatever the
        // server puts there, so it says so rather than promising a value.
        if self.is_untouched_draft(row_ix, col_ix) {
            return cell
                .text_color(cx.theme().muted_foreground)
                .child("default")
                .into_any_element();
        }

        match self.cell(row_ix, col_ix) {
            Some(value) => cell.child(value.to_string()).into_any_element(),
            None => cell
                .text_color(cx.theme().muted_foreground)
                .child("NULL")
                .into_any_element(),
        }
    }

    fn cell_text(&self, row_ix: usize, col_ix: usize, _: &App) -> String {
        match self.data_column(col_ix) {
            Some(col_ix) => self.cell(row_ix, col_ix).clone().unwrap_or_default(),
            None => String::new(),
        }
    }

    /// The header of the checkbox column picks every row at once.
    fn render_th(
        &mut self,
        col_ix: usize,
        _: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        if self.data_column(col_ix).is_some() {
            return div()
                .size_full()
                .child(self.column(col_ix, cx).name.clone())
                .into_any_element();
        }

        let rows = self.rows_count(cx);
        let all = rows > 0 && self.rows_selected.len() == rows;

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                Checkbox::new("pick-all")
                    .accessibility_label("Select every row")
                    .checked(all)
                    .on_click(cx.listener(move |table, checked: &bool, window, cx| {
                        table.delegate_mut().pick_all(*checked, rows);
                        cx.notify();
                        // Like the per-row boxes: the keyboard belongs to the
                        // rows, not to the box that was clicked.
                        table.focus_handle(cx).focus(window, cx);
                    })),
            )
            .into_any_element()
    }
}

/// Stands in for a cell whose row the result does not have.
static ABSENT: Cell = None;

impl ResultDelegate {
    /// Index into the result's rows of the row shown at `row_ix`.
    ///
    /// `None` for a draft row, which the server has never seen.
    pub(super) fn source(&self, row_ix: usize) -> Option<usize> {
        self.order.get(row_ix).copied()
    }

    /// Index into `drafts` of the row shown at `row_ix`, if it is one.
    pub(super) fn draft(&self, row_ix: usize) -> Option<usize> {
        row_ix
            .checked_sub(self.order.len())
            .filter(|index| *index < self.drafts.len())
    }

    /// The value as it came from the server.
    pub(super) fn baseline(&self, row_ix: usize, col_ix: usize) -> &Cell {
        let Some(row_ix) = self.source(row_ix) else {
            return &ABSENT;
        };
        cell_at(&self.result.rows, row_ix, col_ix)
    }

    /// The value as it stands, staged edit included.
    pub(super) fn cell(&self, row_ix: usize, col_ix: usize) -> &Cell {
        if let Some(draft) = self.draft(row_ix) {
            return self.drafts[draft].get(&col_ix).unwrap_or(&ABSENT);
        }

        match self
            .source(row_ix)
            .and_then(|row_ix| self.edits.get(&(row_ix, col_ix)))
        {
            Some(staged) => staged,
            None => self.baseline(row_ix, col_ix),
        }
    }

    /// Whether a draft row has nothing typed into this cell yet, so the
    /// server's own default is what it would be written with.
    pub(super) fn is_untouched_draft(&self, row_ix: usize, col_ix: usize) -> bool {
        self.draft(row_ix)
            .is_some_and(|draft| !self.drafts[draft].contains_key(&col_ix))
    }

    /// The items a right click on `row_ix` offers.
    ///
    /// Right-clicking a row outside the sweep acts on that row alone, the way
    /// a file manager does. An empty menu is never shown, so a grid that
    /// cannot be written to simply has none.
    ///
    /// This is deliberately not `TableDelegate::context_menu`: the table opens
    /// a menu of its own for right clicks it handles itself, and leaving that
    /// one empty is what keeps two menus from appearing at once.
    pub(super) fn row_menu_items(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        // Looking at a value is not editing it, so this one is offered even on
        // a read-only grid — and on a query result, which has no owner that
        // could write it back.
        // Copying rows is what the keystroke does while rows are picked out,
        // so that is the item the shortcut is shown beside.
        let picked = self.rows_selected.len();
        let copies_rows = picked > 0 && self.rows_selected.contains(&row_ix);

        let menu = match self.menu_cell.filter(|(row, _)| *row == row_ix) {
            Some((row, col)) => {
                let report = self.report_view.clone();
                let menu = menu.item(
                    PopupMenuItem::new("View value")
                        .on_click(move |_, _window, cx| report(row, col, cx)),
                );

                let report = self.report_copy.clone();
                let menu = menu.item({
                    let item = PopupMenuItem::new("Copy value")
                        .on_click(move |_, _window, cx| report(Some((row, col)), cx));
                    match copies_rows {
                        true => item,
                        false => item.action(Box::new(CopyValue)),
                    }
                });

                // Nothing to jump to for a NULL foreign key value.
                if self.foreign_keys.contains(&col) && self.cell(row, col).is_some() {
                    let report = self.report_navigate.clone();
                    menu.item(
                        PopupMenuItem::new("Go to referenced row")
                            .on_click(move |_, _window, cx| report(row, col, cx)),
                    )
                } else {
                    menu
                }
            }
            None => menu,
        };

        let menu = if copies_rows {
            let label = match picked {
                1 => "Copy row".to_string(),
                rows => format!("Copy {rows} rows"),
            };
            let report = self.report_copy.clone();
            menu.item(
                PopupMenuItem::new(label)
                    .action(Box::new(CopyValue))
                    .on_click(move |_, _window, cx| report(None, cx)),
            )
        } else {
            menu
        };

        // Exporting is a read, so it is offered on a read-only grid too. The
        // formats open a save dialog, hence the ellipsis on the submenu that
        // holds them. A query result's grid has no owner that knows a table
        // name, and sets `exportable` false so it offers none of this.
        let menu = if self.exportable && copies_rows {
            let label = match picked {
                1 => "Export row…".to_string(),
                rows => format!("Export {rows} rows…"),
            };
            let report = self.report_export.clone();
            let submenu = PopupMenu::build(window, cx, move |menu, _window, _cx| {
                Format::ALL.into_iter().fold(menu, |menu, format| {
                    let report = report.clone();
                    menu.item(
                        PopupMenuItem::new(format.label())
                            .on_click(move |_, _window, cx| report(format, cx)),
                    )
                })
            });
            menu.item(PopupMenuItem::submenu(label, submenu))
        } else {
            menu
        };

        if !self.editable {
            return menu;
        }

        // A draft row has nothing on the server to delete, so the menu drops
        // the row itself instead.
        if let Some(draft) = self.draft(row_ix) {
            let table = cx.weak_entity();
            return menu.item(PopupMenuItem::new("Discard new row").on_click(
                move |_, _window, cx| {
                    let Some(table) = table.upgrade() else {
                        return;
                    };
                    table.update(cx, |table, cx| {
                        table.delegate_mut().discard_draft(draft);
                        table.refresh(cx);
                    });
                },
            ));
        }

        let deleted = self.is_deleted(row_ix);
        let rows = self.mark_for_menu(row_ix);
        let label = match (deleted, rows) {
            (false, 1) => "Delete row".to_string(),
            (false, rows) => format!("Delete {rows} rows"),
            (true, 1) => "Restore row".to_string(),
            (true, rows) => format!("Restore {rows} rows"),
        };

        let table = cx.weak_entity();
        let report = self.report_change.clone();
        menu.item(PopupMenuItem::new(label).on_click(move |_, _window, cx| {
            let Some(table) = table.upgrade() else {
                return;
            };
            table.update(cx, |table, cx| {
                table.delegate_mut().set_deleted(!deleted);
                cx.notify();
            });
            report(cx);
        }))
    }

    /// Whether the row shown at `row_ix` is marked for deletion.
    pub(super) fn is_deleted(&self, row_ix: usize) -> bool {
        self.source(row_ix)
            .is_some_and(|source| self.deletions.contains(&source))
    }

    /// Mark every swept row for deletion, or take the mark off again.
    ///
    /// A row being built by hand is not in the result, so it is dropped
    /// outright rather than marked. That is what the row menu's "Discard new
    /// row" does, and doing it here too is what gives the menu item a
    /// keystroke.
    pub(super) fn set_deleted(&mut self, deleted: bool) {
        let rows: Vec<usize> = self.rows_selected.iter().copied().collect();
        for row_ix in &rows {
            let Some(source) = self.source(*row_ix) else {
                continue;
            };
            if deleted {
                self.deletions.insert(source);
            } else {
                self.deletions.remove(&source);
            }
        }

        if !deleted {
            return;
        }

        // Highest first: dropping one moves the ones after it along.
        let mut drafts: Vec<usize> = rows.iter().filter_map(|row| self.draft(*row)).collect();
        drafts.sort_unstable();
        for draft in drafts.into_iter().rev() {
            self.discard_draft(draft);
        }

        // The rows that are gone cannot stay selected: their positions now
        // belong to whatever moved up into them.
        let remaining = self.rows();
        self.rows_selected.retain(|row| *row < remaining);
        self.anchor = self.anchor.filter(|row| *row < remaining);
    }

    /// Drop the row being built at `draft`, leaving the others alone.
    pub(super) fn discard_draft(&mut self, draft: usize) {
        if draft < self.drafts.len() {
            self.drafts.remove(draft);
        }
    }

    /// Take `row_ix` into the selection if it is outside it, and say how many
    /// rows the menu is then about.
    pub(super) fn mark_for_menu(&mut self, row_ix: usize) -> usize {
        if !self.rows_selected.contains(&row_ix) {
            self.rows_selected = HashSet::from([row_ix]);
            self.anchor = Some(row_ix);
        }
        self.rows_selected.len()
    }

    /// Pick `row_ix` out, or put it back; shift takes the rows between it and
    /// the last one picked.
    pub(super) fn pick(&mut self, row_ix: usize, extend: bool) {
        if extend && let Some(anchor) = self.anchor {
            let (first, last) = if anchor <= row_ix {
                (anchor, row_ix)
            } else {
                (row_ix, anchor)
            };
            self.rows_selected.extend(first..=last);
            return;
        }

        if !self.rows_selected.remove(&row_ix) {
            self.rows_selected.insert(row_ix);
        }
        self.anchor = Some(row_ix);
    }

    /// Start a range at `row_ix`: pick it out, without putting it back if it
    /// was already picked, and make it the anchor the range grows from.
    pub(super) fn anchor_at(&mut self, row_ix: usize) {
        self.rows_selected.insert(row_ix);
        self.anchor = Some(row_ix);
    }

    /// Take the rows between the anchor and `row_ix` into the selection, or
    /// out of it, as a drag across the pick boxes passes over them.
    pub(super) fn sweep_to(&mut self, row_ix: usize, adding: bool) {
        let Some(anchor) = self.anchor else {
            return;
        };
        let (first, last) = if anchor <= row_ix {
            (anchor, row_ix)
        } else {
            (row_ix, anchor)
        };

        for row in first..=last {
            if adding {
                self.rows_selected.insert(row);
            } else {
                self.rows_selected.remove(&row);
            }
        }
    }

    /// Pick every row out, or put them all back.
    pub(super) fn pick_all(&mut self, picked: bool, rows: usize) {
        self.rows_selected = match picked {
            true => (0..rows).collect(),
            false => HashSet::new(),
        };
        self.anchor = None;
    }

    /// How many rows the grid is showing, rows being built by hand included.
    pub(super) fn rows(&self) -> usize {
        self.order.len() + self.drafts.len()
    }

    /// The checkbox that picks one row out.
    ///
    /// The box is not a tab stop: a result has as many of them as it has rows,
    /// and tabbing through a page of them to reach the grid is worse than the
    /// keys that do the same job — space on the focused row, `secondary-a` for
    /// all of them. Clicking one hands the focus back to the table, so the
    /// next keystroke still goes to the rows rather than to a box.
    pub(super) fn render_pick(
        &self,
        row_ix: usize,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let picked = self.rows_selected.contains(&row_ix);

        div()
            .id(("pick-cell", row_ix))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            // Pressing a box starts a sweep, in the direction the box itself
            // is about to go: a drag that began on an unpicked row picks the
            // rows it passes out, and one that began on a picked row puts
            // them back. The press only notes where the drag started — the
            // box's own click is still what answers a plain click, and a drag
            // that ends on another row never produces one.
            .capture_any_mouse_down(cx.listener(
                move |table, event: &MouseDownEvent, _window, _cx| {
                    if event.button != MouseButton::Left
                        || event.modifiers.shift
                        || event.modifiers.secondary()
                    {
                        return;
                    }

                    let picked = table.delegate().rows_selected.contains(&row_ix);
                    let delegate = table.delegate_mut();
                    delegate.anchor = Some(row_ix);
                    delegate.sweeping = Some(!picked);
                },
            ))
            // Dragging down the boxes sweeps a range, which is how a run of
            // rows is picked out without counting them first.
            .on_mouse_down(MouseButton::Right, stop_right_click)
            .on_mouse_move(
                cx.listener(move |table, event: &MouseMoveEvent, _window, cx| {
                    if event.pressed_button != Some(MouseButton::Left) {
                        return;
                    }
                    let Some(adding) = table.delegate().sweeping else {
                        return;
                    };
                    table.delegate_mut().sweep_to(row_ix, adding);
                    cx.notify();
                }),
            )
            .child(
                Checkbox::new(("pick", row_ix))
                    .accessibility_label(format!("Select row {}", row_ix + 1))
                    .tab_stop(false)
                    .checked(picked)
                    .on_click(cx.listener(move |table, _checked: &bool, window, cx| {
                        let modifiers = window.modifiers();
                        // The row's own capture handler has already answered
                        // this one; toggling again here would undo it.
                        if modifiers.secondary() {
                            return;
                        }

                        table.delegate_mut().pick(row_ix, modifiers.shift);
                        cx.notify();

                        // The box took the focus when it was pressed; the rows
                        // are where the keyboard belongs.
                        table.focus_handle(cx).focus(window, cx);
                    })),
            )
    }

    /// Index into the result's columns of the column shown at `col_ix`.
    ///
    /// `None` for the checkbox column, which belongs to no value.
    pub(super) fn data_column(&self, col_ix: usize) -> Option<usize> {
        col_ix.checked_sub(PICK_COLUMN + 1)
    }

    /// Where the column at `data_ix` is shown.
    pub(super) fn shown_column(data_ix: usize) -> usize {
        data_ix + PICK_COLUMN + 1
    }

    pub(super) fn is_staged(&self, row_ix: usize, col_ix: usize) -> bool {
        if let Some(draft) = self.draft(row_ix) {
            return self.drafts[draft].contains_key(&col_ix);
        }
        self.source(row_ix)
            .is_some_and(|row_ix| self.edits.contains_key(&(row_ix, col_ix)))
    }

    /// Whether this cell can be typed into.
    ///
    /// A binary column and a value the driver could only describe (`<3 bytes>`,
    /// `<XML>`) are shown but not held, so writing one back would lose it.
    pub(super) fn is_editable(&self, row_ix: usize, col_ix: usize) -> bool {
        if !self.editable {
            return false;
        }

        let binary = self
            .result
            .column_types
            .get(col_ix)
            .is_some_and(|name| query::is_binary_type(name));
        if binary {
            return false;
        }

        // A draft row has no value to lose, so only the column's type can
        // stand in the way.
        if self.draft(row_ix).is_some() {
            return true;
        }

        // A row on its way out is not worth typing into.
        if self.is_deleted(row_ix) {
            return false;
        }

        self.source(row_ix).is_some() && !query::is_placeholder(self.baseline(row_ix, col_ix))
    }

    /// Stage `value` on a cell, or drop the edit when it matches the row as
    /// loaded, so typing a value back the way it was leaves nothing to write.
    pub(super) fn stage(&mut self, row_ix: usize, col_ix: usize, value: Cell) {
        // A draft has no loaded value behind it: what is typed is what the
        // `INSERT` carries, and typing nothing leaves the column out of it.
        if let Some(draft) = self.draft(row_ix) {
            self.drafts[draft].insert(col_ix, value);
            return;
        }

        let Some(source) = self.source(row_ix) else {
            return;
        };
        if self.baseline(row_ix, col_ix) == &value {
            self.edits.remove(&(source, col_ix));
            return;
        }
        self.edits.insert((source, col_ix), value);
    }

    /// Put the display order where `sort` asks for; `Default` restores the
    /// order the server sent.
    pub(super) fn reorder(&mut self, col_ix: usize, sort: ColumnSort) {
        let mut order: Vec<usize> = (0..self.result.rows.len()).collect();

        if sort != ColumnSort::Default {
            let rows = &self.result.rows;
            order.sort_by(|&left, &right| {
                let ordering = compare(cell_at(rows, left, col_ix), cell_at(rows, right, col_ix));
                match sort {
                    ColumnSort::Descending => ordering.reverse(),
                    _ => ordering,
                }
            });
        }

        self.order = order;
    }
}
