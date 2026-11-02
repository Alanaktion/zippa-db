//! Where in a statement the server says it went wrong.
//!
//! Each engine says it differently: Postgres reports a character `position`
//! on most syntax and name errors, MySQL ends its syntax errors with
//! `near '…' at line N`, and SQLite's say `near "…": syntax error`. Read
//! into one [`ErrorSpot`] and resolved against the SQL that was sent, they
//! give a byte offset the editor can mark and move the caret to. A failure
//! with none of these (a constraint, a missing permission) has no spot, and
//! the editor marks the whole statement instead.

use std::sync::LazyLock;

use regex::Regex;

/// A place in the SQL that was sent, as the server described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorSpot {
    /// A zero-based character index (Postgres' `position`, which counts
    /// characters from one).
    Char(usize),
    /// A one-based line, and the text the server says the error is near,
    /// which begins at the error (MySQL). `near` is empty at the end of the
    /// input.
    Line { line: usize, near: String },
    /// Only the text the error is near (SQLite).
    Near(String),
}

static MYSQL_NEAR: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "a fixed pattern")]
    Regex::new(r"(?s)near '(.*)' at line (\d+)\s*$").expect("a valid pattern")
});

static SQLITE_NEAR: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "a fixed pattern")]
    Regex::new(r#"near "((?:[^"]|"")*)": syntax error"#).expect("a valid pattern")
});

/// The spot `error` names, if the server named one.
pub fn spot(error: &anyhow::Error) -> Option<ErrorSpot> {
    for cause in error.chain() {
        let Some(sqlx::Error::Database(database)) = cause.downcast_ref::<sqlx::Error>() else {
            continue;
        };
        if let Some(postgres) = database.try_downcast_ref::<sqlx::postgres::PgDatabaseError>() {
            return match postgres.position()? {
                sqlx::postgres::PgErrorPosition::Original(at) => {
                    Some(ErrorSpot::Char(at.saturating_sub(1)))
                }
                // A position inside a query the server generated (a
                // function's body) is not in what the user wrote.
                sqlx::postgres::PgErrorPosition::Internal { .. } => None,
            };
        }
        return from_message(database.message());
    }
    None
}

/// The spot a MySQL or SQLite message names.
pub fn from_message(message: &str) -> Option<ErrorSpot> {
    if let Some(found) = MYSQL_NEAR.captures(message) {
        let line = found[2].parse().ok()?;
        return Some(ErrorSpot::Line {
            line,
            near: found[1].to_string(),
        });
    }
    let found = SQLITE_NEAR.captures(message)?;
    Some(ErrorSpot::Near(found[1].replace("\"\"", "\"")))
}

impl ErrorSpot {
    /// The byte offset in `sent` (the SQL as it went to the server) this
    /// spot points at, if it can be found there.
    pub fn resolve(&self, sent: &str) -> Option<usize> {
        match self {
            ErrorSpot::Char(at) => Some(
                sent.char_indices()
                    .nth(*at)
                    .map(|(byte, _)| byte)
                    .unwrap_or(sent.len()),
            ),
            ErrorSpot::Line { line, near } => {
                let start = line_start(sent, *line)?;
                if near.is_empty() {
                    return Some(sent.trim_end().len().max(start));
                }
                // MySQL cuts the quoted text at 80 characters, so a prefix of
                // it is what to look for. The quoted text runs to the end of
                // the statement, so it can only start once in what follows
                // the line's start; where it seems to start more than once
                // (a prefix cut short), the spot is unknown.
                let needle: String = near.chars().take(40).collect();
                unique(&sent[start..], &needle).map(|at| start + at)
            }
            // SQLite names only the token. Where it appears more than once,
            // which one failed is unknown, and marking the first could point
            // at the wrong place.
            ErrorSpot::Near(near) if !near.is_empty() => unique(sent, near),
            ErrorSpot::Near(_) => None,
        }
    }
}

/// Where `needle` is in `haystack`, if it is there exactly once.
fn unique(haystack: &str, needle: &str) -> Option<usize> {
    let mut found = haystack.match_indices(needle).map(|(at, _)| at);
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// Byte offset of the start of one-based line `line`.
fn line_start(text: &str, line: usize) -> Option<usize> {
    if line <= 1 {
        return Some(0);
    }
    text.match_indices('\n').nth(line - 2).map(|(at, _)| at + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_position_counts_characters_not_bytes() {
        assert_eq!(ErrorSpot::Char(7).resolve("select frm t"), Some(7));
        // "é" is two bytes.
        assert_eq!(ErrorSpot::Char(3).resolve("'é' x"), Some(4));
        assert_eq!(ErrorSpot::Char(99).resolve("short"), Some(5));
    }

    #[test]
    fn mysql_names_a_line_and_the_text_after_the_error() {
        let message = "You have an error in your SQL syntax; check the manual that corresponds \
                       to your MySQL server version for the right syntax to use near 'frm t' at line 2";
        let spot = from_message(message).expect("a spot");
        assert_eq!(
            spot,
            ErrorSpot::Line {
                line: 2,
                near: "frm t".into()
            }
        );
        assert_eq!(spot.resolve("select a,\n  b frm t"), Some(14));
        // At the end of the input the quoted text is empty.
        let end = from_message("… to use near '' at line 1").expect("a spot");
        assert_eq!(end.resolve("select (1  "), Some(9));
    }

    #[test]
    fn sqlite_names_the_token() {
        let spot = from_message(r#"near "frm": syntax error"#).expect("a spot");
        assert_eq!(spot, ErrorSpot::Near("frm".into()));
        assert_eq!(spot.resolve("select 1 frm t"), Some(9));
        assert_eq!(from_message("no such table: t"), None);
        // A token that appears twice could be either.
        assert_eq!(
            ErrorSpot::Near("FROM".into()).resolve("SELECT * FROM t FROM u"),
            None
        );
    }

    #[test]
    fn text_the_server_quotes_but_the_sql_lacks_is_no_spot() {
        let spot = ErrorSpot::Line {
            line: 1,
            near: "nothing like it".into(),
        };
        assert_eq!(spot.resolve("select frm t"), None);
    }
}
