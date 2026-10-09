//! Telling reading SQL from writing SQL.
//!
//! Used by the read-only and confirm-before-write safety modes to answer one
//! question about a buffer the user typed: does running it change anything?
//! The answer is deliberately pessimistic — anything this cannot recognise as
//! a read counts as a write, so an unknown statement is refused rather than
//! run. It is a guard on top of the server's own read-only session, not a
//! substitute for it: a `select` that calls a function which writes still
//! reads like a read from here.

use super::config::Engine;

/// Statements that are allowed to run on a read-only connection.
///
/// Transaction control is allowed as well (see [`transaction_control`]);
/// everything else, `SET` included, is a write as far as this module is
/// concerned.
const READING: [&str; 9] = [
    "SELECT", "WITH", "VALUES", "TABLE", "SHOW", "DESCRIBE", "DESC", "EXPLAIN", "PRAGMA",
];

/// Keywords that make a statement a write wherever they turn up in it, so a
/// `WITH ... INSERT` or an `EXPLAIN ANALYZE DELETE` is not mistaken for a
/// read — and so is `INTO`, catching Postgres' `SELECT ... INTO new_table`
/// (creates a table) and MySQL's `SELECT ... INTO OUTFILE`/`INTO DUMPFILE`
/// (writes a file): both start with the reading keyword `SELECT` and would
/// otherwise read as a plain read, which matters most for `ConfirmWrites` —
/// unlike `ReadOnly`, it has no server-side backstop, so a statement this
/// classifier misses as a write runs with no confirmation at all rather than
/// merely with a less specific error message.
const WRITING: [&str; 5] = ["INSERT", "UPDATE", "DELETE", "MERGE", "INTO"];

/// One statement of a buffer, and where it sits in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    /// The statement itself, without the semicolon that ended it.
    pub text: String,
    /// Byte range of `text` in the buffer it came from.
    pub start: usize,
    pub end: usize,
}

/// A `:name` placeholder in user SQL, and where it sits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    /// The name without the colon.
    pub name: String,
    /// Byte range of `:name` in the SQL it came from.
    pub start: usize,
    pub end: usize,
}

/// Split `sql` into the statements it holds.
///
/// Empty statements — a stray semicolon, trailing whitespace — are left out,
/// so running the result runs exactly what the user wrote.
pub fn split(sql: &str, engine: Engine) -> Vec<Statement> {
    statements(sql, engine)
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
pub fn at_cursor(sql: &str, cursor: usize, engine: Engine) -> Option<Statement> {
    let statements = split(sql, engine);
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

/// Every `:name` placeholder in `sql`, outside strings and comments.
///
/// A colon starts a placeholder when the character before it is not part of
/// a word or another colon — so `::` casts, `:=` assignments, and `12:30`
/// are left alone — and the character after it starts a name.
pub fn placeholders(sql: &str, engine: Engine) -> Vec<Placeholder> {
    let characters: Vec<char> = sql.chars().collect();
    // Byte index of each character, so the ranges are byte ranges.
    let mut bytes = Vec::with_capacity(characters.len() + 1);
    let mut byte = 0;
    for character in &characters {
        bytes.push(byte);
        byte += character.len_utf8();
    }
    bytes.push(byte);

    let mut found = Vec::new();
    let mut index = 0;
    while index < characters.len() {
        match characters[index] {
            quote @ ('\'' | '"' | '`') => {
                index = skip_quoted(&characters, index, quote, engine);
            }
            '$' => {
                // A dollar-quoted string or a positional parameter: no
                // `:name` starts inside either.
                if let Some(tag) = dollar_tag(&characters, index) {
                    index = skip_until(&characters, index + tag.len(), &tag);
                } else {
                    index += 1;
                    while index < characters.len() && characters[index].is_ascii_digit() {
                        index += 1;
                    }
                }
            }
            '-' if characters.get(index + 1) == Some(&'-') => {
                index = skip_until(&characters, index, "\n");
            }
            '/' if characters.get(index + 1) == Some(&'*') => {
                index = skip_until(&characters, index + 2, "*/");
            }
            '#' if engine == Engine::MySql => {
                index = skip_until(&characters, index, "\n");
            }
            ':' => {
                let prev = if index > 0 {
                    Some(characters[index - 1])
                } else {
                    None
                };
                let prev_is_word = prev.is_some_and(|prev| {
                    prev.is_alphanumeric() || prev == '_' || prev == ':' || prev == '$'
                });
                let next = characters.get(index + 1).copied();
                let next_starts_name = next.is_some_and(|next| next.is_alphabetic() || next == '_');
                if !prev_is_word && next_starts_name {
                    let mut end = index + 2;
                    while end < characters.len()
                        && (characters[end].is_alphanumeric() || characters[end] == '_')
                    {
                        end += 1;
                    }
                    let name = characters[index + 1..end].iter().collect();
                    found.push(Placeholder {
                        name,
                        start: bytes[index],
                        end: bytes[end],
                    });
                    index = end;
                } else {
                    index += 1;
                }
            }
            _ => {
                index += 1;
            }
        }
    }
    found
}

/// Option words an `EXPLAIN` header can carry before the statement it
/// explains: Postgres' parenthesised options and MySQL's bare ones, with the
/// words that name their values (`FORMAT TREE`, `FORMAT=JSON`).
const EXPLAIN_OPTIONS: [&str; 18] = [
    "ANALYZE",
    "ANALYSE",
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
pub fn explained(sql: &str, engine: Engine) -> Option<(String, bool)> {
    let characters: Vec<char> = sql.chars().collect();
    let offsets: Vec<usize> = sql
        .char_indices()
        .map(|(offset, _)| offset)
        .chain(std::iter::once(sql.len()))
        .collect();
    let byte = |index: usize| offsets.get(index).copied().unwrap_or(sql.len());

    let mut index = skip_trivia(&characters, 0, engine);
    let (word, next) = word_at(&characters, index)?;
    let first = word.to_ascii_uppercase();
    if first != "EXPLAIN" && first != "ANALYZE" {
        return None;
    }

    let mut analyzes = first == "ANALYZE";
    index = next;

    loop {
        index = skip_trivia(&characters, index, engine);
        match characters.get(index) {
            Some('(') => {
                let (found, next) = skip_parens(&characters, index, engine);
                analyzes |= found;
                index = next;
            }
            Some('=') => index += 1,
            Some(_) => {
                if let Some((word, next)) = word_at(&characters, index) {
                    let upper = word.to_ascii_uppercase();
                    if EXPLAIN_OPTIONS.contains(&upper.as_str()) {
                        analyzes |= is_analyze(&upper);
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
pub fn first_write(sql: &str, engine: Engine) -> Option<String> {
    statements(sql, engine)
        .into_iter()
        .find_map(|(words, _, _)| {
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

/// The first `limit` words of the first statement in `sql`, upper-cased.
///
/// Comments and quoted text are left out the way [`first_write`] leaves them
/// out, so `/* note */ lock tables` starts with `LOCK`.
pub fn leading_words(sql: &str, limit: usize, engine: Engine) -> Vec<String> {
    statements(sql, engine)
        .into_iter()
        .next()
        .map(|(words, _, _)| words.into_iter().take(limit).collect())
        .unwrap_or_default()
}

/// Whether one statement, already reduced to its words, only opens, ends, or
/// marks a point in a transaction.
///
/// None of these write data themselves, so a read-only connection can run
/// them (the server keeps the transaction read-only) and a careful one does
/// not ask about them: a `COMMIT` only keeps writes that were confirmed when
/// they ran. Two forms are left out on purpose. `READ WRITE` would lift the
/// read-only session a read-only connection is opened with, and `PREPARED`
/// (`COMMIT PREPARED 'x'`, `PREPARE TRANSACTION`) acts on a two-phase
/// transaction this tab never saw the writes of.
fn transaction_control(words: &[String]) -> bool {
    let word = |index: usize| words.get(index).map(String::as_str).unwrap_or("");
    let control = match word(0) {
        "BEGIN" | "COMMIT" | "END" | "ROLLBACK" | "ABORT" | "SAVEPOINT" | "RELEASE" => true,
        "START" | "SET" => word(1) == "TRANSACTION",
        _ => false,
    };
    control
        && !words
            .iter()
            .any(|word| word == "WRITE" || word == "PREPARED" || WRITING.contains(&word.as_str()))
}

/// Whether one statement, already reduced to its words, only reads.
fn reads(words: &[String]) -> bool {
    let Some(first) = words.first() else {
        // An empty statement — a stray semicolon — runs nothing.
        return true;
    };

    if transaction_control(words) {
        return true;
    }

    if !READING.contains(&first.as_str()) {
        return false;
    }

    // `EXPLAIN ANALYZE` (or Postgres' `ANALYSE`) runs the statement it
    // explains, and `WITH` can carry a write in a branch, so a reading first
    // word is not the whole answer.
    if words
        .iter()
        .any(|word| WRITING.contains(&word.as_str()) || is_analyze(word))
    {
        return false;
    }

    // `PRAGMA foo = bar` sets it, and so does `PRAGMA foo(bar)` — unless
    // `foo` is one of the pragmas whose argument says what to read:
    // `PRAGMA table_info(items)`.
    if first == "PRAGMA" {
        if words.iter().any(|word| word == "=") {
            return false;
        }
        if let Some(paren) = words.iter().position(|word| word == "(") {
            let name = paren
                .checked_sub(1)
                .and_then(|index| words.get(index))
                .map(String::as_str)
                .unwrap_or("");
            return READING_PRAGMAS.contains(&name);
        }
    }

    true
}

/// `ANALYZE`, either spelling: Postgres takes the British one too.
fn is_analyze(word: &str) -> bool {
    word.eq_ignore_ascii_case("ANALYZE") || word.eq_ignore_ascii_case("ANALYSE")
}

/// The SQLite pragmas whose parenthesised argument names what to read rather
/// than a value to set.
const READING_PRAGMAS: [&str; 9] = [
    "TABLE_INFO",
    "TABLE_XINFO",
    "TABLE_LIST",
    "INDEX_LIST",
    "INDEX_INFO",
    "INDEX_XINFO",
    "FOREIGN_KEY_LIST",
    "FOREIGN_KEY_CHECK",
    "INTEGRITY_CHECK",
];

/// Split `sql` into statements: the words each one is made of, upper-cased,
/// and the byte range it covers.
///
/// Comments and the insides of quoted strings and identifiers are dropped on
/// the way through, so neither a semicolon nor a keyword hiding in one can
/// change the answer. Words keep only the characters an identifier can have;
/// `=` is the one piece of punctuation kept, for `PRAGMA`.
/// `engine` decides the two places the dialects part ways: `#` starts a
/// comment only on MySQL (on Postgres it is an operator: `#>>`), and a
/// backslash escapes a quote only on MySQL or inside a Postgres `E'…'` string
/// — read everywhere, `'C:\'` on SQLite would swallow the statements after
/// it into one.
fn statements(sql: &str, engine: Engine) -> Vec<(Vec<String>, usize, usize)> {
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
            '#' if engine == Engine::MySql => {
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
                index = skip_quoted(&characters, index, character, engine);
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
            // Kept for `PRAGMA name(value)`, which sets as surely as `=` does.
            '(' if words.first().is_some_and(|first| first == "PRAGMA") => {
                end_word!();
                words.push("(".to_string());
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
fn skip_trivia(characters: &[char], mut index: usize, engine: Engine) -> usize {
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
            Some(('#', _)) if engine == Engine::MySql => {
                index = skip_until(characters, index, "\n")
            }
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
fn skip_parens(characters: &[char], start: usize, engine: Engine) -> (bool, usize) {
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
                index = skip_quoted(characters, index, quote, engine);
                continue;
            }
            character if character.is_alphanumeric() || character == '_' => {
                let (word, next) = word_at(characters, index).expect("a word starts here");
                analyzes |= is_analyze(&word);
                index = next;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    (analyzes, index)
}

/// Whether a backslash escapes the next character in the quoted text opening
/// at `from`: in any MySQL string, and in a Postgres string written `E'…'`.
/// Postgres and SQLite otherwise take a backslash as it is.
pub(crate) fn backslash_escapes(
    characters: &[char],
    from: usize,
    quote: char,
    engine: Engine,
) -> bool {
    match engine {
        Engine::MySql => quote != '`',
        Engine::Postgres => {
            quote == '\''
                && from
                    .checked_sub(1)
                    .and_then(|index| characters.get(index))
                    .is_some_and(|prefix| matches!(prefix, 'E' | 'e'))
                && from
                    .checked_sub(2)
                    .and_then(|index| characters.get(index))
                    .is_none_or(|before| !(before.is_alphanumeric() || *before == '_'))
        }
        Engine::Sqlite => false,
    }
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

/// Index just past the closing `quote`, treating a doubled quote as an escape
/// — and a backslash too, where `engine` reads one (see [`backslash_escapes`]).
fn skip_quoted(characters: &[char], from: usize, quote: char, engine: Engine) -> usize {
    let escapes = backslash_escapes(characters, from, quote, engine);
    let mut index = from + 1;
    while index < characters.len() {
        if escapes && characters[index] == '\\' {
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

    // The tests below were written before the scanner took an engine, and
    // read the buffer the way MySQL does — the dialect with the most to skip.
    // The ones that tell the dialects apart name their engine.
    fn split(sql: &str) -> Vec<Statement> {
        super::split(sql, Engine::MySql)
    }

    fn at_cursor(sql: &str, cursor: usize) -> Option<Statement> {
        super::at_cursor(sql, cursor, Engine::MySql)
    }

    fn explained(sql: &str) -> Option<(String, bool)> {
        super::explained(sql, Engine::MySql)
    }

    fn first_write(sql: &str) -> Option<String> {
        super::first_write(sql, Engine::MySql)
    }

    fn leading_words(sql: &str, limit: usize) -> Vec<String> {
        super::leading_words(sql, limit, Engine::MySql)
    }

    #[test]
    fn a_backslash_ends_nothing_on_sqlite_or_postgres() {
        let sql = "select * from files where dir = 'C:\\';\ndelete from files where id = 3;";
        for engine in [Engine::Sqlite, Engine::Postgres] {
            let statements = super::split(sql, engine);
            assert_eq!(statements.len(), 2, "{engine:?}");
            let first = super::at_cursor(sql, 3, engine).unwrap();
            assert_eq!(super::first_write(&first.text, engine), None, "{engine:?}");
            assert_eq!(
                super::first_write(sql, engine).as_deref(),
                Some("DELETE"),
                "{engine:?}"
            );
        }
        // MySQL does read it as an escape, and Postgres does inside `E''`.
        assert_eq!(super::split(sql, Engine::MySql).len(), 1);
        let escaped = "select E'it\\'s';\nselect 2;";
        assert_eq!(super::split(escaped, Engine::Postgres).len(), 2);
        let named = "select date 'x\\';\nselect 2;";
        assert_eq!(super::split(named, Engine::Postgres).len(), 2);
    }

    #[test]
    fn a_hash_is_a_comment_only_on_mysql() {
        let sql = "select data #>> '{a}' from t;\nselect 2;";
        assert_eq!(super::split(sql, Engine::Postgres).len(), 2);
        let into = "select d #>> '{a}' as x into t2 from t";
        assert_eq!(
            super::first_write(into, Engine::Postgres).as_deref(),
            Some("SELECT")
        );
        assert_eq!(
            super::split("select 1; # note; select 2", Engine::MySql).len(),
            2
        );
    }

    #[test]
    fn analyse_is_analyze() {
        assert_eq!(
            explained("explain analyse delete from items"),
            Some(("delete from items".to_string(), true))
        );
        assert_eq!(
            explained("explain (analyse) delete from items"),
            Some(("delete from items".to_string(), true))
        );
        assert!(first_write("explain analyse create table t as select 1").is_some());
    }

    #[test]
    fn a_pragma_set_with_parentheses_is_a_write() {
        for sql in [
            "PRAGMA user_version(5)",
            "pragma journal_mode(delete)",
            "PRAGMA main.writable_schema(1)",
        ] {
            assert_eq!(first_write(sql).as_deref(), Some("PRAGMA"), "{sql}");
        }
        for sql in [
            "PRAGMA table_info(items)",
            "pragma main.index_list('items')",
            "PRAGMA journal_mode",
        ] {
            assert_eq!(first_write(sql), None, "{sql}");
        }
    }

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
    fn leading_words_skip_comments_and_quotes() {
        assert_eq!(
            leading_words("/* first */ -- second\n lock Tables `t` write", 3),
            ["LOCK", "TABLES", "WRITE"]
        );
        assert_eq!(leading_words("vacuum; select 1", 5), ["VACUUM"]);
        assert!(leading_words("  -- nothing\n", 2).is_empty());
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
    fn a_with_select_is_a_read() {
        for sql in [
            "with cte1 as (select a, b from table1), cte2 as (select c, d from table2) select b, d from cte1 join cte2 where cte1.a = cte2.c",
            "WITH RECURSIVE countdown(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM countdown WHERE n < 3) SELECT n FROM countdown",
            "with x as (select 1) values (2)",
        ] {
            assert_eq!(first_write(sql), None, "{sql} should read");
        }
        // But a CTE carrying a write still counts as one.
        assert_eq!(
            first_write("with moved as (update items set x = 1 returning *) select * from moved")
                .as_deref(),
            Some("WITH")
        );
        assert_eq!(
            first_write("with n as (select 1) insert into items select * from n").as_deref(),
            Some("WITH")
        );
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
    fn transaction_control_is_not_a_write() {
        for sql in [
            "begin",
            "BEGIN TRANSACTION",
            "begin immediate",
            "begin isolation level serializable",
            "start transaction",
            "START TRANSACTION READ ONLY",
            "start transaction with consistent snapshot",
            "commit",
            "commit work",
            "end",
            "rollback",
            "abort",
            "rollback to savepoint a",
            "savepoint a",
            "release savepoint a",
            "release a",
            "set transaction isolation level repeatable read",
        ] {
            assert_eq!(first_write(sql), None, "{sql} should not write");
        }
    }

    #[test]
    fn transaction_control_that_could_write_still_counts() {
        // `READ WRITE` lifts a read-only session's guard for the transaction.
        assert_eq!(first_write("begin read write").as_deref(), Some("BEGIN"));
        assert!(first_write("start transaction read write").is_some());
        assert!(first_write("set transaction read write").is_some());
        // Two-phase commit acts on writes this statement did not make.
        assert!(first_write("commit prepared 'x'").is_some());
        assert!(first_write("set session characteristics as transaction read only").is_some());
        assert!(first_write("set search_path = app").is_some());
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

    /// `SELECT` starts both, so neither is a plain read: Postgres' `SELECT
    /// ... INTO` creates a table, and MySQL's `SELECT ... INTO OUTFILE`/
    /// `INTO DUMPFILE` writes a file — both change something despite the
    /// reading keyword out front, which matters most for `ConfirmWrites`
    /// (no server-side read-only session backs it up the way `ReadOnly` has).
    #[test]
    fn a_select_into_is_not_a_plain_read() {
        assert_eq!(
            first_write("select * into new_table from items").as_deref(),
            Some("SELECT")
        );
        assert_eq!(
            first_write("select * from items into outfile '/tmp/dump.csv'").as_deref(),
            Some("SELECT")
        );
        assert!(first_write("SELECT id INTO DUMPFILE '/tmp/id' FROM items LIMIT 1").is_some());
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
        assert!(first_write("lock tables items write").is_some());
    }

    fn placeholder_names(sql: &str, engine: Engine) -> Vec<String> {
        super::placeholders(sql, engine)
            .into_iter()
            .map(|placeholder| placeholder.name)
            .collect()
    }

    #[test]
    fn placeholders_are_found_outside_strings_and_comments() {
        let sql = "select ':a', x from t -- :b\nwhere id = :id and name = :name";
        assert_eq!(placeholder_names(sql, Engine::Postgres), ["id", "name"]);
        assert_eq!(
            placeholder_names("select /* :c */ :d", Engine::Postgres),
            ["d"]
        );
    }

    #[test]
    fn casts_assignments_and_times_are_not_placeholders() {
        assert!(placeholder_names("select x::int from t", Engine::Postgres).is_empty());
        assert!(placeholder_names("select a:=1 from t", Engine::MySql).is_empty());
        assert!(placeholder_names("select 12:30", Engine::Postgres).is_empty());
        assert!(placeholder_names("select $1 from t", Engine::Postgres).is_empty());
    }

    #[test]
    fn a_placeholder_carries_its_byte_range() {
        let sql = "select * from t where id = :id";
        let placeholders = super::placeholders(sql, Engine::Postgres);
        assert_eq!(placeholders.len(), 1);
        let placeholder = &placeholders[0];
        assert_eq!(placeholder.name, "id");
        assert_eq!(&sql[placeholder.start..placeholder.end], ":id");
    }

    #[test]
    fn dollar_quoted_strings_hide_placeholders() {
        let sql = "select $$:not_a_var$$, :real from t";
        assert_eq!(placeholder_names(sql, Engine::Postgres), ["real"]);
    }
}
