//! What a result tab says about the result it shows.
//!
//! A script that returns several results gets one tab per result. A bare
//! `Result 3` tells the user nothing, so each tab is named by what the
//! statement behind it did — `users · 42 rows`, `3 updated` — with the
//! statement's first line and its time in the tooltip. Pure text, so it is
//! tested without a window.

use super::config::Engine;
use super::query::QueryResult;
use super::statement::verb_and_table;

/// How long a tooltip's statement line may run before it is cut.
const TOOLTIP_STATEMENT: usize = 80;

/// One result tab's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultLabel {
    /// The tab's own text: `users · 42 rows`.
    pub label: String,
    /// The statement's first line and how long it took.
    pub tooltip: String,
}

impl ResultLabel {
    /// The tab for a statement's result. `statement` is the SQL as typed,
    /// when the run knows it; without it the tab still says how many rows.
    pub fn for_result(result: &QueryResult, statement: Option<&str>, engine: Engine) -> Self {
        let (verb, table) = statement
            .and_then(|sql| verb_and_table(sql, engine))
            .map_or((None, None), |(verb, table)| (Some(verb), table));

        let count = count_label(result, verb.as_deref());
        let label = match table {
            Some(table) => format!("{table} · {count}"),
            None => count,
        };

        let elapsed = format!("{} ms", result.elapsed.as_millis());
        let tooltip = match statement.and_then(first_line) {
            Some(line) => format!("{line}\n{elapsed}"),
            None => elapsed,
        };
        Self { label, tooltip }
    }

    /// The tab a script's skipped failures are listed in, said in words so
    /// it is not told apart by colour alone.
    pub fn for_failures(failures: usize) -> Self {
        let label = match failures {
            1 => "Error".to_string(),
            count => format!("{count} errors"),
        };
        let tooltip = match failures {
            1 => "The statement that failed and was skipped".to_string(),
            _ => "The statements that failed and were skipped".to_string(),
        };
        Self { label, tooltip }
    }
}

/// `42 rows`, `3 updated`, or — for a statement that returns nothing and
/// changes no rows, like `CREATE TABLE` — its verb.
fn count_label(result: &QueryResult, verb: Option<&str>) -> String {
    let changed = match verb {
        Some("UPDATE") => Some("updated"),
        Some("INSERT") | Some("REPLACE") => Some("inserted"),
        Some("DELETE") => Some("deleted"),
        Some("MERGE") => Some("affected"),
        _ => None,
    };
    if result.rows.is_empty()
        && let (Some(changed), Some(affected)) = (changed, result.affected)
    {
        return format!("{} {changed}", grouped(affected));
    }
    if result.rows.is_empty()
        && result.columns.is_empty()
        && let Some(verb) = verb
    {
        return capitalized(verb);
    }
    rows(result.row_count() as u64)
}

/// `1 row`, `42 rows`, `48,210 rows`.
pub fn rows(count: u64) -> String {
    let unit = if count == 1 { "row" } else { "rows" };
    format!("{} {unit}", grouped(count))
}

/// A count with its thousands set apart: `48,210`.
pub fn grouped(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `CREATE` → `Create`.
fn capitalized(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    let mut characters = lower.chars();
    match characters.next() {
        Some(first) => first.to_ascii_uppercase().to_string() + characters.as_str(),
        None => String::new(),
    }
}

/// The statement's first line that is not blank, cut to fit a tooltip.
fn first_line(sql: &str) -> Option<String> {
    let line = sql.lines().map(str::trim).find(|line| !line.is_empty())?;
    if line.chars().count() <= TOOLTIP_STATEMENT {
        return Some(line.to_string());
    }
    let cut: String = line.chars().take(TOOLTIP_STATEMENT - 1).collect();
    Some(format!("{}…", cut.trim_end()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn result(rows: usize, columns: usize, affected: Option<u64>) -> QueryResult {
        QueryResult {
            columns: (0..columns).map(|i| format!("c{i}")).collect(),
            column_types: vec!["TEXT".into(); columns],
            rows: vec![vec![Some("x".into()); columns]; rows],
            elapsed: Duration::from_millis(12),
            affected,
            messages: Vec::new(),
        }
    }

    #[test]
    fn a_read_is_named_by_its_table_and_rows() {
        let label = ResultLabel::for_result(
            &result(42, 2, None),
            Some("select *\nfrom users"),
            Engine::Postgres,
        );
        assert_eq!(label.label, "users · 42 rows");
        assert_eq!(label.tooltip, "select *\n12 ms");

        let one = ResultLabel::for_result(&result(1, 1, None), Some("select 1"), Engine::Sqlite);
        assert_eq!(one.label, "1 row");
    }

    #[test]
    fn a_write_is_named_by_what_it_changed() {
        let update = ResultLabel::for_result(
            &result(0, 0, Some(3)),
            Some("update items set a = 1"),
            Engine::MySql,
        );
        assert_eq!(update.label, "items · 3 updated");
        let insert = ResultLabel::for_result(
            &result(0, 0, Some(1)),
            Some("insert into items values (1)"),
            Engine::Sqlite,
        );
        assert_eq!(insert.label, "items · 1 inserted");
        let delete = ResultLabel::for_result(
            &result(0, 0, Some(1200)),
            Some("delete from items"),
            Engine::Sqlite,
        );
        assert_eq!(delete.label, "items · 1,200 deleted");
        // A `RETURNING` write is described by the rows it returned.
        let returning = ResultLabel::for_result(
            &result(2, 1, Some(2)),
            Some("delete from items returning id"),
            Engine::Postgres,
        );
        assert_eq!(returning.label, "items · 2 rows");
    }

    #[test]
    fn a_statement_with_nothing_to_count_is_named_by_its_verb() {
        let create = ResultLabel::for_result(
            &result(0, 0, Some(0)),
            Some("create table x (a int)"),
            Engine::Sqlite,
        );
        assert_eq!(create.label, "Create");
        let unknown = ResultLabel::for_result(&result(0, 0, None), None, Engine::Sqlite);
        assert_eq!(unknown.label, "0 rows");
        assert_eq!(unknown.tooltip, "12 ms");
    }

    #[test]
    fn failures_say_error_in_words() {
        assert_eq!(ResultLabel::for_failures(1).label, "Error");
        assert_eq!(ResultLabel::for_failures(3).label, "3 errors");
    }

    #[test]
    fn a_long_first_line_is_cut() {
        let sql = format!("select {} from t", "a, ".repeat(60));
        let label = ResultLabel::for_result(&result(0, 1, None), Some(&sql), Engine::Sqlite);
        let line = label.tooltip.lines().next().unwrap_or_default();
        assert!(line.ends_with('…'), "{line}");
        assert!(line.chars().count() <= TOOLTIP_STATEMENT, "{line}");
    }

    #[test]
    fn counts_are_grouped_by_thousands() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(48_210), "48,210");
        assert_eq!(grouped(1_234_567), "1,234,567");
        assert_eq!(rows(1), "1 row");
        assert_eq!(rows(2000), "2,000 rows");
    }
}
