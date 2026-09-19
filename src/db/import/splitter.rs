//! Splitting a dump into statements while it streams.
//!
//! [`statement::split`](crate::db::statement) is for the editor's buffer: it
//! takes the whole text at once and is content with the common cases. A dump is
//! a different problem — it is too large to hold, and it carries dialect
//! furniture the editor never sees: `COPY ... FROM stdin` data, MySQL's
//! `DELIMITER` command, SQLite triggers whose bodies hold semicolons, psql's
//! backslash meta-lines. This splitter reads one line at a time and yields one
//! statement at a time, never holding more than one in memory (plus a COPY
//! batch), so an unterminated quote on line one does not swallow the file.
//!
//! It is deliberately pessimistic in the same way the editor's splitter is: a
//! statement it cannot parse is a statement whose line is named in an error,
//! not one that is quietly glued to the next.

use std::collections::VecDeque;
use std::io::BufRead;

use anyhow::{Result, bail};

/// Which engine's syntax the dump is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Postgres,
    MySql,
    Sqlite,
}

/// The largest statement that will be assembled before the dump is judged to
/// be malformed — an unterminated quote would otherwise grow without bound.
const MAX_STATEMENT: usize = 64 * 1024 * 1024;

/// How much `COPY ... FROM stdin` data is handed over at a time.
const COPY_BATCH: usize = 64 * 1024;

/// One statement, and the line it started on.
#[derive(Debug, Clone)]
pub struct StatementChunk {
    pub sql: String,
    pub line: u64,
}

/// What the splitter hands back as it reads.
#[derive(Debug, Clone)]
pub enum Chunk {
    /// An ordinary statement.
    Statement(StatementChunk),
    /// A `COPY ... FROM stdin` header; the [`Chunk::CopyData`] that follow
    /// belong to it.
    Copy(StatementChunk),
    /// A block of COPY data, already newline-terminated.
    CopyData(Vec<u8>),
}

/// Split `reader`'s statements according to `dialect`.
pub fn new<R: BufRead>(reader: R, dialect: Dialect) -> Splitter<R> {
    Splitter::new(reader, dialect)
}

/// Whether `sql` is a `COPY ... FROM stdin` statement, so its data follows.
pub fn is_copy(sql: &str) -> bool {
    let upper = sql.to_ascii_uppercase();
    let mut tokens = upper
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|token| !token.is_empty());

    if tokens.next() != Some("COPY") {
        return false;
    }

    let mut after_from = false;
    for token in tokens {
        if token == "FROM" {
            after_from = true;
        } else if token == "STDIN" && after_from {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone)]
enum Scan {
    Normal,
    Single,
    Double,
    Backtick,
    LineComment,
    BlockComment(u32),
    Dollar(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    Trigger,
    Case,
}

pub struct Splitter<R> {
    reader: R,
    dialect: Dialect,
    /// The line being read, counting from one.
    line: u64,
    /// The statement being assembled, verbatim including comments.
    statement: String,
    /// The line the statement started on, for an error or a log entry.
    statement_line: u64,
    state: Scan,
    /// MySQL's `DELIMITER`, which is `;` until the dump says otherwise.
    delimiter: String,
    delimiter_chars: Vec<char>,
    /// The identifier being read, so block keywords can be recognised.
    word: String,
    /// Whether this statement mentions `TRIGGER`, so `BEGIN`/`END` in a SQLite
    /// or MySQL body are block structure rather than transaction control.
    saw_trigger: bool,
    /// SQLite trigger `BEGIN ... END` and the `CASE ... END` inside them.
    blocks: Vec<Block>,
    /// Whether a backslash escapes inside the current quoted string.
    escape: bool,
    /// Whether the next data belongs to a `COPY ... FROM stdin`.
    copying: bool,
    copy_buffer: Vec<u8>,
    ready: VecDeque<Chunk>,
    done: bool,
}

impl<R: BufRead> Splitter<R> {
    fn new(reader: R, dialect: Dialect) -> Self {
        Self {
            reader,
            dialect,
            line: 0,
            statement: String::new(),
            statement_line: 0,
            state: Scan::Normal,
            delimiter: ";".to_string(),
            delimiter_chars: vec![';'],
            word: String::new(),
            saw_trigger: false,
            blocks: Vec::new(),
            escape: false,
            copying: false,
            copy_buffer: Vec::new(),
            ready: VecDeque::new(),
            done: false,
        }
    }

    /// Read one line and fold it into the state machine.
    fn advance(&mut self) -> Result<()> {
        let mut raw = Vec::new();
        let read = self.reader.read_until(b'\n', &mut raw)?;
        if read == 0 {
            self.finish()?;
            return Ok(());
        }
        self.line += 1;

        if self.copying {
            self.push_copy(&raw);
            return Ok(());
        }

        let line = String::from_utf8_lossy(&raw).into_owned();

        // A `DELIMITER` command only counts where a statement would start.
        if matches!(self.state, Scan::Normal)
            && self.statement.trim().is_empty()
            && self.set_delimiter(&line)
        {
            return Ok(());
        }

        // psql's meta-commands (`\connect`, `\restrict`, …) are not SQL.
        let trimmed = line.trim_start();
        if matches!(self.state, Scan::Normal)
            && self.statement.trim().is_empty()
            && trimmed.starts_with('\\')
            && !trimmed.starts_with("\\.")
        {
            return Ok(());
        }

        if self.statement.trim().is_empty() {
            self.statement_line = self.line;
        }
        self.scan(&line)
    }

    /// Recognise and consume a MySQL `DELIMITER` line.
    fn set_delimiter(&mut self, line: &str) -> bool {
        let trimmed = line.trim_start();
        let Some(keyword) = trimmed.get(..9) else {
            return false;
        };
        if !keyword.eq_ignore_ascii_case("DELIMITER") {
            return false;
        }

        let rest = &trimmed[keyword.len()..];
        if !rest.starts_with(char::is_whitespace) {
            return false;
        }
        let token = rest.trim();
        if token.is_empty() {
            return false;
        }

        self.delimiter = token.to_string();
        self.delimiter_chars = self.delimiter.chars().collect();
        true
    }

    /// Append COPY data, ending the block on the `\.` line.
    fn push_copy(&mut self, raw: &[u8]) {
        let text = raw.strip_suffix(b"\n").unwrap_or(raw);
        let text = text.strip_suffix(b"\r").unwrap_or(text);
        if text == b"\\." {
            if !self.copy_buffer.is_empty() {
                self.ready
                    .push_back(Chunk::CopyData(std::mem::take(&mut self.copy_buffer)));
            }
            self.copying = false;
            return;
        }

        self.copy_buffer.extend_from_slice(raw);
        if self.copy_buffer.len() >= COPY_BATCH {
            self.ready
                .push_back(Chunk::CopyData(std::mem::take(&mut self.copy_buffer)));
        }
    }

    fn scan(&mut self, line: &str) -> Result<()> {
        let characters: Vec<char> = line.chars().collect();
        let mut index = 0;

        while index < characters.len() {
            if self.statement.len() > MAX_STATEMENT {
                bail!(
                    "the statement starting on line {} is longer than the {} MiB limit",
                    self.statement_line,
                    MAX_STATEMENT / (1024 * 1024)
                );
            }

            let state = self.state.clone();
            index = match state {
                Scan::Normal => self.scan_normal(&characters, index)?,
                Scan::Single => self.scan_single(&characters, index),
                Scan::Double => self.scan_quoted(&characters, index, '"'),
                Scan::Backtick => self.scan_quoted(&characters, index, '`'),
                Scan::LineComment => self.scan_line_comment(&characters, index),
                Scan::BlockComment(depth) => self.scan_block_comment(&characters, index, depth),
                Scan::Dollar(tag) => self.scan_dollar(&characters, index, &tag),
            };
        }
        Ok(())
    }

    fn scan_normal(&mut self, characters: &[char], index: usize) -> Result<usize> {
        // The delimiter ends the statement unless a trigger body is open.
        if !self.delimiter_chars.is_empty()
            && characters[index..].starts_with(&self.delimiter_chars)
        {
            self.end_word();
            if self.blocks.is_empty() {
                self.end_statement();
            } else {
                for character in self.delimiter_chars.clone() {
                    self.statement.push(character);
                }
            }
            return Ok(index + self.delimiter_chars.len());
        }

        let character = characters[index];
        let next = characters.get(index + 1).copied();

        match character {
            '\'' => {
                self.end_word();
                self.escape = self.escapes_in_a_string();
                self.statement.push(character);
                self.state = Scan::Single;
                Ok(index + 1)
            }
            '"' | '`' => {
                self.end_word();
                // MySQL reads a backslash in either kind of quoted string as an
                // escape; Postgres and SQLite only in `E'...'`.
                self.escape = self.dialect == Dialect::MySql;
                self.statement.push(character);
                self.state = if character == '"' {
                    Scan::Double
                } else {
                    Scan::Backtick
                };
                Ok(index + 1)
            }
            '-' if next == Some('-') => {
                self.end_word();
                self.statement.push('-');
                self.statement.push('-');
                self.state = Scan::LineComment;
                Ok(index + 2)
            }
            '#' if self.dialect == Dialect::MySql => {
                self.end_word();
                self.statement.push('#');
                self.state = Scan::LineComment;
                Ok(index + 1)
            }
            '/' if next == Some('*') => {
                self.end_word();
                self.statement.push('/');
                self.statement.push('*');
                self.state = Scan::BlockComment(1);
                Ok(index + 2)
            }
            '$' if self.dialect == Dialect::Postgres => match dollar_tag(characters, index) {
                Some(tag) => {
                    self.end_word();
                    self.statement.push_str(&tag);
                    let length = tag.chars().count();
                    self.state = Scan::Dollar(tag);
                    Ok(index + length)
                }
                None => {
                    self.statement.push('$');
                    Ok(index + 1)
                }
            },
            character if is_word(character) => {
                self.word.push(character);
                self.statement.push(character);
                Ok(index + 1)
            }
            _ => {
                self.end_word();
                self.statement.push(character);
                Ok(index + 1)
            }
        }
    }

    fn scan_single(&mut self, characters: &[char], index: usize) -> usize {
        let character = characters[index];
        if character == '\\' && self.escape {
            self.statement.push(character);
            if let Some(next) = characters.get(index + 1) {
                self.statement.push(*next);
                return index + 2;
            }
            return index + 1;
        }
        if character == '\'' {
            self.statement.push(character);
            if characters.get(index + 1) == Some(&'\'') {
                self.statement.push('\'');
                return index + 2;
            }
            self.state = Scan::Normal;
            return index + 1;
        }
        self.statement.push(character);
        index + 1
    }

    /// A `"..."` or backtick-quoted name; a doubled quote escapes and MySQL
    /// also honours a backslash.
    fn scan_quoted(&mut self, characters: &[char], index: usize, quote: char) -> usize {
        let character = characters[index];
        if character == '\\' && self.escape {
            self.statement.push(character);
            if let Some(next) = characters.get(index + 1) {
                self.statement.push(*next);
                return index + 2;
            }
            return index + 1;
        }
        if character == quote {
            self.statement.push(character);
            if characters.get(index + 1) == Some(&quote) {
                self.statement.push(quote);
                return index + 2;
            }
            self.state = Scan::Normal;
            return index + 1;
        }
        self.statement.push(character);
        index + 1
    }

    fn scan_line_comment(&mut self, characters: &[char], index: usize) -> usize {
        let character = characters[index];
        self.statement.push(character);
        if character == '\n' {
            self.state = Scan::Normal;
        }
        index + 1
    }

    fn scan_block_comment(&mut self, characters: &[char], index: usize, depth: u32) -> usize {
        let character = characters[index];
        let next = characters.get(index + 1).copied();

        if character == '*' && next == Some('/') {
            self.statement.push('*');
            self.statement.push('/');
            self.state = if depth <= 1 {
                Scan::Normal
            } else {
                Scan::BlockComment(depth - 1)
            };
            return index + 2;
        }
        if character == '/' && next == Some('*') && self.dialect == Dialect::Postgres {
            self.statement.push('/');
            self.statement.push('*');
            self.state = Scan::BlockComment(depth + 1);
            return index + 2;
        }
        self.statement.push(character);
        index + 1
    }

    fn scan_dollar(&mut self, characters: &[char], index: usize, tag: &str) -> usize {
        let tag: Vec<char> = tag.chars().collect();
        if characters[index..].starts_with(&tag) {
            for character in &tag {
                self.statement.push(*character);
            }
            self.state = Scan::Normal;
            return index + tag.len();
        }
        self.statement.push(characters[index]);
        index + 1
    }

    /// Finish the identifier being read, and let it move the block depth.
    fn end_word(&mut self) {
        if self.word.is_empty() {
            return;
        }
        let word = std::mem::take(&mut self.word).to_ascii_uppercase();

        if word == "TRIGGER" {
            self.saw_trigger = true;
        }
        if self.saw_trigger {
            match word.as_str() {
                "BEGIN" => self.blocks.push(Block::Trigger),
                "CASE" => self.blocks.push(Block::Case),
                "END" => {
                    self.blocks.pop();
                }
                _ => {}
            }
        }
    }

    /// Emit the statement just completed, if it holds anything.
    fn end_statement(&mut self) {
        self.end_word();
        let text = self.statement.trim().to_string();
        let line = self.statement_line;
        self.statement.clear();
        self.word.clear();
        self.saw_trigger = false;
        self.blocks.clear();
        self.escape = false;

        if text.is_empty() {
            return;
        }
        if is_copy(&text) {
            self.ready
                .push_back(Chunk::Copy(StatementChunk { sql: text, line }));
            self.copying = true;
        } else {
            self.ready
                .push_back(Chunk::Statement(StatementChunk { sql: text, line }));
        }
    }

    /// The end of the file: one last statement, or an error saying where the
    /// dump gave up.
    fn finish(&mut self) -> Result<()> {
        self.done = true;

        if self.copying {
            bail!("the dump ends inside COPY data (line {})", self.line);
        }
        match self.state {
            Scan::Single | Scan::Double | Scan::Backtick | Scan::Dollar(_) => {
                bail!("the dump ends inside a quoted string (line {})", self.line)
            }
            Scan::BlockComment(_) => {
                bail!("the dump ends inside a comment (line {})", self.line)
            }
            // A line comment runs out with the line, so there is nothing left
            // open when the file ends.
            Scan::Normal | Scan::LineComment => {}
        }

        self.end_statement();
        Ok(())
    }

    /// Whether a backslash escapes inside a string opened here.
    fn escapes_in_a_string(&self) -> bool {
        match self.dialect {
            Dialect::MySql => true,
            Dialect::Sqlite => false,
            Dialect::Postgres => {
                let trimmed = self.statement.trim_end();
                trimmed.ends_with('E') || trimmed.ends_with('e')
            }
        }
    }
}

impl<R: BufRead> Iterator for Splitter<R> {
    type Item = Result<Chunk>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(chunk) = self.ready.pop_front() {
                return Some(Ok(chunk));
            }
            if self.done {
                return None;
            }
            if let Err(error) = self.advance() {
                self.done = true;
                return Some(Err(error));
            }
        }
    }
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
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
            character if character.is_ascii_alphanumeric() || *character == '_' => {
                tag.push(*character)
            }
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn statements(sql: &str, dialect: Dialect) -> Vec<String> {
        new(Cursor::new(sql.as_bytes().to_vec()), dialect)
            .map(|chunk| match chunk.expect("the splitter should not fail") {
                Chunk::Statement(statement) | Chunk::Copy(statement) => statement.sql,
                Chunk::CopyData(_) => "<data>".to_string(),
            })
            .collect()
    }

    #[test]
    fn plain_statements_split_on_semicolons() {
        let sql = "CREATE TABLE t (id int);\nINSERT INTO t VALUES (1);\n";
        assert_eq!(
            statements(sql, Dialect::Sqlite),
            ["CREATE TABLE t (id int)", "INSERT INTO t VALUES (1)"]
        );
    }

    #[test]
    fn a_semicolon_in_a_string_is_not_a_split() {
        let sql = "INSERT INTO t VALUES ('a;b');\nSELECT 1;\n";
        assert_eq!(
            statements(sql, Dialect::Sqlite),
            ["INSERT INTO t VALUES ('a;b')", "SELECT 1"]
        );
    }

    #[test]
    fn a_doubled_quote_is_an_escaped_quote() {
        let sql = "INSERT INTO t VALUES ('it''s; fine');\nSELECT 1;\n";
        assert_eq!(
            statements(sql, Dialect::Postgres),
            ["INSERT INTO t VALUES ('it''s; fine')", "SELECT 1"]
        );
    }

    #[test]
    fn a_comment_keeps_its_semicolon() {
        let sql = "/* a ; b */ SELECT 1;\n-- another ; one\nSELECT 2;\n";
        assert_eq!(
            statements(sql, Dialect::Postgres),
            ["/* a ; b */ SELECT 1", "-- another ; one\nSELECT 2"]
        );
    }

    #[test]
    fn a_dollar_quoted_body_is_one_statement() {
        let sql = "CREATE FUNCTION f() RETURNS int AS $body$\nBEGIN\n  RETURN 1;\nEND;\n$body$ LANGUAGE plpgsql;\nSELECT f();\n";
        assert_eq!(
            statements(sql, Dialect::Postgres),
            [
                "CREATE FUNCTION f() RETURNS int AS $body$\nBEGIN\n  RETURN 1;\nEND;\n$body$ LANGUAGE plpgsql",
                "SELECT f()"
            ]
        );
    }

    #[test]
    fn a_backslash_escapes_in_a_mysql_string() {
        let sql = "INSERT INTO t VALUES ('a\\'; b');\nSELECT 1;\n";
        assert_eq!(
            statements(sql, Dialect::MySql),
            ["INSERT INTO t VALUES ('a\\'; b')", "SELECT 1"]
        );
    }

    #[test]
    fn a_mysql_delimiter_is_honoured_and_dropped() {
        let sql = "DELIMITER ;;\nCREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW BEGIN SET @a = 1; END;;\nDELIMITER ;\nSELECT 1;\n";
        assert_eq!(
            statements(sql, Dialect::MySql),
            [
                "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW BEGIN SET @a = 1; END",
                "SELECT 1"
            ]
        );
    }

    #[test]
    fn a_sqlite_trigger_body_keeps_its_semicolons() {
        let sql = "CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n  UPDATE t SET n = n + 1;\n  INSERT INTO log VALUES (1);\nEND;\nSELECT 1;\n";
        assert_eq!(
            statements(sql, Dialect::Sqlite),
            [
                "CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n  UPDATE t SET n = n + 1;\n  INSERT INTO log VALUES (1);\nEND",
                "SELECT 1"
            ]
        );
    }

    #[test]
    fn a_case_inside_a_trigger_does_not_end_it_early() {
        let sql = "CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n  UPDATE t SET x = CASE WHEN n > 0 THEN 1 ELSE 0 END;\n  DELETE FROM t WHERE n < 0;\nEND;\n";
        assert_eq!(
            statements(sql, Dialect::Sqlite),
            [
                "CREATE TRIGGER tr AFTER INSERT ON t BEGIN\n  UPDATE t SET x = CASE WHEN n > 0 THEN 1 ELSE 0 END;\n  DELETE FROM t WHERE n < 0;\nEND"
            ]
        );
    }

    #[test]
    fn a_psql_meta_line_is_skipped() {
        let sql = "\\restrict abc\nSELECT 1;\n\\connect db\nSELECT 2;\n";
        assert_eq!(statements(sql, Dialect::Postgres), ["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn copy_data_is_batched_and_the_header_is_its_own_chunk() {
        let sql = "COPY t (id, name) FROM stdin;\n1\tone\n2\ttwo\n\\.\nSELECT 1;\n";
        let chunks: Vec<Chunk> = new(Cursor::new(sql.as_bytes().to_vec()), Dialect::Postgres)
            .map(|chunk| chunk.expect("the splitter should not fail"))
            .collect();

        assert!(matches!(&chunks[0], Chunk::Copy(statement) if statement.sql.starts_with("COPY")));
        match &chunks[1] {
            Chunk::CopyData(data) => {
                assert_eq!(data, b"1\tone\n2\ttwo\n");
            }
            other => panic!("expected COPY data, got {other:?}"),
        }
        assert!(matches!(&chunks[2], Chunk::Statement(statement) if statement.sql == "SELECT 1"));
    }

    #[test]
    fn an_unterminated_quote_names_the_line() {
        let sql = "SELECT 1;\nSELECT 'oops;\n";
        let error = new(Cursor::new(sql.as_bytes().to_vec()), Dialect::Postgres)
            .find_map(|chunk| chunk.err())
            .expect("an unterminated quote should fail");
        assert!(format!("{error}").contains("quoted string"), "{error}");
    }

    #[test]
    fn is_copy_tells_a_stdin_copy_from_a_file_copy() {
        assert!(is_copy("COPY t (a, b) FROM stdin"));
        assert!(is_copy("copy t from STDIN with (format csv)"));
        assert!(!is_copy("COPY t FROM '/tmp/data.csv'"));
        assert!(!is_copy("COPY t TO STDOUT"));
        assert!(!is_copy("SELECT 1"));
    }
}
