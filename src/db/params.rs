//! Named query variables (`:name`).
//!
//! A query tab's variables map names to values; before a statement runs,
//! [`substitute`] rewrites each `:name` placeholder it holds. A placeholder
//! in a value position becomes a real bind parameter — the same value is
//! bound once per occurrence, so `:name` used twice binds twice — while one
//! where a bind cannot go (after `FROM`, for a table name) is pasted in as
//! written, and a value that is a keyword literal (`NOW()`) is pasted rather
//! than bound, since binding would quote it instead of evaluating it.

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use super::config::Engine;
use super::query::Cell;
use super::sql;
use super::statement;

/// One row of a query tab's Variables dialog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    pub value: String,
}

/// Words after which a `:name` names something a bind parameter cannot be —
///
/// a table, a view, a database — so its value is pasted in as written
/// rather than bound.
const IDENTIFIER_WORDS: [&str; 10] = [
    "FROM", "JOIN", "INTO", "UPDATE", "TABLE", "VIEW", "INDEX", "DATABASE", "SCHEMA", "TRUNCATE",
];

/// Rewrite `sql`'s `:name` placeholders with `variables`.
///
/// Returns the rewritten statement and the bind parameters in order. Every
/// placeholder must have a value, or the run is refused naming it. A
/// statement without placeholders comes back unchanged with no parameters.
pub fn substitute(
    sql: &str,
    variables: &[Variable],
    engine: Engine,
) -> Result<(String, Vec<Cell>)> {
    let placeholders = statement::placeholders(sql, engine);
    if placeholders.is_empty() {
        return Ok((sql.to_string(), Vec::new()));
    }

    let values: HashMap<&str, &str> = variables
        .iter()
        .map(|variable| (variable.name.as_str(), variable.value.as_str()))
        .collect();
    for placeholder in &placeholders {
        if !values.contains_key(placeholder.name.as_str()) {
            return Err(anyhow!("no value for variable :{}", placeholder.name));
        }
    }

    let mut rewritten = String::with_capacity(sql.len());
    let mut params: Vec<Cell> = Vec::new();
    let mut cursor = 0;
    for placeholder in &placeholders {
        rewritten.push_str(&sql[cursor..placeholder.start]);
        let value = values[placeholder.name.as_str()];
        if let Some(keyword) = sql::keyword_literal(value) {
            // `NOW()` and friends evaluate; binding would store the text.
            rewritten.push_str(keyword);
        } else if is_identifier_position(sql, placeholder.start) {
            rewritten.push_str(value);
        } else {
            params.push(Some(value.to_string()));
            rewritten.push_str(&sql::placeholder(engine, params.len()));
        }
        cursor = placeholder.end;
    }
    rewritten.push_str(&sql[cursor..]);
    Ok((rewritten, params))
}

/// Whether the placeholder at `pos` sits where a bind parameter cannot go:
///
/// after a word that takes an identifier, or after a `.` qualifying one.
fn is_identifier_position(sql: &str, pos: usize) -> bool {
    let bytes = sql.as_bytes();
    let mut index = pos;
    while index > 0 && bytes[index - 1].is_ascii_whitespace() {
        index -= 1;
    }
    if index > 0 && bytes[index - 1] == b'.' {
        return true;
    }
    let mut start = index;
    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }
    if start < index {
        let word = sql[start..index].to_ascii_uppercase();
        return IDENTIFIER_WORDS.contains(&word.as_str());
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::config::Engine;

    fn variables(pairs: &[(&str, &str)]) -> Vec<Variable> {
        pairs
            .iter()
            .map(|(name, value)| Variable {
                name: name.to_string(),
                value: value.to_string(),
            })
            .collect()
    }

    #[test]
    fn a_statement_without_placeholders_is_untouched() {
        let (sql, params) =
            substitute("SELECT 1", &variables(&[("x", "1")]), Engine::Postgres).unwrap();
        assert_eq!(sql, "SELECT 1");
        assert!(params.is_empty());
    }

    #[test]
    fn a_value_position_becomes_a_bind_parameter() {
        let (sql, params) = substitute(
            "SELECT * FROM t WHERE id = :id",
            &variables(&[("id", "42")]),
            Engine::Postgres,
        )
        .unwrap();
        assert_eq!(sql, "SELECT * FROM t WHERE id = $1");
        assert_eq!(params, vec![Some("42".to_string())]);
    }

    #[test]
    fn mysql_and_sqlite_use_question_marks() {
        let (sql, _) = substitute(
            "SELECT * FROM t WHERE id = :id",
            &variables(&[("id", "42")]),
            Engine::MySql,
        )
        .unwrap();
        assert_eq!(sql, "SELECT * FROM t WHERE id = ?");
    }

    #[test]
    fn a_repeated_variable_binds_once_per_occurrence() {
        let (sql, params) = substitute(
            "INSERT INTO t (name) VALUES (:name); SELECT 1 FROM t WHERE name = :name",
            &variables(&[("name", "widget")]),
            Engine::Postgres,
        )
        .unwrap();
        assert_eq!(
            sql,
            "INSERT INTO t (name) VALUES ($1); SELECT 1 FROM t WHERE name = $2"
        );
        assert_eq!(
            params,
            vec![Some("widget".to_string()), Some("widget".to_string())]
        );
    }

    #[test]
    fn a_missing_variable_refuses_the_run() {
        let error = substitute(
            "SELECT * FROM t WHERE id = :id",
            &variables(&[]),
            Engine::Postgres,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "no value for variable :id");
    }

    #[test]
    fn placeholders_in_strings_and_comments_are_ignored() {
        let (sql, params) = substitute(
            "SELECT ':not_a_var', x FROM t -- :also_not\nWHERE id = :id",
            &variables(&[("id", "1")]),
            Engine::Postgres,
        )
        .unwrap();
        assert_eq!(
            sql,
            "SELECT ':not_a_var', x FROM t -- :also_not\nWHERE id = $1"
        );
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn casts_and_assignments_are_not_placeholders() {
        let (sql, params) = substitute(
            "SELECT x::int, y FROM t WHERE z = :z",
            &variables(&[("z", "1")]),
            Engine::Postgres,
        )
        .unwrap();
        assert_eq!(sql, "SELECT x::int, y FROM t WHERE z = $1");
        assert_eq!(params.len(), 1);
    }

    #[test]
    fn a_table_position_is_pasted_not_bound() {
        let (sql, params) = substitute(
            "SELECT * FROM :table WHERE id = 1",
            &variables(&[("table", "users")]),
            Engine::Postgres,
        )
        .unwrap();
        assert_eq!(sql, "SELECT * FROM users WHERE id = 1");
        assert!(params.is_empty());
    }

    #[test]
    fn a_keyword_literal_is_pasted_not_bound() {
        let (sql, params) = substitute(
            "INSERT INTO t (created) VALUES (:now)",
            &variables(&[("now", "NOW()")]),
            Engine::Postgres,
        )
        .unwrap();
        assert_eq!(sql, "INSERT INTO t (created) VALUES (NOW())");
        assert!(params.is_empty());
    }
}
