//! A table's own definition: columns, indexes, and foreign keys.
//!
//! Mirrors the shape of [`Connection::row_key`](super::connection::Connection::row_key):
//! one entry point dispatching per-engine to three sibling `*_sql` functions
//! in `postgres.rs`/`mysql.rs`/`sqlite.rs`, run through the same
//! `run_query`/`fetch_all` path everything else uses.

use anyhow::Result;

use super::config::Engine;
use super::connection::{Connection, DatabaseObject};
use super::query::{Cell, QueryResult};
use super::{mysql, postgres, sqlite};

/// One column of a table, as the server reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub name: String,
    /// Driver-reported, e.g. `character varying(255)`.
    pub type_name: String,
    pub nullable: bool,
    /// Raw expression text, shown verbatim.
    pub default: Option<String>,
    /// Cross-referenced from the same dedicated primary key read
    /// [`Connection::row_key`] uses, rather than [`TableSchema::indexes`]:
    /// SQLite's common single-column `INTEGER PRIMARY KEY` is the `rowid`
    /// itself and has no backing index to read it from.
    pub is_primary_key: bool,
}

/// One index, in the order its columns are searched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexDef {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub is_primary_key: bool,
}

/// One foreign key, local columns to referenced columns in matching order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyDef {
    /// SQLite reports no name for a foreign key; one is synthesized.
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_schema: Option<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
    pub on_delete: ReferentialAction,
    pub on_update: ReferentialAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferentialAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl ReferentialAction {
    /// Parse the text every engine reports this as (`CASCADE`, `SET NULL`, ...).
    fn parse(action: &str) -> Self {
        match action.to_ascii_uppercase().as_str() {
            "CASCADE" => Self::Cascade,
            "SET NULL" => Self::SetNull,
            "SET DEFAULT" => Self::SetDefault,
            "RESTRICT" => Self::Restrict,
            _ => Self::NoAction,
        }
    }

    /// As it reads in a generated `ON DELETE`/`ON UPDATE` clause.
    pub fn label(self) -> &'static str {
        match self {
            Self::NoAction => "NO ACTION",
            Self::Restrict => "RESTRICT",
            Self::Cascade => "CASCADE",
            Self::SetNull => "SET NULL",
            Self::SetDefault => "SET DEFAULT",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableSchema {
    pub columns: Vec<ColumnDef>,
    pub indexes: Vec<IndexDef>,
    pub foreign_keys: Vec<ForeignKeyDef>,
}

impl Connection {
    /// The structure of `object`: its columns, indexes, and foreign keys.
    ///
    /// Three round trips, like `row_key` is one more query on top of the
    /// object list — table metadata is small, so nothing here is worth
    /// running concurrently.
    pub async fn table_schema(&self, object: &DatabaseObject) -> Result<TableSchema> {
        let engine = self.config.engine;
        // `objects` drops the schema when it is the default one, same as
        // `row_key` — an unqualified Postgres table is in `public`.
        let schema = object.schema.as_deref().unwrap_or("public");
        let table = object.name.as_str();

        let (columns_sql, indexes_sql, foreign_keys_sql, primary_key_sql) = match engine {
            Engine::Postgres => (
                postgres::columns_sql(schema, table),
                postgres::indexes_sql(schema, table),
                postgres::foreign_keys_sql(schema, table),
                postgres::primary_key_sql(schema, table),
            ),
            Engine::MySql => (
                mysql::columns_sql(table),
                mysql::indexes_sql(table),
                mysql::foreign_keys_sql(table),
                mysql::primary_key_sql(table),
            ),
            Engine::Sqlite => (
                sqlite::columns_sql(table),
                sqlite::indexes_sql(table),
                sqlite::foreign_keys_sql(table),
                sqlite::primary_key_sql(table),
            ),
        };

        let columns_result = self.run_query(&columns_sql).await?;
        let indexes_result = self.run_query(&indexes_sql).await?;
        let foreign_keys_result = self.run_query(&foreign_keys_sql).await?;
        let primary_key_result = self.run_query(&primary_key_sql).await?;

        let pk_columns: Vec<String> = primary_key_result
            .rows
            .iter()
            .filter_map(|row| row.first().cloned().flatten())
            .collect();

        let mut columns = parse_columns(&columns_result);
        for column in &mut columns {
            column.is_primary_key = pk_columns.contains(&column.name);
        }

        Ok(TableSchema {
            columns,
            indexes: parse_indexes(&indexes_result),
            foreign_keys: parse_foreign_keys(&foreign_keys_result),
        })
    }
}

fn text(row: &[Cell], index: usize) -> String {
    text_opt(row, index).unwrap_or_default()
}

fn text_opt(row: &[Cell], index: usize) -> Option<String> {
    row.get(index).cloned().flatten()
}

/// A boolean expression's cell, which every engine's `cell()` renders as `1`
/// or `0` — a real boolean on Postgres, an integer comparison elsewhere.
fn flag(row: &[Cell], index: usize) -> bool {
    text_opt(row, index).as_deref() == Some("1")
}

/// A comma-joined column list back into its parts.
fn split_columns(joined: &str) -> Vec<String> {
    joined
        .split(',')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn parse_columns(result: &QueryResult) -> Vec<ColumnDef> {
    result
        .rows
        .iter()
        .map(|row| ColumnDef {
            name: text(row, 0),
            type_name: text(row, 1),
            nullable: flag(row, 2),
            default: text_opt(row, 3),
            is_primary_key: false,
        })
        .collect()
}

fn parse_indexes(result: &QueryResult) -> Vec<IndexDef> {
    result
        .rows
        .iter()
        .map(|row| IndexDef {
            name: text(row, 0),
            columns: split_columns(&text(row, 1)),
            unique: flag(row, 2),
            is_primary_key: flag(row, 3),
        })
        .collect()
}

fn parse_foreign_keys(result: &QueryResult) -> Vec<ForeignKeyDef> {
    result
        .rows
        .iter()
        .enumerate()
        .map(|(position, row)| ForeignKeyDef {
            name: text_opt(row, 0).unwrap_or_else(|| format!("fk_{}", position + 1)),
            columns: split_columns(&text(row, 1)),
            referenced_schema: text_opt(row, 2),
            referenced_table: text(row, 3),
            referenced_columns: split_columns(&text(row, 4)),
            on_delete: ReferentialAction::parse(&text(row, 5)),
            on_update: ReferentialAction::parse(&text(row, 6)),
        })
        .collect()
}
