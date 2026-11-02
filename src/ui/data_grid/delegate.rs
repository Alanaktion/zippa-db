//! The table's view of a result, and the overlays staged on top of it.
//!
//! [`ResultDelegate`] is what the table component asks for rows, columns, and
//! cells. Beside the result it holds everything the user has done to it but
//! not written yet — typed cells, rows being built by hand, rows marked for
//! deletion, the row-level selection — so the grid itself stays a thin view
//! over one place that knows what a cell currently says.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use gpui_kit::assets::IconName;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::table::{Column, ColumnSort, TableDelegate, TableState};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Size, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Div, Entity, Focusable as _, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent,
    Pixels, SharedString, Stateful, Window, div, px,
};

use crate::db::Engine;
use crate::db::export::Format;
use crate::db::query::{self, Cell, QueryResult};
use crate::settings::Settings;

use super::layout::MIN_COLUMN_WIDTH;

use super::{
    ChangeReporter, CommandReporter, CopyAs, CopyAsReporter, CopyReporter, CopyValue,
    CopyWithHeaders, ExportReporter, MenuCommand, NavReporter, SortReporter, Sorting, ViewCell,
    ViewReporter,
};
use crate::ui::table_view::{EditCell, SetNull};

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
    /// The result column the table's selected cell was in when the click
    /// landed: what the menu's column copies fall back to when the click was
    /// beside the cells rather than on one.
    pub(super) menu_column: Option<usize>,
    /// Rows marked for deletion, by their index into the result. They stay on
    /// screen, struck through, until the owner writes them away.
    pub(super) deletions: HashSet<usize>,
    pub(super) report_change: ChangeReporter,
    pub(super) report_view: ViewReporter,
    pub(super) report_navigate: NavReporter,
    pub(super) report_copy: CopyReporter,
    /// Asks the grid to copy what the menu is about, in a chosen shape.
    pub(super) report_copy_as: CopyAsReporter,
    /// Asks the owner to export the picked rows in a format.
    pub(super) report_export: ExportReporter,
    /// Runs a cell or row command from the menu.
    pub(super) report_command: CommandReporter,
    /// The grid's result generation this delegate's rows belong to, so a
    /// menu built over one page does not act on the page that replaced it.
    pub(super) generation: u64,
    /// The engine a copied value is quoted for.
    pub(super) engine: Engine,
    /// The table a SQL `INSERT` copy names, set by the table view. A query
    /// result's grid leaves this `None`, so its menu offers no SQL copy and no
    /// export.
    pub(super) table: Option<String>,
    /// Column indices with a usable single-column foreign key, set by the
    /// table view once it has read the table's own schema. Empty for an
    /// ad-hoc query result, which has no owner that could look one up.
    pub(super) foreign_keys: HashSet<usize>,
    /// What the table's schema says about each column, by name, for the
    /// headers. Empty for a query result.
    pub(super) column_info: super::ColumnInfos,
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

/// Stands in for a cell a row does not have: past the end of a short row, or
/// a column nobody typed into on a draft.
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

/// The grid's right-click menu, as a cell or a pick box carries it.
///
/// Each cell carries the menu itself rather than leaving it to an element
/// around the table: a context menu listens before the elements inside it
/// paint, so the innermost one answers first, and the table answers a right
/// click on a cell by stopping it there. Being innermost, the cell's menu also
/// stops the click before the table sees it, so the table never outlines the
/// cell. Built when the menu opens, so it is about the row the click noted.
fn grid_menu(
    cx: &mut Context<TableState<ResultDelegate>>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let table = cx.entity().downgrade();
    move |menu, window, cx| {
        let Some(table) = table.upgrade() else {
            return menu;
        };
        table.update(cx, |table, cx| table.delegate_mut().menu(menu, window, cx))
    }
}

/// The padding the table gives an `xsmall` cell.
///
/// Every column is built with none (`Column::p_0`) and the delegate's own
/// cells and headers carry this instead, so the whole cell is the element
/// that holds the grid's menu: left to the table, a right click in the
/// padding lands on the table's own cell, which stops it before any menu.
fn cell_padding() -> gpui_kit::Edges<Pixels> {
    Size::XSmall.table_cell_padding()
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
    /// A right click on a row beside its cells — past the last column — is
    /// the table's own to answer; the cells and pick boxes carry the same menu
    /// themselves (`grid_menu`).
    fn context_menu(
        &mut self,
        _row_ix: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        self.menu(menu, window, cx)
    }

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
            return Column::new("", "")
                .width(px(PICK_WIDTH))
                .resizable(false)
                .p_0();
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
            .p_0()
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
            // A right click on a cell is stopped by the cell's own menu, so
            // the row it landed on is noted in the capture phase, before that
            // happens. The menu (`grid_menu`) reads it.
            // Picking rows with the pointer rides along here for the same
            // reason: the table stops a click on a cell before it bubbles.
            .capture_any_mouse_down(cx.listener(move |table, event: &MouseDownEvent, _, cx| {
                if event.button == MouseButton::Right {
                    let selected_column = table
                        .selected_cell()
                        .and_then(|(_, col)| table.delegate().data_column(col));
                    let delegate = table.delegate_mut();
                    delegate.menu_row = Some(row_ix);
                    delegate.menu_column = selected_column;
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
                } else if event.modifiers.secondary(cx) {
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
            let pad = cell_padding();
            return div()
                .size_full()
                .px(pad.left)
                .py(pad.top)
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

        // Clipped text ends in an ellipsis, so a value that runs on past the
        // column edge does not look like one that stops there; numbers line
        // up on their last digit so magnitudes can be compared down a column.
        let numeric = self.is_numeric_column(col_ix);
        let cell = div()
            .w_full()
            .min_w_0()
            .truncate()
            // The gap keeps a right-aligned number off the next column's text.
            .when(numeric, |cell| cell.text_right().pr_2())
            .font_family(self.font.clone())
            .text_xs();
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
        } else if self.is_staged(row_ix, col_ix) {
            // A value typed into a new row keeps the row's blue rather than
            // turning green, but it is underlined like any other staged value:
            // with the word "default" in the cells nobody typed into, every
            // cell of a new row says what it is without its colour.
            cell.underline()
        } else {
            cell
        };

        let content = if self.is_untouched_draft(row_ix, col_ix) {
            // A draft cell nobody has typed into is not NULL: it is whatever
            // the server puts there, so it says so rather than promising a
            // value.
            cell.text_color(cx.theme().muted_foreground)
                .child("default")
        } else {
            match self.cell(row_ix, col_ix) {
                Some(value) => {
                    // A staged binary value is hex, not the value itself: show
                    // its size, the way the loaded value's own stand-in does,
                    // rather than a wall of hex digits.
                    let staged_binary = self.is_staged(row_ix, col_ix)
                        && self
                            .result
                            .column_types
                            .get(col_ix)
                            .is_some_and(|name| query::is_blob_type(name))
                        && value.len() % 2 == 0
                        && value.bytes().all(|byte| byte.is_ascii_hexdigit());
                    if staged_binary {
                        cell.child(format!("<{} bytes>", value.len() / 2))
                    } else {
                        cell.child(value.to_string())
                    }
                }
                None => cell.text_color(cx.theme().muted_foreground).child("NULL"),
            }
        };

        // The whole cell, padding included, holds the menu, so a right click
        // anywhere in it is about this value. The value's own styling stays
        // on the content: the menu draws inside this element and would
        // otherwise inherit the value's font, colour, and strike-through.
        let pad = cell_padding();
        div()
            .id(("value", col_ix))
            .size_full()
            .pl(pad.left)
            .pr(pad.right)
            .pt(pad.top)
            .pb(pad.bottom)
            // The menu is built on the frame after the click, but an event
            // from the table only reaches its owner after that, so the cell
            // the menu is about is noted here, in the capture phase.
            .capture_any_mouse_down(cx.listener(move |table, event: &MouseDownEvent, _, _cx| {
                if event.button == MouseButton::Right {
                    table.delegate_mut().menu_cell = Some((row_ix, col_ix));
                }
            }))
            .context_menu(grid_menu(cx))
            .child(content)
            .into_any_element()
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
        if let Some(data_ix) = self.data_column(col_ix) {
            let name = self.column(col_ix, cx).name.clone();
            let type_name = self
                .result
                .column_types
                .get(data_ix)
                .cloned()
                .unwrap_or_default();
            let info = self.column_info.get(name.as_ref());
            let text = super::header_text(&name, &type_name, info);
            let primary_key = info.is_some_and(|info| info.primary_key);
            let foreign_key = info.is_some_and(|info| info.references.is_some());
            let tooltip = text.tooltip;
            let muted = cx.theme().muted_foreground;
            // The table leaves room for the sort icon on the right itself.
            let pad = cell_padding();
            return h_flex()
                .id(("column-header", data_ix))
                .size_full()
                .pl(pad.left)
                .py(pad.top)
                .gap_1()
                .overflow_hidden()
                .when(self.is_numeric_column(data_ix), |header| {
                    header.justify_end()
                })
                .aria_label(text.label)
                .aria_description(tooltip.clone())
                .when(primary_key, |header| {
                    header.child(
                        Icon::new(IconName::Key)
                            .xsmall()
                            .flex_none()
                            .text_color(muted),
                    )
                })
                .when(foreign_key, |header| {
                    header.child(
                        Icon::new(IconName::Link)
                            .xsmall()
                            .flex_none()
                            .text_color(muted),
                    )
                })
                .child(div().min_w_0().truncate().child(name))
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                })
                .into_any_element();
        }

        let rows = self.rows_count(cx);
        let all = rows > 0 && self.rows_selected.len() == rows;

        div()
            .size_full()
            // The table keeps a sort icon's width free on the right; the same
            // on the left keeps the box over the column's own boxes.
            .pl(cell_padding().right)
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

impl ResultDelegate {
    /// Whether the result's column `col_ix` holds numbers, by its driver type.
    pub(super) fn is_numeric_column(&self, col_ix: usize) -> bool {
        self.result
            .column_types
            .get(col_ix)
            .is_some_and(|name| crate::db::export::is_numeric_type(name))
    }

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
            return &MISSING;
        };
        cell_at(&self.result.rows, row_ix, col_ix)
    }

    /// The value as it stands, staged edit included.
    pub(super) fn cell(&self, row_ix: usize, col_ix: usize) -> &Cell {
        if let Some(draft) = self.draft(row_ix) {
            return self.drafts[draft].get(&col_ix).unwrap_or(&MISSING);
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

    /// The menu for the row and cell the last right click noted. A click
    /// that landed on no row has nothing to offer.
    pub(super) fn menu(
        &mut self,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> PopupMenu {
        let Some(row_ix) = self.menu_row else {
            return menu;
        };
        self.row_menu_items(row_ix, menu, self.menu_column, window, cx)
    }

    /// The items a right click on `row_ix` offers.
    ///
    /// Right-clicking a row outside the sweep acts on that row alone, the way
    /// a file manager does. An empty menu is never shown, so a grid that
    /// cannot be written to simply has none.
    pub(super) fn row_menu_items(
        &mut self,
        row_ix: usize,
        menu: PopupMenu,
        selected_column: Option<usize>,
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
                        .action(Box::new(ViewCell))
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
                let menu = if self.foreign_keys.contains(&col) && self.cell(row, col).is_some() {
                    let report = self.report_navigate.clone();
                    menu.item(
                        PopupMenuItem::new("Go to referenced row")
                            .on_click(move |_, _window, cx| report(row, col, cx)),
                    )
                } else {
                    menu
                };
                self.cell_menu_items(row, col, menu)
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

        // The same rows in another shape live behind one submenu, so the menu
        // does not become a list of seven copies. The scope is stated once, in
        // the submenu's label: the picked rows when there are any, every row
        // shown otherwise — which is what the plain `Copy` key does with rows
        // picked out.
        let total = self.rows();
        let menu = if total > 0 {
            let label = match picked {
                0 => format!("Copy all {total} rows as…"),
                1 => "Copy 1 row as…".to_string(),
                rows => format!("Copy {rows} rows as…"),
            };
            // The column a `Copy column` item acts on is the cell the menu
            // was opened on when there is one, and the selected cell's column
            // — the fallback the grid worked out — when it was opened beside
            // the cells.
            let column = self
                .menu_cell
                .filter(|(row, _)| *row == row_ix)
                .map(|(_, col)| col)
                .or(selected_column);
            let has_table = self.table.is_some();
            let report = self.report_copy_as.clone();

            let submenu = PopupMenu::build(window, cx, move |mut menu, _window, _cx| {
                let report_headers = report.clone();
                menu = menu.item(
                    PopupMenuItem::new("With headers")
                        .action(Box::new(CopyWithHeaders))
                        .on_click(move |_, _window, cx| {
                            report_headers(CopyAs::Rows(Format::Tsv), cx)
                        }),
                );

                for format in [Format::Csv, Format::Json, Format::Markdown] {
                    let report = report.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format.label())
                            .on_click(move |_, _window, cx| report(CopyAs::Rows(format), cx)),
                    );
                }
                if has_table {
                    let report = report.clone();
                    menu = menu.item(
                        PopupMenuItem::new(Format::Sql.label())
                            .on_click(move |_, _window, cx| report(CopyAs::Rows(Format::Sql), cx)),
                    );
                }

                menu = menu.separator();
                match column {
                    Some(column) => {
                        let report_values = report.clone();
                        menu = menu.item(PopupMenuItem::new("Column values").on_click(
                            move |_, _window, cx| report_values(CopyAs::ColumnValues(column), cx),
                        ));
                        let report_list = report.clone();
                        menu.item(PopupMenuItem::new("IN (...) list").on_click(
                            move |_, _window, cx| report_list(CopyAs::ColumnInList(column), cx),
                        ))
                    }
                    None => menu
                        .item(PopupMenuItem::new("Column values").disabled(true))
                        .item(PopupMenuItem::new("IN (...) list").disabled(true)),
                }
            });
            menu.item(PopupMenuItem::submenu(label, submenu))
        } else {
            menu
        };

        // Exporting is a read, so it is offered on a read-only grid too. Like
        // Delete below, it is about the row the menu was opened on even when
        // that row was not picked out, so the row is taken into the selection
        // first — `Scope::Picked` would otherwise export every row. The formats
        // open a save dialog, hence the ellipsis on the submenu that holds them.
        // A query result's grid has no table name, so it offers none of this.
        let menu = if self.table.is_some() {
            let rows = self.mark_for_menu(row_ix);
            let label = match rows {
                1 => "Export row…".to_string(),
                rows => format!("Export {rows} rows…"),
            };
            let report = self.report_export.clone();
            let submenu = PopupMenu::build(window, cx, move |menu, _window, _cx| {
                Format::FILE.into_iter().fold(menu, |menu, format| {
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
            let report = self.report_change.clone();
            return menu.item(PopupMenuItem::new("Discard new row").on_click(
                move |_, _window, cx| {
                    let Some(table) = table.upgrade() else {
                        return;
                    };
                    table.update(cx, |table, cx| {
                        table.delegate_mut().discard_draft(draft);
                        table.refresh(cx);
                    });
                    // The owner counts staged changes for its footer and the
                    // row panel; a draft gone is one fewer.
                    report(cx);
                },
            ));
        }

        // A copy of the row to finish by hand, beside the row it came from.
        let menu = {
            let report = self.command_reporter();
            menu.separator().item(
                PopupMenuItem::new("Duplicate row").on_click(move |_, window, cx| {
                    report(MenuCommand::Duplicate(row_ix), window, cx)
                }),
            )
        };

        // Taking a row's typed changes back without discarding the rest of
        // the tab's.
        let menu =
            if self.row_has_edits(row_ix) {
                let report = self.command_reporter();
                menu.item(PopupMenuItem::new("Revert row changes").on_click(
                    move |_, window, cx| report(MenuCommand::RevertRow(row_ix), window, cx),
                ))
            } else {
                menu
            };

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
                // Deleting can drop drafts outright, and the table caches its
                // row count, so it is refreshed rather than only redrawn.
                table.refresh(cx);
            });
            report(cx);
        }))
    }

    /// The items about the one cell the menu was opened on: filtering by its
    /// value, and editing it. Each is left out where it cannot apply — a
    /// query result has no filter bar, a read-only grid no edits — the way
    /// Copy as… leaves out SQL without a table.
    fn cell_menu_items(&self, row: usize, col: usize, menu: PopupMenu) -> PopupMenu {
        // A value the grid only describes (`<3 bytes>`, `<XML>`) is not the
        // value, so it cannot be matched on; nor can a row not yet written.
        // The value as the server has it, which is what the filter looks for
        // (a staged edit is on no row yet), so the label says the same.
        let value = self.baseline(row, col);
        let describable = !query::is_binary_type(self.column_type(col))
            && !query::is_placeholder(value, self.column_type(col));
        let menu = if self.table.is_some() && self.source(row).is_some() && describable {
            let report = self.command_reporter();
            let exclude = self.command_reporter();
            let (keep, drop) = match value {
                Some(_) => ("Filter by this value", "Exclude this value"),
                None => ("Filter to NULL", "Exclude NULL"),
            };
            menu.separator()
                .item(PopupMenuItem::new(keep).on_click(move |_, window, cx| {
                    report(
                        MenuCommand::Filter {
                            row,
                            col,
                            exclude: false,
                        },
                        window,
                        cx,
                    )
                }))
                .item(PopupMenuItem::new(drop).on_click(move |_, window, cx| {
                    exclude(
                        MenuCommand::Filter {
                            row,
                            col,
                            exclude: true,
                        },
                        window,
                        cx,
                    )
                }))
        } else {
            menu
        };

        if !self.is_editable(row, col) {
            return menu;
        }
        let edit = self.command_reporter();
        let null = self.command_reporter();
        let menu = menu
            .separator()
            .item(
                PopupMenuItem::new("Edit cell")
                    .action(Box::new(EditCell))
                    .on_click(move |_, window, cx| edit(MenuCommand::Edit(row, col), window, cx)),
            )
            .item(
                PopupMenuItem::new("Set NULL")
                    .action(Box::new(SetNull))
                    .on_click(move |_, window, cx| {
                        null(MenuCommand::SetNull(row, col), window, cx)
                    }),
            );
        // A draft's typed cells are the draft: discarding it is the row's
        // own item, so only a loaded row's edit can be reverted here.
        if self.source(row).is_some() && self.is_staged(row, col) {
            let report = self.command_reporter();
            menu.item(
                PopupMenuItem::new("Revert change").on_click(move |_, window, cx| {
                    report(MenuCommand::Revert(row, col), window, cx)
                }),
            )
        } else {
            menu
        }
    }

    /// A command reporter for a menu item, tied to the rows as they are now:
    /// a menu left open while a reload replaces them runs nothing.
    fn command_reporter(&self) -> impl Fn(MenuCommand, &mut Window, &mut App) + 'static {
        let report = self.report_command.clone();
        let generation = self.generation;
        move |command, window, cx| report(command, generation, window, cx)
    }

    /// Whether the loaded row shown at `row_ix` has a staged edit.
    pub(super) fn row_has_edits(&self, row_ix: usize) -> bool {
        self.source(row_ix)
            .is_some_and(|source| self.edits.keys().any(|(row, _)| *row == source))
    }

    /// Drop the staged edit on one cell, or on every cell of the row when
    /// `col` is `None`.
    pub(super) fn revert(&mut self, row_ix: usize, col: Option<usize>) {
        let Some(source) = self.source(row_ix) else {
            return;
        };
        match col {
            Some(col) => {
                self.edits.remove(&(source, col));
            }
            None => self.edits.retain(|(row, _), _| *row != source),
        }
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
        // The rows after it moved up one, and the last row is gone: nothing
        // picked may point past the end.
        let rows = self.rows();
        self.rows_selected.retain(|&row| row < rows);
        self.anchor = self.anchor.filter(|&row| row < rows);
        self.focused_row = self.focused_row.filter(|&row| row < rows);
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
                move |table, event: &MouseDownEvent, _window, cx| {
                    if event.button != MouseButton::Left
                        || event.modifiers.shift
                        || event.modifiers.secondary(cx)
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
                        if modifiers.secondary(cx) {
                            return;
                        }

                        table.delegate_mut().pick(row_ix, modifiers.shift);
                        cx.notify();

                        // The box took the focus when it was pressed; the rows
                        // are where the keyboard belongs.
                        table.focus_handle(cx).focus(window, cx);
                    })),
            )
            .context_menu(grid_menu(cx))
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

        self.source(row_ix).is_some()
            && !query::is_placeholder(self.baseline(row_ix, col_ix), self.column_type(col_ix))
    }

    /// Column `col_ix`'s driver type name, or nothing when the result has
    /// none.
    pub(super) fn column_type(&self, col_ix: usize) -> &str {
        self.result
            .column_types
            .get(col_ix)
            .map(String::as_str)
            .unwrap_or_default()
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

        // The picked rows, the anchor, and the keyboard's row are in display
        // coordinates, so they follow their rows to where the sort put them:
        // left where they were, they would point at other rows — and the
        // process list's "End Selected" would end other connections.
        let rows = self.result.rows.len();
        let source = |display: usize| self.order.get(display).copied().unwrap_or(display);
        let mut shown = vec![0; rows];
        for (display, &row) in order.iter().enumerate() {
            shown[row] = display;
        }
        let follow = |display: usize| {
            if display < rows {
                shown[source(display)]
            } else {
                // A drafted row sits after the result and does not move.
                display
            }
        };
        self.rows_selected = self.rows_selected.iter().map(|&row| follow(row)).collect();
        self.anchor = self.anchor.map(follow);
        self.focused_row = self.focused_row.map(follow);
        self.menu_row = None;
        self.menu_cell = None;
        self.menu_column = None;

        self.order = order;
    }
}
