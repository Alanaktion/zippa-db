//! The filter bar above a table view.
//!
//! TODO.md section 2 ("Advanced filtering GUI"). One line per filter: the
//! column, what to test it with, and the value to test it against. The bar
//! only describes the filters — turning them into a `where` clause is the
//! table view's job, since it is the one that knows the engine, the column
//! types, and how the rest of the statement is built.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, IndexPath, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, SharedString, Window, div, px};

/// What a filter tests a column with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    Equals,
    NotEquals,
    Greater,
    GreaterOrEqual,
    Less,
    LessOrEqual,
    Like,
    NotLike,
    In,
    NotIn,
    IsNull,
    IsNotNull,
}

impl Operator {
    pub const ALL: [Operator; 12] = [
        Operator::Equals,
        Operator::NotEquals,
        Operator::Greater,
        Operator::GreaterOrEqual,
        Operator::Less,
        Operator::LessOrEqual,
        Operator::Like,
        Operator::NotLike,
        Operator::In,
        Operator::NotIn,
        Operator::IsNull,
        Operator::IsNotNull,
    ];

    /// The operator as it is written in SQL, which is also what the dropdown
    /// shows: this is a tool for people who know the language.
    pub fn label(self) -> &'static str {
        match self {
            Operator::Equals => "=",
            Operator::NotEquals => "<>",
            Operator::Greater => ">",
            Operator::GreaterOrEqual => ">=",
            Operator::Less => "<",
            Operator::LessOrEqual => "<=",
            Operator::Like => "LIKE",
            Operator::NotLike => "NOT LIKE",
            Operator::In => "IN",
            Operator::NotIn => "NOT IN",
            Operator::IsNull => "IS NULL",
            Operator::IsNotNull => "IS NOT NULL",
        }
    }

    pub fn from_label(label: &str) -> Option<Operator> {
        Operator::ALL
            .into_iter()
            .find(|operator| operator.label() == label)
    }

    /// Whether the filter needs something to compare against.
    pub fn needs_value(self) -> bool {
        !matches!(self, Operator::IsNull | Operator::IsNotNull)
    }

    /// Whether the value is SQL rather than a plain value.
    ///
    /// `IN` and `NOT IN` take a list or a subquery, so what is typed goes into
    /// the statement as written instead of being bound as a parameter.
    pub fn takes_expression(self) -> bool {
        matches!(self, Operator::In | Operator::NotIn)
    }
}

/// One filter, as the owner reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterSpec {
    pub column: String,
    pub operator: Operator,
    /// Empty when the operator needs no value.
    pub value: String,
}

/// The bar asks its owner to read the filters again.
pub struct FiltersChanged;

type Choices = Entity<SelectState<Vec<SharedString>>>;

/// One line of the bar.
struct FilterRow {
    column: Choices,
    operator: Choices,
    value: Entity<InputState>,
    /// Hint the value box is showing, so it is only set when it changes.
    hint: &'static str,
}

pub struct FilterBar {
    /// Columns of the table, for the first dropdown. Empty until the table
    /// has been read once.
    columns: Vec<SharedString>,
    /// Whether the dropdowns still have to be told about them.
    columns_changed: bool,
    rows: Vec<FilterRow>,
}

impl EventEmitter<FiltersChanged> for FilterBar {}

impl FilterBar {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            columns: Vec::new(),
            columns_changed: false,
            rows: Vec::new(),
        }
    }

    /// Tell the bar which columns the table has.
    ///
    /// Called after every load: a table whose columns changed under an open
    /// filter would otherwise offer names that are no longer there.
    pub fn set_columns(&mut self, columns: &[String], cx: &mut Context<Self>) {
        let columns: Vec<SharedString> = columns.iter().map(SharedString::from).collect();
        if self.columns == columns {
            return;
        }

        // A filter is about a column of the table it was written for. When
        // the columns change under it — another database, another table — the
        // lines go rather than quietly filtering on a name that is not there.
        let had_columns = !self.columns.is_empty();
        self.columns = columns;
        // The dropdowns are handed the new names while the bar is drawn: a
        // table is read from a task, which has no window to hand them here.
        self.columns_changed = true;
        if had_columns {
            self.rows.clear();
        }
        cx.notify();
    }

    /// Say what belongs in the value box for `operator`.
    ///
    /// `IN` and `NOT IN` take SQL, so the hint says so rather than asking for
    /// a value that would be pasted in as one.
    fn value_hint(operator: Operator) -> &'static str {
        if operator.takes_expression() {
            "1, 2 or select id from …"
        } else {
            "value"
        }
    }

    /// Keep each value box's hint in step with the operator beside it.
    fn refresh_hints(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for index in 0..self.rows.len() {
            let hint = Self::value_hint(self.operator_of(index, cx).unwrap_or(Operator::Equals));
            if self.rows[index].hint == hint {
                continue;
            }

            self.rows[index].hint = hint;
            let value = self.rows[index].value.clone();
            value.update(cx, |state, cx| state.set_placeholder(hint, window, cx));
        }
    }

    /// Give the open dropdowns the columns the table last came back with.
    fn refresh_columns(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.columns_changed {
            return;
        }
        self.columns_changed = false;

        for row in &self.rows {
            row.column.update(cx, |state, cx| {
                state.set_items(self.columns.clone(), window, cx);
            });
        }
    }

    /// Build one filter row: the column and operator dropdowns, the value
    /// box, and the subscriptions that read the filters again on a change.
    /// Shared by [`Self::add_filter`], which starts a row empty, and
    /// [`Self::set_filter`], which starts one already filled in.
    fn build_row(
        column_items: Vec<SharedString>,
        column_index: usize,
        operator: Operator,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> FilterRow {
        let column = cx.new(|cx| {
            SelectState::new(column_items, Some(IndexPath::new(column_index)), window, cx)
        });

        let operators: Vec<SharedString> = Operator::ALL
            .iter()
            .map(|operator| SharedString::from(operator.label()))
            .collect();
        let operator_index = Operator::ALL
            .iter()
            .position(|candidate| *candidate == operator)
            .expect("every operator is in ALL");
        let operator_state = cx.new(|cx| {
            SelectState::new(operators, Some(IndexPath::new(operator_index)), window, cx)
        });

        let hint = Self::value_hint(operator);
        let value_state = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(hint)
                .default_value(value.to_string())
        });

        // Picking a column or an operator does not run anything on its own;
        // the filters are read again when the user asks for them to be.
        cx.subscribe(
            &column,
            |this, _, _: &SelectEvent<Vec<SharedString>>, cx| {
                cx.notify();
                this.apply(cx);
            },
        )
        .detach();
        cx.subscribe(
            &operator_state,
            |this, _, _: &SelectEvent<Vec<SharedString>>, cx| {
                cx.notify();
                this.apply(cx);
            },
        )
        .detach();
        cx.subscribe(&value_state, |this, _, event: &InputEvent, cx| {
            // Applied on Enter rather than per keystroke, so a half-typed
            // value never runs a query of its own.
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.apply(cx);
            }
        })
        .detach();

        FilterRow {
            column,
            operator: operator_state,
            value: value_state,
            hint,
        }
    }

    /// Add an empty line to the bar, on the first column.
    pub fn add_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.columns.is_empty() {
            return;
        }

        let row = Self::build_row(self.columns.clone(), 0, Operator::Equals, "", window, cx);
        self.rows.push(row);
        cx.notify();
    }

    /// Replace every filter with one already filled in, e.g. for a foreign
    /// key jump: the matching row is what the user asked to see, not one more
    /// condition AND'd onto whatever was filtered before.
    ///
    /// May run before the table's first load has told the bar its columns —
    /// opening a fresh tab to jump straight into, say — so the dropdown is
    /// seeded with just the column already known rather than waiting on
    /// [`Self::set_columns`]. `SelectState` keeps the selected value apart
    /// from the item list `refresh_columns` later swaps in, so the selection
    /// survives that.
    pub fn set_filter(
        &mut self,
        column: &str,
        operator: Operator,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rows.clear();

        let known = self.columns.iter().any(|name| name == column);
        let items = if known {
            self.columns.clone()
        } else {
            vec![SharedString::from(column)]
        };
        let column_index = items.iter().position(|name| name == column).unwrap_or(0);

        let row = Self::build_row(items, column_index, operator, value, window, cx);
        self.rows.push(row);
        self.apply(cx);
    }

    fn remove_filter(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.rows.len() {
            return;
        }
        self.rows.remove(index);
        cx.notify();
        self.apply(cx);
    }

    /// Throw every filter away.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        self.rows.clear();
        cx.notify();
        self.apply(cx);
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        cx.emit(FiltersChanged);
    }

    /// The filters as they stand, with the unfinished ones left out.
    pub fn specs(&self, cx: &App) -> Vec<FilterSpec> {
        self.rows
            .iter()
            .filter_map(|row| {
                let column = row.column.read(cx).selected_value()?.to_string();
                let operator = Operator::from_label(row.operator.read(cx).selected_value()?)?;
                let value = row.value.read(cx).value().trim().to_string();

                // A filter with nothing to compare against is still being
                // written, so it does not narrow anything yet.
                if operator.needs_value() && value.is_empty() {
                    return None;
                }

                Some(FilterSpec {
                    column,
                    operator,
                    value,
                })
            })
            .collect()
    }

    fn operator_of(&self, index: usize, cx: &App) -> Option<Operator> {
        let row = self.rows.get(index)?;
        Operator::from_label(row.operator.read(cx).selected_value()?)
    }

    fn render_row(&self, index: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let row = &self.rows[index];
        let operator = self.operator_of(index, cx).unwrap_or(Operator::Equals);

        h_flex()
            .w_full()
            .gap_2()
            .child(
                div().w(px(180.)).child(
                    Select::new(&row.column)
                        .id(("filter-column", index))
                        .xsmall()
                        .placeholder("column")
                        .accessibility_label("Filter column"),
                ),
            )
            .child(
                div().w(px(120.)).child(
                    Select::new(&row.operator)
                        .id(("filter-operator", index))
                        .xsmall()
                        .placeholder("is")
                        .accessibility_label("Filter operator"),
                ),
            )
            // `IS NULL` has nothing to compare against, so the box for it goes
            // away rather than sitting there doing nothing.
            .when(operator.needs_value(), |this| {
                this.child(
                    div().flex_1().min_w(px(120.)).child(
                        Input::new(&row.value)
                            .id(("filter-value", index))
                            .aria_label("Filter value")
                            .xsmall(),
                    ),
                )
            })
            .child(
                Button::new(("remove-filter", index))
                    .ghost()
                    .xsmall()
                    .icon(IconName::Close)
                    .accessibility_label("Remove this filter")
                    .tooltip("Remove this filter")
                    .on_click(
                        cx.listener(move |this, _, _window, cx| this.remove_filter(index, cx)),
                    ),
            )
    }
}

impl Render for FilterBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_columns(window, cx);
        self.refresh_hints(window, cx);

        let rows: Vec<_> = (0..self.rows.len())
            .map(|index| self.render_row(index, cx).into_any_element())
            .collect();

        v_flex()
            .w_full()
            .flex_none()
            .px_3()
            .py_1()
            .gap_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().status_bar)
            .children(rows)
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("add-filter")
                            .ghost()
                            .xsmall()
                            .icon(IconName::Plus)
                            .label("Filter")
                            .tooltip("Narrow the rows this table shows")
                            .disabled(self.columns.is_empty())
                            .on_click(
                                cx.listener(|this, _, window, cx| this.add_filter(window, cx)),
                            ),
                    )
                    .when(!self.rows.is_empty(), |this| {
                        this.child(
                            Button::new("clear-filters")
                                .ghost()
                                .xsmall()
                                .label("Clear")
                                .tooltip("Remove every filter")
                                .on_click(cx.listener(|this, _, _window, cx| this.clear(cx))),
                        )
                    }),
            )
    }
}

#[cfg(test)]
impl FilterBar {
    /// Add a filter already filled in, the way using the bar does.
    pub(crate) fn add_filter_for_test(
        &mut self,
        column: &str,
        operator: Operator,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_filter(window, cx);
        let Some(row) = self.rows.last() else {
            return;
        };

        let column_index = self
            .columns
            .iter()
            .position(|name| name == column)
            .expect("the table has no such column");
        row.column.update(cx, |state, cx| {
            state.set_selected_index(Some(IndexPath::new(column_index)), window, cx);
        });

        let operator_index = Operator::ALL
            .iter()
            .position(|candidate| *candidate == operator)
            .expect("every operator is in ALL");
        row.operator.update(cx, |state, cx| {
            state.set_selected_index(Some(IndexPath::new(operator_index)), window, cx);
        });

        row.value.update(cx, |state, cx| {
            state.set_value(value.to_string(), window, cx)
        });

        self.apply(cx);
    }

    pub(crate) fn filter_count_for_test(&self) -> usize {
        self.rows.len()
    }

    /// Whether the line at `index` is showing its value box.
    pub(crate) fn shows_value_for_test(&self, index: usize, cx: &App) -> bool {
        self.operator_of(index, cx)
            .map(Operator::needs_value)
            .unwrap_or(false)
    }
}
