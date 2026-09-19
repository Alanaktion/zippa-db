//! The SQL this view generates.
//!
//! Split out from the view itself so the statements that read a page, filter
//! it, and write a row back are in one place: they share the table's quoting,
//! its column types, and the row key it addresses rows by. Anything else that
//! needs a statement about the open table — exporting it, describing it —
//! belongs here too rather than in the rendering.
//!
//! Values are never pasted in. Every value is bound as a parameter through
//! [`typed_placeholder`], with two deliberate exceptions: `IN` / `NOT IN` take
//! SQL the user wrote so a subquery can do the filtering, and a staged cell
//! that reads as one of [`keyword_literal`]'s keywords — `NOW()` and its kin —
//! is pasted in as that keyword rather than bound as text, the way a cell
//! already accepts `NULL`.

use gpui_kit::component::table::ColumnSort;

use crate::db::query::{Cell, QueryResult};
use crate::db::{
    Engine, RowKey, keyword_literal, placeholder, quote_identifier, text_type, typed_placeholder,
};
use crate::ui::data_grid::StagedRow;
use crate::ui::filter_bar::{FilterSpec, Operator};

use super::TableView;

impl TableView {
    /// The table this view reads and writes, qualified and quoted.
    fn target(&self) -> String {
        let engine = self.connection.config.engine;
        match &self.object.schema {
            Some(schema) => format!(
                "{}.{}",
                quote_identifier(schema, engine),
                quote_identifier(&self.object.name, engine)
            ),
            None => quote_identifier(&self.object.name, engine),
        }
    }

    /// The statement this view runs for its current page and sort.
    #[cfg(test)]
    pub fn query(&self, cx: &gpui_kit::App) -> String {
        self.query_with_params(cx).0
    }

    /// The statement and the values bound to it.
    ///
    /// A filter's value is a parameter rather than text pasted into the
    /// statement — except for `IN` and `NOT IN`, whose value is SQL the user
    /// wrote and is meant to run as written.
    pub fn query_with_params(&self, cx: &gpui_kit::App) -> (String, Vec<Cell>) {
        self.select_sql(true, cx)
    }

    /// The `select` for this view, paged for the grid or whole for an export.
    ///
    /// A whole-table export still honours the filters and the sort; only the
    /// `limit` / `offset` are left off, since every row is what an export is
    /// for.
    pub(super) fn select_sql(&self, paged: bool, cx: &gpui_kit::App) -> (String, Vec<Cell>) {
        let engine = self.connection.config.engine;
        let target = self.target();

        // A table with no primary key is addressed by the engine's own row
        // identifier, which `select *` leaves out, so it is asked for by name
        // and taken back out of the result before the grid sees it.
        let mut sql = match &self.row_key {
            Some(RowKey::RowId(id)) => format!("select {id}, * from {target}"),
            _ => format!("select * from {target}"),
        };

        let (conditions, params) = self.where_clause(cx);
        if !conditions.is_empty() {
            sql.push_str(&format!(" where {}", conditions.join(" and ")));
        }

        if let Some((column, sort)) = &self.sort {
            let direction = match sort {
                ColumnSort::Descending => "desc",
                _ => "asc",
            };
            sql.push_str(&format!(
                " order by {} {direction}",
                quote_identifier(column, engine)
            ));
        }

        if paged {
            sql.push_str(&format!(
                " limit {} offset {}",
                self.limit,
                self.page * self.limit
            ));
        }
        (sql, params)
    }

    /// The filter bar's lines as SQL conditions, and the values to bind.
    fn where_clause(&self, cx: &gpui_kit::App) -> (Vec<String>, Vec<Cell>) {
        let engine = self.connection.config.engine;
        let mut params: Vec<Cell> = Vec::new();
        let mut conditions = Vec::new();

        for spec in self.filters.read(cx).specs(cx) {
            let Some(condition) = self.condition(&spec, engine, &mut params) else {
                continue;
            };
            conditions.push(condition);
        }

        (conditions, params)
    }

    /// One filter as a condition, pushing whatever it binds onto `params`.
    fn condition(
        &self,
        spec: &FilterSpec,
        engine: Engine,
        params: &mut Vec<Cell>,
    ) -> Option<String> {
        let column = quote_identifier(&spec.column, engine);
        let type_name = self
            .columns
            .iter()
            .position(|name| *name == spec.column)
            .and_then(|index| self.column_types.get(index).cloned())
            .unwrap_or_default();

        Some(match spec.operator {
            Operator::IsNull => format!("{column} is null"),
            Operator::IsNotNull => format!("{column} is not null"),
            // The value is a list or a subquery the user wrote, so it goes in
            // as written; nothing else here does.
            Operator::In => format!("{column} in ({})", spec.value),
            Operator::NotIn => format!("{column} not in ({})", spec.value),
            // A pattern is text whatever the column holds, so the column is
            // the thing that gets cast here.
            Operator::Like | Operator::NotLike => {
                params.push(Some(spec.value.clone()));
                let keyword = if spec.operator == Operator::Like {
                    "like"
                } else {
                    "not like"
                };
                format!(
                    "cast({column} as {}) {keyword} {}",
                    text_type(engine),
                    placeholder(engine, params.len())
                )
            }
            operator => {
                params.push(Some(spec.value.clone()));
                format!(
                    "{column} {} {}",
                    operator.label(),
                    typed_placeholder(engine, params.len(), &type_name)
                )
            }
        })
    }

    /// Take the row identifier column back out of a freshly loaded page.
    ///
    /// It was asked for so rows could be addressed, not so it could be shown,
    /// so its values are kept here and the grid is handed the table's own
    /// columns.
    pub(super) fn take_key_column(&mut self, result: &mut QueryResult) {
        self.key_values.clear();
        self.key_type = None;

        if !matches!(self.row_key, Some(RowKey::RowId(_))) {
            return;
        }

        let (values, key_type) = take_row_id(result);
        self.key_values = values;
        self.key_type = key_type;
    }

    /// How a row is addressed in a write: the left-hand side, the type to cast
    /// the parameter to, and the value itself.
    fn key_for(&self, row: usize, cx: &gpui_kit::App) -> Option<Vec<(String, String, Cell)>> {
        let engine = self.connection.config.engine;
        match self.row_key.as_ref()? {
            RowKey::Columns(columns) => {
                let loaded = self.grid.read(cx).baseline_row(row, cx)?;
                columns
                    .iter()
                    .map(|column| {
                        let index = self.columns.iter().position(|name| name == column)?;
                        let value = loaded.get(index)?.clone();
                        // A NULL key would never match with `=`, and a key
                        // column should not be NULL in the first place.
                        value.as_ref()?;
                        Some((
                            quote_identifier(column, engine),
                            self.column_types.get(index).cloned().unwrap_or_default(),
                            value,
                        ))
                    })
                    .collect()
            }
            RowKey::RowId(id) => {
                let value = self.key_values.get(row)?.clone();
                value.as_ref()?;
                Some(vec![(
                    id.to_string(),
                    self.key_type.clone().unwrap_or_default(),
                    value,
                )])
            }
            RowKey::Unavailable(_) => None,
        }
    }

    /// The `UPDATE` for one row's staged cells, and the values to bind to it.
    pub(super) fn update_statement(
        &self,
        staged: &StagedRow,
        cx: &gpui_kit::App,
    ) -> Option<(String, Vec<Cell>)> {
        if staged.cells.is_empty() {
            return None;
        }
        let engine = self.connection.config.engine;
        let key = self.key_for(staged.row, cx)?;

        let mut params: Vec<Cell> = Vec::with_capacity(staged.cells.len() + key.len());

        let assignments: Vec<String> = staged
            .cells
            .iter()
            .map(|(column, value)| {
                let name = self.columns.get(*column).cloned().unwrap_or_default();
                // A recognized keyword is pasted as written: bound as a
                // parameter, a driver would quote it as text instead of
                // evaluating it.
                if let Some(keyword) = value.as_deref().and_then(keyword_literal) {
                    return format!("{} = {keyword}", quote_identifier(&name, engine));
                }
                params.push(value.clone());
                let type_name = self.column_types.get(*column).cloned().unwrap_or_default();
                format!(
                    "{} = {}",
                    quote_identifier(&name, engine),
                    typed_placeholder(engine, params.len(), &type_name)
                )
            })
            .collect();

        let conditions: Vec<String> = key
            .into_iter()
            .map(|(left, type_name, value)| {
                params.push(value);
                format!(
                    "{left} = {}",
                    typed_placeholder(engine, params.len(), &type_name)
                )
            })
            .collect();

        Some((
            format!(
                "update {} set {} where {}",
                self.target(),
                assignments.join(", "),
                conditions.join(" and ")
            ),
            params,
        ))
    }

    /// The `INSERT` for one row being built by hand, and its values.
    ///
    /// Columns nobody typed into are left out, so the server fills them in
    /// itself: leaving them out is the only way to get a default or a
    /// generated key.
    pub(super) fn insert_statement(&self, cells: &[(usize, Cell)]) -> Option<(String, Vec<Cell>)> {
        if cells.is_empty() {
            return None;
        }

        let engine = self.connection.config.engine;
        let mut params: Vec<Cell> = Vec::with_capacity(cells.len());
        let mut columns = Vec::with_capacity(cells.len());
        let mut values = Vec::with_capacity(cells.len());

        for (column, value) in cells {
            let name = self.columns.get(*column).cloned().unwrap_or_default();
            if name.is_empty() {
                return None;
            }
            columns.push(quote_identifier(&name, engine));

            // A recognized keyword is pasted as written: bound as a
            // parameter, a driver would quote it as text instead of
            // evaluating it.
            if let Some(keyword) = value.as_deref().and_then(keyword_literal) {
                values.push(keyword.to_string());
                continue;
            }
            let type_name = self.column_types.get(*column).cloned().unwrap_or_default();
            params.push(value.clone());
            values.push(typed_placeholder(engine, params.len(), &type_name));
        }

        Some((
            format!(
                "insert into {} ({}) values ({})",
                self.target(),
                columns.join(", "),
                values.join(", ")
            ),
            params,
        ))
    }

    /// The `DELETE` for one row marked for deletion, and its values.
    pub(super) fn delete_statement(
        &self,
        row: usize,
        cx: &gpui_kit::App,
    ) -> Option<(String, Vec<Cell>)> {
        let engine = self.connection.config.engine;
        let key = self.key_for(row, cx)?;

        let mut params: Vec<Cell> = Vec::with_capacity(key.len());
        let conditions: Vec<String> = key
            .into_iter()
            .enumerate()
            .map(|(index, (left, type_name, value))| {
                params.push(value);
                format!(
                    "{left} = {}",
                    typed_placeholder(engine, index + 1, &type_name)
                )
            })
            .collect();

        Some((
            format!(
                "delete from {} where {}",
                self.target(),
                conditions.join(" and ")
            ),
            params,
        ))
    }

    /// Whether a staged row changes a column the write addresses it by.
    pub(super) fn touches_key(&self, staged: &StagedRow) -> bool {
        let Some(RowKey::Columns(key)) = &self.row_key else {
            return false;
        };
        staged.cells.iter().any(|(column, _)| {
            self.columns
                .get(*column)
                .is_some_and(|name| key.contains(name))
        })
    }
}

/// Take the row identifier column out of a freshly loaded result, handing back
/// its values and its driver type.
///
/// It was asked for so rows could be addressed by a write, not so it could be
/// shown, so the grid is handed the table's own columns. An export drops it for
/// the same reason and ignores what comes back.
pub(super) fn take_row_id(result: &mut QueryResult) -> (Vec<Cell>, Option<String>) {
    let mut values = Vec::new();
    if result.columns.is_empty() {
        return (values, None);
    }

    result.columns.remove(0);
    let key_type = if result.column_types.is_empty() {
        None
    } else {
        Some(result.column_types.remove(0))
    };
    for row in &mut result.rows {
        if row.is_empty() {
            values.push(None);
            continue;
        }
        values.push(row.remove(0));
    }
    (values, key_type)
}
