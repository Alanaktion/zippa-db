//! Named query variables (`:name` or `$name`).
//!
//! A query tab's variables map names to values; before a statement runs,
//! [`substitute`] rewrites each `:name`/`$name` placeholder it holds. A placeholder
//! in a value position becomes a real bind parameter — the same value is
//! bound once per occurrence, so a name used twice binds twice — while one
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

/// Words after which a placeholder names something a bind parameter cannot be —
///
/// a table, a view, a database — so its value is pasted in as written
/// rather than bound.
const IDENTIFIER_WORDS: [&str; 10] = [
    "FROM", "JOIN", "INTO", "UPDATE", "TABLE", "VIEW", "INDEX", "DATABASE", "SCHEMA", "TRUNCATE",
];

/// Rewrite `sql`'s `:name`/`$name` placeholders with `variables`.
///
/// Returns the rewritten statement and the bind parameters in order. Every
/// placeholder must have a value, or the run is refused naming it. A
/// statement without placeholders comes back unchanged with no parameters.
pub fn substitute(
    sql: &str,
    variables: &[Variable],
    engine: Engine,
) -> Result<(String, Vec<Cell>)> {
    rewrite(sql, variables, engine).map(|rewrite| (rewrite.sql, rewrite.params))
}

/// What [`substitute`] made of a statement, with where each placeholder went,
/// so a place in the rewritten SQL (a server's error position) can be found
/// again in the text the user wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rewrite {
    pub sql: String,
    pub params: Vec<Cell>,
    /// Each placeholder's byte range in the original, and the range its
    /// replacement took in `sql`, in order.
    spans: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)>,
}

impl Rewrite {
    /// The byte offset in the original statement of `offset` in the
    /// rewritten one. Inside a replacement it lands on the placeholder's
    /// start.
    pub fn original_offset(&self, offset: usize) -> usize {
        let mut shift: isize = 0;
        for (original, replaced) in &self.spans {
            if offset < replaced.start {
                break;
            }
            if offset < replaced.end {
                return original.start;
            }
            shift = original.end as isize - replaced.end as isize;
        }
        (offset as isize + shift).max(0) as usize
    }
}

/// [`substitute`], keeping where each placeholder went.
pub fn rewrite(sql: &str, variables: &[Variable], engine: Engine) -> Result<Rewrite> {
    let placeholders = statement::placeholders(sql, engine);
    if placeholders.is_empty() {
        return Ok(Rewrite {
            sql: sql.to_string(),
            ..Rewrite::default()
        });
    }

    let values: HashMap<&str, &str> = variables
        .iter()
        .map(|variable| (variable.name.as_str(), variable.value.as_str()))
        .collect();
    for placeholder in &placeholders {
        if !values.contains_key(placeholder.name.as_str()) {
            return Err(anyhow!("no value for variable '{}'", placeholder.name));
        }
    }

    let mut rewritten = String::with_capacity(sql.len());
    let mut params: Vec<Cell> = Vec::new();
    let mut spans = Vec::with_capacity(placeholders.len());
    let mut cursor = 0;
    for placeholder in &placeholders {
        rewritten.push_str(&sql[cursor..placeholder.start]);
        let from = rewritten.len();
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
        spans.push((placeholder.start..placeholder.end, from..rewritten.len()));
        cursor = placeholder.end;
    }
    rewritten.push_str(&sql[cursor..]);
    Ok(Rewrite {
        sql: rewritten,
        params,
        spans,
    })
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
    fn an_offset_in_the_rewrite_maps_back_to_what_was_typed() {
        let typed = "SELECT * FROM t WHERE name = :name AND x frm";
        let rewrite = rewrite(typed, &variables(&[("name", "a")]), Engine::Postgres).unwrap();
        assert_eq!(rewrite.sql, "SELECT * FROM t WHERE name = $1 AND x frm");
        // Before the placeholder nothing moved.
        assert_eq!(rewrite.original_offset(7), 7);
        // Inside it, its start.
        assert_eq!(rewrite.original_offset(29), 29);
        // After it, shifted by the length it lost.
        let frm = rewrite.sql.find("frm").unwrap();
        assert_eq!(rewrite.original_offset(frm), typed.find("frm").unwrap());
    }

    #[test]
    fn an_offset_after_a_second_replacement_maps_back() {
        let typed = "SELECT * FROM t WHERE name = :name AND id = :id AND x frm";
        let rewrite = rewrite(
            typed,
            &variables(&[("name", "a"), ("id", "42")]),
            Engine::Postgres,
        )
        .unwrap();
        assert_eq!(
            rewrite.sql,
            "SELECT * FROM t WHERE name = $1 AND id = $2 AND x frm"
        );
        let frm = rewrite.sql.find("frm").unwrap();
        assert_eq!(rewrite.original_offset(frm), typed.find("frm").unwrap());
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
        assert_eq!(error.to_string(), "no value for variable 'id'");
    }

    #[test]
    fn a_dollar_variable_substitutes_like_a_colon_one() {
        let (sql, params) = substitute(
            "SELECT * FROM t WHERE id = $id",
            &variables(&[("id", "42")]),
            Engine::Postgres,
        )
        .unwrap();
        assert_eq!(sql, "SELECT * FROM t WHERE id = $1");
        assert_eq!(params, vec![Some("42".to_string())]);
    }

    #[test]
    fn mixed_sigils_share_one_value() {
        let (sql, params) = substitute(
            "SELECT * FROM t WHERE a = :x AND b = $x",
            &variables(&[("x", "1")]),
            Engine::Postgres,
        )
        .unwrap();
        assert_eq!(sql, "SELECT * FROM t WHERE a = $1 AND b = $2");
        assert_eq!(params, vec![Some("1".to_string()), Some("1".to_string())]);
    }

    #[test]
    fn a_missing_dollar_variable_names_itself() {
        let error = substitute(
            "SELECT * FROM t WHERE id = $id",
            &variables(&[]),
            Engine::Postgres,
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "no value for variable 'id'");
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
