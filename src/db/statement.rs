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

/// One statement of a buffer, and where it sits in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The statement itself, without the semicolon that ended it.
    pub text: String,
    /// Byte range of `text` in the buffer it came from.
    pub start: usize,
    pub end: usize,
}

/// Split `sql` into the statements it holds.
///
/// Empty statements — a stray semicolon, trailing whitespace — are left out,
/// so running the result runs exactly what the user wrote.
pub fn split(sql: &str) -> Vec<Statement> {
    statements(sql)
        .into_iter()
        .filter_map(|(_, start, end)| {
            let text = sql[start..end].trim();
            if text.is_empty() {
                return None;
            }

            // Trimming moved the edges, so the range follows the text.
            let offset = sql[start..end].find(text).unwrap_or_default();
            Some(Statement {
                text: text.to_string(),
                start: start + offset,
                end: start + offset + text.len(),
            })
        })
        .collect()
}

/// The statement the caret at `cursor` is in.
///
/// A caret sitting between statements — on the blank line after one — belongs
/// to the statement before it, the way running the line you just typed does.
pub fn at_cursor(sql: &str, cursor: usize) -> Option<Statement> {
    let statements = split(sql);
    statements
        .iter()
        .find(|statement| cursor >= statement.start && cursor <= statement.end)
        .or_else(|| {
            statements
                .iter()
                .rev()
                .find(|statement| statement.end <= cursor)
        })
        .or_else(|| statements.first())
        .cloned()
}

/// Option words an `EXPLAIN` header can carry before the statement it
/// explains: Postgres' parenthesised options and MySQL's bare ones, with the
/// words that name their values (`FORMAT TREE`, `FORMAT=JSON`).
const EXPLAIN_OPTIONS: [&str; 17] = [
    "ANALYZE",
    "VERBOSE",
    "COSTS",
    "SETTINGS",
    "BUFFERS",
    "WAL",
    "TIMING",
    "SUMMARY",
    "FORMAT",
    "TREE",
    "JSON",
    "TRADITIONAL",
    "EXTENDED",
    "PARTITIONS",
    "XML",
    "YAML",
    "GENERIC_PLAN",
];

/// The statement an `EXPLAIN` explains, and whether running it runs that
/// statement too.
///
/// `None` when `sql` is not an `EXPLAIN` at all, so the caller knows it has a
/// plain statement to wrap itself. `Some((inner, analyzes))` otherwise: `inner`
/// is the statement after the header, which is what the classifier is asked
/// about, and `analyzes` is true when the header asks for `ANALYZE`, so the
/// inner statement really runs. Both `EXPLAIN ...` and MariaDB's `ANALYZE
/// FORMAT=JSON ...` spelling are recognised.
pub fn explained(sql: &str) -> Option<(String, bool)> {
    let characters: Vec<char> = sql.chars().collect();
    let offsets: Vec<usize> = sql
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(sql.len()))
        .collect();
    let byte = |index: usize| offsets.get(index).copied().unwrap_or(sql.len());

    let mut index = skip_trivia(&characters, 0);
    let (word, next) = word_at(&characters, index)?;
    let first = word.to_ascii_uppercase();
    if first != "EXPLAIN" && first != "ANALYZE" {
        return None;
    }

    let mut analyzes = first == "ANALYZE";
    index = next;

    loop {
        index = skip_trivia(&characters, index);
        match characters.get(index) {
            Some('(') => {
                let (found, next) = skip_parens(&characters, index);
                analyzes |= found;
                index = next;
            }
            Some('=') => index += 1,
            Some(_) => {
                if let Some((word, next)) = word_at(&characters, index) {
                    let upper = word.to_ascii_uppercase();
                    if EXPLAIN_OPTIONS.contains(&upper.as_str()) {
                        analyzes |= upper == "ANALYZE";
                        index = next;
                        continue;
                    }
                }
                return Some((sql[byte(index)..].trim().to_string(), analyzes));
            }
            // An `EXPLAIN` with nothing after it explains nothing.
            None => return Some((String::new(), analyzes)),
        }
    }
}

/// The first statement in `sql` that is not plainly a read.
///
/// The answer is the word the statement starts with, upper-cased, for a
/// message that can say what was refused. `None` means every statement in the
/// buffer reads.
pub fn first_write(sql: &str) -> Option<String> {
    statements(sql).into_iter().find_map(|(words, _, _)| {
        if reads(&words) {
            return None;
        }
        Some(
            words
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

/// Split `sql` into statements: the words each one is made of, upper-cased,
/// and the byte range it covers.
///
/// Comments and the insides of quoted strings and identifiers are dropped on
/// the way through, so neither a semicolon nor a keyword hiding in one can
/// change the answer. Words keep only the characters an identifier can have;
/// `=` is the one piece of punctuation kept, for `PRAGMA`.
fn statements(sql: &str) -> Vec<(Vec<String>, usize, usize)> {
    let mut statements = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let characters: Vec<char> = sql.chars().collect();
    // Byte offset of each character, so a range can be cut out of `sql` again.
    let offsets: Vec<usize> = sql
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(sql.len()))
        .collect();
    let byte = |index: usize| offsets.get(index).copied().unwrap_or(sql.len());
    let mut start = 0;
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
                statements.push((std::mem::take(&mut words), byte(start), byte(index)));
                index += 1;
                start = index;
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
    // Whatever follows the last semicolon is a statement of its own, even
    // without one to end it.
    if start < characters.len() {
        statements.push((words, byte(start), sql.len()));
    }
    statements
}

/// Index of the first character that is not whitespace, a comment, or blank
/// space left by one.
fn skip_trivia(characters: &[char], mut index: usize) -> usize {
    loop {
        while index < characters.len() && characters[index].is_whitespace() {
            index += 1;
        }
        match characters
            .get(index)
            .copied()
            .zip(characters.get(index + 1).copied())
        {
            Some(('-', '-')) => index = skip_until(characters, index, "\n"),
            Some(('/', '*')) => index = skip_until(characters, index + 2, "*/"),
            // MySQL's `#` comment.
            Some(('#', _)) => index = skip_until(characters, index, "\n"),
            _ => return index,
        }
    }
}

/// The word starting at `index`, upper-cased by the caller, and where it ends.
/// `None` when there is no word there.
fn word_at(characters: &[char], start: usize) -> Option<(String, usize)> {
    let mut index = start;
    let mut word = String::new();
    while let Some(character) = characters.get(index) {
        if character.is_alphanumeric() || *character == '_' {
            word.push(*character);
            index += 1;
        } else {
            break;
        }
    }
    if word.is_empty() {
        None
    } else {
        Some((word, index))
    }
}

/// Skip a parenthesised option list, answering whether it asked for `ANALYZE`,
/// and the index just past its closing `)`.
fn skip_parens(characters: &[char], start: usize) -> (bool, usize) {
    let mut depth = 0usize;
    let mut analyzes = false;
    let mut index = start;
    while index < characters.len() {
        match characters[index] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return (analyzes, index + 1);
                }
            }
            quote @ ('\'' | '"' | '`') => {
                index = skip_quoted(characters, index, quote);
                continue;
            }
            character if character.is_alphanumeric() || character == '_' => {
                let (word, next) = word_at(characters, index).expect("a word starts here");
                analyzes |= word.eq_ignore_ascii_case("analyze");
                index = next;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    (analyzes, index)
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
    fn a_buffer_splits_into_its_statements() {
        let sql = "select 1;\nselect 2\n";
        let statements = split(sql);
        assert_eq!(
            statements
                .iter()
                .map(|statement| statement.text.as_str())
                .collect::<Vec<_>>(),
            ["select 1", "select 2"]
        );
        // The range is where the statement sits in the buffer it came from.
        assert_eq!(&sql[statements[1].start..statements[1].end], "select 2");

        // A semicolon inside a string does not end anything.
        assert_eq!(split("select 'a; b'").len(), 1);
        // Stray semicolons and blank space run nothing.
        assert!(split(" ; \n ;").is_empty());
    }

    #[test]
    fn the_caret_picks_the_statement_it_is_in() {
        let sql = "select 1;\nselect 2;\nselect 3;";
        let at = |cursor| at_cursor(sql, cursor).map(|statement| statement.text);

        assert_eq!(at(0).as_deref(), Some("select 1"));
        assert_eq!(at(3).as_deref(), Some("select 1"));
        // Between two statements: the one that was just typed.
        assert_eq!(at(9).as_deref(), Some("select 1"));
        assert_eq!(at(12).as_deref(), Some("select 2"));
        assert_eq!(at(sql.len()).as_deref(), Some("select 3"));
        assert_eq!(at_cursor("", 0), None);
    }

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
    fn an_explain_header_is_split_off_what_it_explains() {
        let split = |sql: &str| explained(sql);

        assert_eq!(split("select 1"), None);
        assert_eq!(
            split("explain select * from items"),
            Some(("select * from items".to_string(), false))
        );
        assert_eq!(
            split("EXPLAIN ANALYZE select * from items"),
            Some(("select * from items".to_string(), true))
        );
        // Postgres' parenthesised options, in any order.
        assert_eq!(
            split("explain (analyze, buffers, format json) select 1"),
            Some(("select 1".to_string(), true))
        );
        assert_eq!(
            split("explain (format json) select 1"),
            Some(("select 1".to_string(), false))
        );
        // MySQL's bare options, and MariaDB's `ANALYZE` spelling.
        assert_eq!(
            split("explain format=tree select 1"),
            Some(("select 1".to_string(), false))
        );
        assert_eq!(
            split("analyze format=json select 1"),
            Some(("select 1".to_string(), true))
        );
        // The statement itself is what the classifier has to see.
        assert_eq!(
            split("explain analyze delete from items"),
            Some(("delete from items".to_string(), true))
        );
        assert_eq!(
            split("explain delete from items"),
            Some(("delete from items".to_string(), false))
        );
        assert_eq!(split("explain"), Some((String::new(), false)));
    }

    #[test]
    fn an_unknown_statement_counts_as_a_write() {
        // Better to refuse something harmless than to run something that is
        // not: the user can always switch the connection's mode.
        assert!(first_write("call do_something()").is_some());
        assert!(first_write("begin").is_some());
    }
}
