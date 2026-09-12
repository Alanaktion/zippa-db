//! Telling reading SQL from writing SQL.
//!
//! Used by the read-only and confirm-before-write safety modes to answer one
//! question about a buffer the user typed: does running it change anything?
//! The answer is deliberately pessimistic — anything this cannot recognise as
//! a read counts as a write, so an unknown statement is refused rather than
//! run. It is a guard on top of the server's own read-only session, not a
//! substitute for it: a `select` that calls a function which writes still
//! reads like a read from here.

/// Statements that are allowed to run on a read-only connection.
///
/// Everything else, including transaction control and `SET`, is a write as
/// far as this module is concerned.
const READING: [&str; 8] = [
    "SELECT", "VALUES", "TABLE", "SHOW", "DESCRIBE", "DESC", "EXPLAIN", "PRAGMA",
];

/// Keywords that make a statement a write wherever they turn up in it, so a
/// `WITH ... INSERT` or an `EXPLAIN ANALYZE DELETE` is not mistaken for a read.
const WRITING: [&str; 4] = ["INSERT", "UPDATE", "DELETE", "MERGE"];

/// The first statement in `sql` that is not plainly a read.
///
/// The answer is the word the statement starts with, upper-cased, for a
/// message that can say what was refused. `None` means every statement in the
/// buffer reads.
pub fn first_write(sql: &str) -> Option<String> {
    statements(sql).into_iter().find_map(|statement| {
        if reads(&statement) {
            return None;
        }
        Some(
            statement
                .first()
                .cloned()
                .unwrap_or_else(|| "statement".to_string()),
        )
    })
}

/// Whether one statement, already reduced to its words, only reads.
fn reads(words: &[String]) -> bool {
    let Some(first) = words.first() else {
        // An empty statement — a stray semicolon — runs nothing.
        return true;
    };

    if !READING.contains(&first.as_str()) {
        return false;
    }

    // `EXPLAIN ANALYZE` runs the statement it explains, and `WITH` can carry a
    // write in a branch, so a reading first word is not the whole answer.
    if words
        .iter()
        .any(|word| WRITING.contains(&word.as_str()) || word == "ANALYZE")
    {
        return false;
    }

    // `PRAGMA foo = bar` sets it; `PRAGMA table_info(items)` reads it.
    if first == "PRAGMA" && words.iter().any(|word| word == "=") {
        return false;
    }

    true
}

/// Split `sql` into statements, each one the words it is made of, upper-cased.
///
/// Comments and the insides of quoted strings and identifiers are dropped on
/// the way through, so neither a semicolon nor a keyword hiding in one can
/// change the answer. Words keep only the characters an identifier can have;
/// `=` is the one piece of punctuation kept, for `PRAGMA`.
fn statements(sql: &str) -> Vec<Vec<String>> {
    let mut statements = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let characters: Vec<char> = sql.chars().collect();
    let mut index = 0;

    // A word ends wherever the character after it cannot continue it.
    macro_rules! end_word {
        () => {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word).to_ascii_uppercase());
            }
        };
    }

    while index < characters.len() {
        let character = characters[index];
        let next = characters.get(index + 1).copied();

        match character {
            // Line comments run to the end of the line; `#` is MySQL's.
            '-' if next == Some('-') => {
                end_word!();
                index = skip_until(&characters, index, "\n");
            }
            '#' => {
                end_word!();
                index = skip_until(&characters, index, "\n");
            }
            '/' if next == Some('*') => {
                end_word!();
                index = skip_until(&characters, index + 2, "*/");
            }
            // Quoted text of any kind is opaque: it is a value or a name, not
            // a keyword, and it can hold anything at all.
            '\'' | '"' | '`' => {
                end_word!();
                index = skip_quoted(&characters, index, character);
            }
            // Postgres dollar quoting: `$tag$ ... $tag$`.
            '$' if dollar_tag(&characters, index).is_some() => {
                end_word!();
                let tag = dollar_tag(&characters, index).expect("checked just above");
                index = skip_until(&characters, index + tag.len(), &tag);
            }
            ';' => {
                end_word!();
                statements.push(std::mem::take(&mut words));
                index += 1;
            }
            '=' => {
                end_word!();
                words.push("=".to_string());
                index += 1;
            }
            character if character.is_alphanumeric() || character == '_' => {
                word.push(character);
                index += 1;
            }
            _ => {
                end_word!();
                index += 1;
            }
        }
    }

    end_word!();
    if !words.is_empty() {
        statements.push(words);
    }
    statements
}

/// Index just past the next `terminator` at or after `from`, or the end.
fn skip_until(characters: &[char], from: usize, terminator: &str) -> usize {
    let terminator: Vec<char> = terminator.chars().collect();
    let mut index = from;
    while index < characters.len() {
        if characters[index..].starts_with(terminator.as_slice()) {
            return index + terminator.len();
        }
        index += 1;
    }
    characters.len()
}

/// Index just past the closing `quote`, treating a doubled quote as an escape.
fn skip_quoted(characters: &[char], from: usize, quote: char) -> usize {
    let mut index = from + 1;
    while index < characters.len() {
        if characters[index] == '\\' {
            // MySQL escapes with a backslash; Postgres and SQLite do not, and
            // skipping the next character is harmless either way inside a
            // string whose contents are thrown away.
            index += 2;
            continue;
        }
        if characters[index] == quote {
            if characters.get(index + 1) == Some(&quote) {
                index += 2;
                continue;
            }
            return index + 1;
        }
        index += 1;
    }
    characters.len()
}

/// The `$tag$` opening a dollar-quoted string at `index`, if there is one.
fn dollar_tag(characters: &[char], index: usize) -> Option<String> {
    let mut tag = String::from("$");
    for character in &characters[index + 1..] {
        match character {
            '$' => {
                tag.push('$');
                return Some(tag);
            }
            character if character.is_alphanumeric() || *character == '_' => tag.push(*character),
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_reads_are_reads() {
        for sql in [
            "select * from items",
            "SELECT 1; select 2;",
            "show tables",
            "explain select * from items",
            "pragma table_info(items)",
            "values (1), (2)",
            "  \n-- a comment\nselect 1",
            "",
            ";",
        ] {
            assert_eq!(first_write(sql), None, "{sql} should read");
        }
    }

    #[test]
    fn writes_are_named() {
        assert_eq!(
            first_write("insert into items values (1)").as_deref(),
            Some("INSERT")
        );
        assert_eq!(
            first_write("select 1; drop table items").as_deref(),
            Some("DROP")
        );
        assert_eq!(
            first_write("SET GLOBAL max_connections = 10").as_deref(),
            Some("SET")
        );
        assert_eq!(
            first_write("alter table items add column x int").as_deref(),
            Some("ALTER")
        );
        assert_eq!(first_write("truncate items").as_deref(), Some("TRUNCATE"));
    }

    #[test]
    fn a_write_hiding_behind_a_reading_keyword_is_found() {
        // A CTE can carry the write, and `explain analyze` runs what it
        // explains, so neither first word settles it.
        assert!(
            first_write("with gone as (delete from items returning *) select * from gone")
                .is_some()
        );
        assert!(first_write("explain analyze delete from items").is_some());
        assert!(first_write("pragma journal_mode = wal").is_some());
    }

    #[test]
    fn quoted_text_is_not_sql() {
        // A keyword inside a value or a name is just characters.
        assert_eq!(first_write("select 'drop table items' as warning"), None);
        assert_eq!(first_write(r#"select "drop" from items"#), None);
        assert_eq!(first_write("select $tag$ delete from items $tag$"), None);
        // A semicolon inside a string does not start a new statement.
        assert_eq!(first_write("select 'a; drop table items'"), None);
    }

    #[test]
    fn a_comment_cannot_hide_a_write() {
        assert_eq!(first_write("select 1 -- drop table items"), None);
        assert_eq!(first_write("/* drop table items */ select 1"), None);
        assert_eq!(
            first_write("/* comment */ delete from items").as_deref(),
            Some("DELETE")
        );
    }

    #[test]
    fn an_unknown_statement_counts_as_a_write() {
        // Better to refuse something harmless than to run something that is
        // not: the user can always switch the connection's mode.
        assert!(first_write("call do_something()").is_some());
        assert!(first_write("begin").is_some());
    }
}
