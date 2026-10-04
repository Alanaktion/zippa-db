//! What the SQL editor offers to complete the word under the caret.
//!
//! Suggestions come from the session's [`Catalog`] snapshot — tables, views,
//! columns, and routines — plus a list of SQL keywords, so a keystroke never
//! costs a round trip. Nothing here touches a window or a database: the editor
//! hands over the buffer and the caret and gets back the range to replace and
//! what to offer, which is what lets the ranking be unit-tested on its own.
//!
//! The reading of the statement is deliberately shallow. It tokenizes the
//! statement the caret is in, finds the tables it names (`FROM`, `JOIN`,
//! `UPDATE`, `INTO`) and their aliases, and looks at the word before the caret
//! to tell "a table goes here" from "a column goes here". It is not a parser,
//! and where it cannot tell it offers columns, keywords, and tables together
//! rather than guessing one.

use std::collections::HashSet;
use std::ops::Range;

use super::catalog::{Catalog, CatalogEntry, CatalogKind};
use super::config::Engine;

/// The most suggestions one request hands back. The menu is a short list to
/// pick from, not a browser; typing another letter narrows it.
pub const MAX_SUGGESTIONS: usize = 60;

/// What a suggestion is, for the menu's kind icon and its ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestionKind {
    Keyword,
    Table,
    View,
    Column,
    Routine,
    Schema,
}

/// One thing the menu offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// What the menu shows: the bare name or keyword.
    pub label: String,
    /// What replaces the word under the caret: the label, quoted where the
    /// engine would not read it back bare, and a keyword in the case the user
    /// is typing in.
    pub insert: String,
    pub kind: SuggestionKind,
    /// The muted second column: a column's type and table, a routine's
    /// arguments, or the kind word.
    pub detail: String,
}

/// The answer to one request: the byte range of the buffer the chosen
/// suggestion replaces, and the suggestions, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completions {
    pub replace: Range<usize>,
    /// What the user has typed of the word, for the menu to highlight.
    pub prefix: String,
    pub items: Vec<Suggestion>,
}

/// Keywords offered everywhere. Kept to the words people type in queries and
/// everyday DDL; anything rarer is quicker typed than scrolled to.
const KEYWORDS: &[&str] = &[
    "ADD",
    "ALL",
    "ALTER",
    "AND",
    "ANY",
    "AS",
    "ASC",
    "BEGIN",
    "BETWEEN",
    "BY",
    "CASCADE",
    "CASE",
    "CAST",
    "CHECK",
    "COALESCE",
    "COLUMN",
    "COMMIT",
    "CONSTRAINT",
    "COUNT",
    "CREATE",
    "CROSS",
    "DATABASE",
    "DEFAULT",
    "DELETE",
    "DESC",
    "DISTINCT",
    "DROP",
    "ELSE",
    "END",
    "EXCEPT",
    "EXISTS",
    "EXPLAIN",
    "FALSE",
    "FOREIGN",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "IN",
    "INDEX",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "IS",
    "JOIN",
    "KEY",
    "LEFT",
    "LIKE",
    "LIMIT",
    "NOT",
    "NULL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "PRIMARY",
    "REFERENCES",
    "RIGHT",
    "ROLLBACK",
    "SELECT",
    "SET",
    "TABLE",
    "THEN",
    "TRUE",
    "TRUNCATE",
    "UNION",
    "UNIQUE",
    "UPDATE",
    "USING",
    "VALUES",
    "VIEW",
    "WHEN",
    "WHERE",
    "WITH",
];

/// Keywords only one engine knows, offered alongside [`KEYWORDS`] there.
fn engine_keywords(engine: Engine) -> &'static [&'static str] {
    match engine {
        Engine::Postgres => &[
            "ILIKE",
            "LATERAL",
            "RETURNING",
            "SCHEMA",
            "SIMILAR",
            "VACUUM",
            "WINDOW",
        ],
        Engine::MySql => &[
            "AUTO_INCREMENT",
            "DATABASES",
            "DESCRIBE",
            "REGEXP",
            "SHOW",
            "TABLES",
            "USE",
        ],
        Engine::Sqlite => &[
            "ATTACH",
            "AUTOINCREMENT",
            "GLOB",
            "PRAGMA",
            "RETURNING",
            "VACUUM",
        ],
    }
}

/// Words after which the next word names a table rather than a column.
const TABLE_CLAUSES: &[&str] = &[
    "FROM", "JOIN", "INTO", "UPDATE", "TABLE", "TRUNCATE", "DESCRIBE",
];

/// Words that end a run of table references, or start a clause of their own.
/// The word after a table name is its alias unless it is one of these.
const CLAUSE_WORDS: &[&str] = &[
    "AND",
    "AS",
    "BY",
    "CROSS",
    "DELETE",
    "EXCEPT",
    "FROM",
    "FULL",
    "GROUP",
    "HAVING",
    "INNER",
    "INSERT",
    "INTERSECT",
    "INTO",
    "JOIN",
    "LATERAL",
    "LEFT",
    "LIMIT",
    "NATURAL",
    "OFFSET",
    "ON",
    "OR",
    "ORDER",
    "OUTER",
    "RETURNING",
    "RIGHT",
    "SELECT",
    "SET",
    "STRAIGHT_JOIN",
    "TABLE",
    "UNION",
    "UPDATE",
    "USING",
    "VALUES",
    "WHERE",
    "WINDOW",
    "WITH",
];

/// Names a bare identifier cannot take without quoting on at least one engine
/// — `order` and `user` are the ones that turn up as real column names.
const RESERVED: &[&str] = &[
    "ALL",
    "AND",
    "AS",
    "ASC",
    "BY",
    "CASE",
    "CHECK",
    "COLUMN",
    "CONSTRAINT",
    "CREATE",
    "DEFAULT",
    "DESC",
    "DISTINCT",
    "ELSE",
    "END",
    "FROM",
    "GROUP",
    "HAVING",
    "IN",
    "INDEX",
    "IS",
    "JOIN",
    "KEY",
    "LIMIT",
    "NOT",
    "NULL",
    "ON",
    "OR",
    "ORDER",
    "PRIMARY",
    "REFERENCES",
    "SELECT",
    "TABLE",
    "THEN",
    "TO",
    "UNION",
    "UNIQUE",
    "USER",
    "USING",
    "WHEN",
    "WHERE",
    "WITH",
];

/// What to offer for the word the caret at byte `cursor` ends.
///
/// `None` when there is nothing to complete: the caret is in a string or a
/// comment, or it follows neither a word being typed nor a `.`.
pub fn complete(
    sql: &str,
    cursor: usize,
    catalog: &Catalog,
    engine: Engine,
) -> Option<Completions> {
    let cursor = floor_char_boundary(sql, cursor.min(sql.len()));
    let start = statement_start(sql, cursor);
    let before = lex(&sql[start..cursor], start)?;

    // The word being typed, if the caret ends one.
    let (prefix, replace, rest) = match before.last() {
        Some(Token::Word {
            text,
            quoted: false,
            range,
        }) if range.end == cursor => (text.clone(), range.clone(), &before[..before.len() - 1]),
        _ => (String::new(), cursor..cursor, &before[..]),
    };

    // `a.b.` and `a.` qualify the word: a schema, a table, or an alias.
    let qualifier = qualifier(rest, replace.start);
    if prefix.is_empty() && qualifier.is_none() {
        return None;
    }

    // The whole statement, for the tables it names — including those after
    // the caret, since `SELECT | FROM users` is the usual way to write one.
    let end = statement_end(sql, cursor);
    let statement = lex(&sql[start..end], start).unwrap_or_default();
    let references = references(&statement, &replace);

    let in_table_clause = qualifier.is_none() && expects_table(rest);
    let lead = if in_table_clause {
        SuggestionKind::Table
    } else {
        SuggestionKind::Column
    };
    let mut out = Collector::new(&prefix, engine, lead);

    match qualifier {
        Some(path) => qualified(&mut out, &path, &references, catalog),
        None if in_table_clause => {
            objects(&mut out, catalog, None);
            schemas(&mut out, catalog);
            keywords(&mut out);
        }
        None => {
            for reference in &references {
                if let Some(object) = resolve(catalog, reference) {
                    columns_of(&mut out, catalog, &object, references.len() > 1);
                }
            }
            keywords(&mut out);
            objects(&mut out, catalog, None);
            routines(&mut out, catalog);
            // Before the `FROM` is written, any column the database has.
            if references.is_empty() {
                all_columns(&mut out, catalog);
            }
        }
    }

    let items = out.finish();
    (!items.is_empty()).then_some(Completions {
        replace,
        prefix,
        items,
    })
}

/// One lexical piece of a statement. Only what the reading above needs is
/// told apart; everything else is `Other`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    /// An identifier or keyword. `text` is unquoted; `quoted` says whether it
    /// was written in quotes, which makes it a name rather than a keyword.
    Word {
        text: String,
        quoted: bool,
        range: Range<usize>,
    },
    Dot(usize),
    Comma,
    Open,
    Close,
    Other,
}

impl Token {
    /// The word upper-cased, when this is an unquoted word — what a keyword
    /// test compares against.
    fn keyword(&self) -> Option<String> {
        match self {
            Token::Word {
                text,
                quoted: false,
                ..
            } => Some(text.to_ascii_uppercase()),
            _ => None,
        }
    }

    fn name(&self) -> Option<&str> {
        match self {
            Token::Word { text, .. } => Some(text),
            _ => None,
        }
    }
}

/// Split `text` (which starts at byte `base` of the buffer) into tokens.
///
/// `None` when `text` ends inside a string literal or a comment, which is
/// where a completion menu would only get in the way. A quoted identifier
/// left open counts the same way.
fn lex(text: &str, base: usize) -> Option<Vec<Token>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        match byte {
            b'-' if next == Some(b'-') => index = line_end(bytes, index)?,
            b'#' => index = line_end(bytes, index)?,
            b'/' if next == Some(b'*') => {
                index = text[index + 2..].find("*/").map(|at| index + 2 + at + 2)?;
            }
            b'\'' => index = closing(bytes, index, b'\'')?,
            b'$' if dollar_tag(text, index).is_some() => {
                let tag = dollar_tag(text, index).expect("checked just above");
                let body = index + tag.len();
                index = text[body..].find(&tag).map(|at| body + at + tag.len())?;
            }
            b'"' | b'`' => {
                let end = closing(bytes, index, byte)?;
                let quote = char::from(byte);
                tokens.push(Token::Word {
                    text: text[index + 1..end - 1]
                        .replace(&format!("{quote}{quote}"), &quote.to_string()),
                    quoted: true,
                    range: base + index..base + end,
                });
                index = end;
            }
            b'.' => {
                tokens.push(Token::Dot(base + index));
                index += 1;
            }
            b',' => {
                tokens.push(Token::Comma);
                index += 1;
            }
            b'(' => {
                tokens.push(Token::Open);
                index += 1;
            }
            b')' => {
                tokens.push(Token::Close);
                index += 1;
            }
            _ if is_word_start(text, index) => {
                let end = word_end(text, index);
                tokens.push(Token::Word {
                    text: text[index..end].to_string(),
                    quoted: false,
                    range: base + index..base + end,
                });
                index = end;
            }
            _ if byte.is_ascii_whitespace() => index += 1,
            _ => {
                tokens.push(Token::Other);
                index += text[index..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    Some(tokens)
}

/// Where a line comment starting at `index` ends, or `None` when it runs to
/// the end of `bytes` — the caret is in it.
fn line_end(bytes: &[u8], index: usize) -> Option<usize> {
    bytes[index..]
        .iter()
        .position(|&byte| byte == b'\n')
        .map(|at| index + at + 1)
}

/// The index just past the quote closing the one at `index`, with a doubled
/// quote read as one character of the text. `None` when it is never closed.
fn closing(bytes: &[u8], index: usize, close: u8) -> Option<usize> {
    let mut at = index + 1;
    while at < bytes.len() {
        if bytes[at] == b'\\' && close == b'\'' {
            at += 2;
            continue;
        }
        if bytes[at] == close {
            if bytes.get(at + 1) == Some(&close) {
                at += 2;
                continue;
            }
            return Some(at + 1);
        }
        at += 1;
    }
    None
}

/// Postgres' `$tag$` opening a dollar-quoted string at `index`, if there is
/// one there.
fn dollar_tag(text: &str, index: usize) -> Option<String> {
    let rest = &text[index + 1..];
    let end = rest.find('$')?;
    let tag = &rest[..end];
    (tag.is_empty()
        || (!tag.starts_with(|c: char| c.is_ascii_digit())
            && tag.chars().all(|c| c.is_alphanumeric() || c == '_')))
    .then(|| format!("${tag}$"))
}

fn is_word_char(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

fn is_word_start(text: &str, index: usize) -> bool {
    text[index..]
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

fn word_end(text: &str, index: usize) -> usize {
    text[index..]
        .char_indices()
        .find(|&(_, c)| !is_word_char(c))
        .map_or(text.len(), |(at, _)| index + at)
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The byte where the statement holding `cursor` begins: just past the last
/// `;` before it that is not inside a string or comment.
fn statement_start(sql: &str, cursor: usize) -> usize {
    super::statement::split(&sql[..cursor])
        .last()
        .map_or(cursor, |last| {
            // A `;` between the last statement and the caret ended it, so the
            // caret starts a new one.
            if sql[last.end..cursor].contains(';') {
                cursor
            } else {
                last.start
            }
        })
}

/// The byte where the statement holding `cursor` ends.
fn statement_end(sql: &str, cursor: usize) -> usize {
    super::statement::split(&sql[cursor..])
        .first()
        .map_or(sql.len(), |first| {
            if sql[cursor..cursor + first.start].contains(';') {
                cursor
            } else {
                cursor + first.end
            }
        })
}

/// The dotted path qualifying the word that starts at `at`: `["s", "t"]` for
/// `s.t.|`, `["u"]` for `u.|`. `None` when no `.` sits right before it.
fn qualifier(tokens: &[Token], at: usize) -> Option<Vec<String>> {
    let mut path = Vec::new();
    let mut expect = at;
    let mut rest = tokens;
    while let [head @ .., Token::Word { text, range, .. }, Token::Dot(dot)] = rest {
        if dot + 1 != expect || range.end != *dot {
            break;
        }
        path.insert(0, text.clone());
        expect = range.start;
        rest = head;
    }
    (!path.is_empty()).then_some(path)
}

/// Whether the word being typed names a table: the nearest clause word before
/// it is one of [`TABLE_CLAUSES`], with no parenthesis opened since — that
/// would be `INSERT INTO t (|`, a column list.
fn expects_table(tokens: &[Token]) -> bool {
    let mut depth = 0i32;
    for token in tokens.iter().rev() {
        match token {
            Token::Close => depth += 1,
            Token::Open if depth == 0 => return false,
            Token::Open => depth -= 1,
            _ => {}
        }
        if let Some(word) = token.keyword() {
            if TABLE_CLAUSES.contains(&word.as_str()) {
                return depth == 0;
            }
            if CLAUSE_WORDS.contains(&word.as_str()) {
                return false;
            }
        }
    }
    false
}

/// A table or view the statement names, and what it calls it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reference {
    schema: Option<String>,
    name: String,
    alias: Option<String>,
}

/// The tables and views the statement names after `FROM`, `JOIN`, `UPDATE`,
/// or `INTO`, with their aliases. The word being typed (`typing`) is left out
/// — it is not a table yet.
fn references(tokens: &[Token], typing: &Range<usize>) -> Vec<Reference> {
    let mut found = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let in_list = matches!(
            tokens[index].keyword().as_deref(),
            Some("FROM" | "JOIN" | "UPDATE" | "INTO")
        );
        index += 1;
        if !in_list {
            continue;
        }
        // `FROM a x, b y` names two; a `JOIN` names one at a time.
        while let Some((reference, next)) = reference_at(tokens, index, typing) {
            found.push(reference);
            index = next;
            if matches!(tokens.get(index), Some(Token::Comma)) {
                index += 1;
            } else {
                break;
            }
        }
    }
    found
}

/// One `[schema.]name [[AS] alias]` starting at `index`, and the index after it.
fn reference_at(
    tokens: &[Token],
    mut index: usize,
    typing: &Range<usize>,
) -> Option<(Reference, usize)> {
    let mut path = Vec::new();
    loop {
        let Token::Word { text, range, .. } = tokens.get(index)? else {
            return None;
        };
        if range == typing {
            return None;
        }
        path.push(text.clone());
        index += 1;
        match tokens.get(index) {
            Some(Token::Dot(_)) => index += 1,
            _ => break,
        }
    }
    let name = path.pop()?;
    let schema = path.pop();

    let mut alias = None;
    if tokens.get(index).and_then(Token::keyword).as_deref() == Some("AS") {
        index += 1;
    }
    if let Some(token @ Token::Word { range, .. }) = tokens.get(index)
        && range != typing
        && !token
            .keyword()
            .is_some_and(|word| CLAUSE_WORDS.contains(&word.as_str()))
    {
        alias = token.name().map(str::to_string);
        index += 1;
    }
    Some((
        Reference {
            schema,
            name,
            alias,
        },
        index,
    ))
}

/// The table or view `reference` names in the catalog.
fn resolve(catalog: &Catalog, reference: &Reference) -> Option<CatalogEntry> {
    find_object(catalog, reference.schema.as_deref(), &reference.name)
}

/// A table or view by name, preferring an exact match over one that differs
/// only in case.
fn find_object(catalog: &Catalog, schema: Option<&str>, name: &str) -> Option<CatalogEntry> {
    let candidates = catalog.entries.iter().filter(|entry| {
        matches!(entry.kind, CatalogKind::Table | CatalogKind::View)
            && entry.name.eq_ignore_ascii_case(name)
            && schema.is_none_or(|schema| {
                entry
                    .object
                    .as_ref()
                    .and_then(|object| object.schema.as_deref())
                    .is_some_and(|own| own.eq_ignore_ascii_case(schema))
            })
    });
    let mut fallback = None;
    for entry in candidates {
        if entry.name == name {
            return Some(entry.clone());
        }
        fallback.get_or_insert_with(|| entry.clone());
    }
    fallback
}

/// Suggestions after `path.`: a reference's columns when the path names an
/// alias or a table, and a schema's tables when it names a schema. Both, when
/// a name is both.
fn qualified(out: &mut Collector, path: &[String], references: &[Reference], catalog: &Catalog) {
    if let [name] = path {
        let aliased = references.iter().find(|reference| {
            reference
                .alias
                .as_ref()
                .is_some_and(|alias| alias.eq_ignore_ascii_case(name))
        });
        let named = references
            .iter()
            .find(|reference| reference.name.eq_ignore_ascii_case(name));
        let object = aliased
            .or(named)
            .and_then(|reference| resolve(catalog, reference))
            .or_else(|| find_object(catalog, None, name));
        if let Some(object) = object {
            columns_of(out, catalog, &object, false);
        }
        objects(out, catalog, Some(name));
        routines_in(out, catalog, name);
    } else if let [schema, table] = path
        && let Some(object) = find_object(catalog, Some(schema), table)
    {
        columns_of(out, catalog, &object, false);
    }
}

/// Columns of one table or view, in their own order. `owned` adds the table's
/// name to the detail, for a statement that joins several.
fn columns_of(out: &mut Collector, catalog: &Catalog, object: &CatalogEntry, owned: bool) {
    let Some(owner) = object.object.as_ref() else {
        return;
    };
    for entry in &catalog.entries {
        if entry.kind == CatalogKind::Column && entry.object.as_ref() == Some(owner) {
            let detail = if owned {
                format!("{} · {}", entry.detail, owner.name)
            } else {
                entry.detail.clone()
            };
            out.name(&entry.name, SuggestionKind::Column, detail);
        }
    }
}

/// Every column in the catalog, once per name.
fn all_columns(out: &mut Collector, catalog: &Catalog) {
    for entry in &catalog.entries {
        if entry.kind == CatalogKind::Column {
            let detail = entry
                .object
                .as_ref()
                .map(|owner| format!("{} · {}", entry.detail, owner.name))
                .unwrap_or_else(|| entry.detail.clone());
            out.name(&entry.name, SuggestionKind::Column, detail);
        }
    }
}

/// Tables and views, all of them or those in `schema`.
fn objects(out: &mut Collector, catalog: &Catalog, schema: Option<&str>) {
    for entry in &catalog.entries {
        let kind = match entry.kind {
            CatalogKind::Table => SuggestionKind::Table,
            CatalogKind::View => SuggestionKind::View,
            _ => continue,
        };
        let own_schema = entry.object.as_ref().and_then(|o| o.schema.as_deref());
        if let Some(schema) = schema
            && !own_schema.is_some_and(|own| own.eq_ignore_ascii_case(schema))
        {
            continue;
        }
        let detail = match (schema, own_schema) {
            (None, Some(own)) => format!("{} · {own}", entry.kind.label()),
            _ => entry.kind.label().to_string(),
        };
        out.name(&entry.name, kind, detail);
    }
}

/// Every schema an object lives in.
fn schemas(out: &mut Collector, catalog: &Catalog) {
    for entry in &catalog.entries {
        if let Some(schema) = entry
            .object
            .as_ref()
            .and_then(|object| object.schema.as_deref())
            .or_else(|| entry.routine.as_ref().and_then(|r| r.schema.as_deref()))
        {
            out.name(schema, SuggestionKind::Schema, "Schema".into());
        }
    }
}

fn routines(out: &mut Collector, catalog: &Catalog) {
    for entry in &catalog.entries {
        if entry.kind == CatalogKind::Routine {
            out.name(
                &entry.name,
                SuggestionKind::Routine,
                entry.kind_label().into(),
            );
        }
    }
}

fn routines_in(out: &mut Collector, catalog: &Catalog, schema: &str) {
    for entry in &catalog.entries {
        if entry.kind == CatalogKind::Routine
            && entry
                .routine
                .as_ref()
                .and_then(|routine| routine.schema.as_deref())
                .is_some_and(|own| own.eq_ignore_ascii_case(schema))
        {
            out.name(
                &entry.name,
                SuggestionKind::Routine,
                entry.kind_label().into(),
            );
        }
    }
}

fn keywords(out: &mut Collector) {
    let engine = out.engine;
    for keyword in KEYWORDS.iter().chain(engine_keywords(engine)) {
        out.keyword(keyword);
    }
}

/// Gathers suggestions in the order they are offered, keeping only those that
/// start with what was typed and dropping repeats.
struct Collector {
    prefix: String,
    engine: Engine,
    /// Whether keywords go in upper case: they follow the case the user is
    /// typing in, and upper case when there is nothing typed to follow.
    upper: bool,
    /// The group listed ahead of the keywords — what the clause expects.
    lead: usize,
    seen: HashSet<(String, usize)>,
    groups: Vec<Vec<Suggestion>>,
}

impl Collector {
    fn new(prefix: &str, engine: Engine, lead: SuggestionKind) -> Self {
        Self {
            prefix: prefix.to_lowercase(),
            engine,
            upper: prefix.is_empty() || prefix.chars().any(char::is_uppercase),
            lead: group_of(lead),
            seen: HashSet::new(),
            groups: vec![Vec::new(); GROUPS],
        }
    }

    /// Whether `label` starts with the prefix, ignoring case, and goes on
    /// past it: offering exactly what is already typed only keeps the menu
    /// open over a word that is finished. Allocates nothing, since it runs
    /// over the whole catalog on every keystroke.
    fn matches(&self, label: &str) -> bool {
        let mut rest = label.chars().flat_map(char::to_lowercase);
        self.prefix.chars().all(|typed| rest.next() == Some(typed)) && rest.next().is_some()
    }

    fn push(&mut self, suggestion: Suggestion) {
        let group = group_of(suggestion.kind);
        if self.seen.insert((suggestion.label.clone(), group)) {
            self.groups[group].push(suggestion);
        }
    }

    fn name(&mut self, name: &str, kind: SuggestionKind, detail: String) {
        if self.matches(name) {
            self.push(Suggestion {
                label: name.to_string(),
                insert: quote(name, self.engine),
                kind,
                detail,
            });
        }
    }

    fn keyword(&mut self, keyword: &str) {
        if self.matches(keyword) {
            let insert = if self.upper {
                keyword.to_string()
            } else {
                keyword.to_ascii_lowercase()
            };
            self.push(Suggestion {
                label: insert.clone(),
                insert,
                kind: SuggestionKind::Keyword,
                detail: "Keyword".into(),
            });
        }
    }

    /// The lead group, then keywords, then the rest. Columns keep their
    /// table's own order; every other group is sorted shortest first, since a
    /// short name is the one a prefix pins down.
    fn finish(mut self) -> Vec<Suggestion> {
        let columns = group_of(SuggestionKind::Column);
        for (index, group) in self.groups.iter_mut().enumerate() {
            if index != columns {
                group.sort_by(|a, b| (a.label.len(), &a.label).cmp(&(b.label.len(), &b.label)));
            }
        }
        let keywords = group_of(SuggestionKind::Keyword);
        let mut order = vec![self.lead, keywords];
        order.extend((0..GROUPS).filter(|group| *group != self.lead && *group != keywords));
        order
            .into_iter()
            .flat_map(|group| std::mem::take(&mut self.groups[group]))
            .take(MAX_SUGGESTIONS)
            .collect()
    }
}

/// How many groups [`group_of`] sorts suggestions into.
const GROUPS: usize = 5;

/// Which group a kind is listed in, in the order groups follow the keywords.
fn group_of(kind: SuggestionKind) -> usize {
    match kind {
        SuggestionKind::Column => 0,
        SuggestionKind::Table | SuggestionKind::View => 1,
        SuggestionKind::Schema => 2,
        SuggestionKind::Routine => 3,
        SuggestionKind::Keyword => 4,
    }
}

/// `name` as it can be pasted into a statement: bare when the engine reads it
/// back as the same name, quoted otherwise.
fn quote(name: &str, engine: Engine) -> String {
    let plain = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !RESERVED.contains(&name.to_ascii_uppercase().as_str());
    // Postgres folds a bare name to lower case, so an upper-case letter needs
    // quotes there; MySQL and SQLite match names regardless of case.
    let folds = engine == Engine::Postgres && name.chars().any(|c| c.is_ascii_uppercase());
    if plain && !folds {
        return name.to_string();
    }
    // Not `sql::quote_identifier`, which leaves a reserved word bare.
    match engine {
        Engine::MySql => format!("`{}`", name.replace('`', "``")),
        Engine::Postgres | Engine::Sqlite => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::catalog::CatalogEntry;
    use crate::db::{DatabaseObject, ObjectKind, StoredKind, StoredObject};

    fn table(schema: Option<&str>, name: &str) -> DatabaseObject {
        DatabaseObject {
            schema: schema.map(str::to_string),
            name: name.into(),
            kind: ObjectKind::Table,
        }
    }

    fn catalog() -> Catalog {
        let users = table(Some("public"), "users");
        let orders = table(Some("public"), "orders");
        let events = table(Some("audit"), "events");
        let column = |owner: &DatabaseObject, name: &str, kind: &str| {
            CatalogEntry::member(CatalogKind::Column, owner.clone(), name.into(), kind.into())
        };
        let entries = vec![
            CatalogEntry::object(users.clone()),
            CatalogEntry::object(orders.clone()),
            CatalogEntry::object(events.clone()),
            column(&users, "id", "integer"),
            column(&users, "email", "text"),
            column(&users, "Created", "timestamp"),
            column(&orders, "id", "integer"),
            column(&orders, "user_id", "integer"),
            column(&orders, "order", "integer"),
            column(&events, "kind", "text"),
            CatalogEntry::routine(StoredObject {
                schema: Some("public".into()),
                name: "uuid_generate".into(),
                arguments: Some("".into()),
                kind: StoredKind::Function,
            }),
        ];
        Catalog {
            total: entries.len(),
            entries,
        }
    }

    /// The labels offered with the caret at the `|` in `sql`.
    fn labels(sql: &str) -> Vec<String> {
        labels_on(sql, Engine::Postgres)
    }

    fn labels_on(sql: &str, engine: Engine) -> Vec<String> {
        let cursor = sql.find('|').expect("a caret marker");
        let sql = sql.replacen('|', "", 1);
        complete(&sql, cursor, &catalog(), engine)
            .map(|completions| completions.items.into_iter().map(|s| s.label).collect())
            .unwrap_or_default()
    }

    fn inserts(sql: &str) -> Vec<String> {
        let cursor = sql.find('|').expect("a caret marker");
        let sql = sql.replacen('|', "", 1);
        complete(&sql, cursor, &catalog(), Engine::Postgres)
            .map(|completions| completions.items.into_iter().map(|s| s.insert).collect())
            .unwrap_or_default()
    }

    #[test]
    fn keywords_follow_the_case_being_typed() {
        assert_eq!(labels("SEL|"), ["SELECT"]);
        assert_eq!(labels("sel|"), ["select"]);
    }

    #[test]
    fn a_finished_word_offers_nothing() {
        assert!(labels("SELECT|").is_empty());
        assert!(labels("SELECT |").is_empty());
    }

    #[test]
    fn after_from_tables_come_first() {
        let offered = labels("SELECT * FROM u|");
        assert_eq!(offered.first().map(String::as_str), Some("users"));
        assert!(!offered.contains(&"user_id".to_string()));
        assert!(offered.contains(&"union".to_string()));
    }

    #[test]
    fn columns_of_the_named_table_come_first() {
        let offered = labels("SELECT e| FROM users");
        assert_eq!(offered.first().map(String::as_str), Some("email"));
        // `events` is a table, offered after the columns and keywords.
        assert!(offered.contains(&"events".to_string()));
        assert!(
            offered.iter().position(|l| l == "email") < offered.iter().position(|l| l == "events")
        );
    }

    #[test]
    fn an_alias_qualifies_its_tables_columns() {
        assert_eq!(
            labels("SELECT * FROM users u JOIN orders o ON o.| = u.id"),
            ["id", "user_id", "order"]
        );
        assert_eq!(labels("SELECT u.e| FROM users AS u"), ["email"]);
    }

    #[test]
    fn a_table_name_qualifies_its_columns_without_an_alias() {
        assert_eq!(
            labels("SELECT users.| FROM users"),
            ["id", "email", "Created"]
        );
    }

    #[test]
    fn a_schema_qualifies_its_tables() {
        assert_eq!(labels("SELECT * FROM audit.|"), ["events"]);
        assert_eq!(labels("SELECT audit.events.| FROM audit.events"), ["kind"]);
    }

    #[test]
    fn a_comma_separated_from_list_names_every_table() {
        let offered = labels("SELECT k|, e FROM users, audit.events ev");
        assert_eq!(offered.first().map(String::as_str), Some("kind"));
        assert_eq!(labels("SELECT ev.| FROM users, audit.events ev"), ["kind"]);
    }

    #[test]
    fn an_insert_column_list_offers_columns() {
        let offered = labels("INSERT INTO orders (u|");
        assert_eq!(offered.first().map(String::as_str), Some("user_id"));
    }

    #[test]
    fn nothing_inside_a_string_or_comment() {
        assert!(labels("SELECT 'us|").is_empty());
        assert!(labels("SELECT 1 -- us|").is_empty());
        assert!(labels("SELECT 1 /* us|").is_empty());
        // A closed string before the caret is no obstacle.
        assert_eq!(labels("SELECT 'x' FROM us|"), ["users", "using"]);
    }

    #[test]
    fn names_are_quoted_where_the_engine_needs_it() {
        assert_eq!(inserts("SELECT C| FROM users")[0], "\"Created\"");
        assert_eq!(inserts("SELECT o.o| FROM orders o"), ["\"order\""]);
        assert_eq!(
            labels_on("SELECT C| FROM users", Engine::MySql)[0],
            "Created"
        );
    }

    #[test]
    fn the_statement_the_caret_is_in_names_the_tables() {
        let offered = labels("SELECT * FROM orders; SELECT u| FROM users");
        assert!(!offered.contains(&"user_id".to_string()));
        assert!(labels("SELECT * FROM orders; SELECT u|").contains(&"user_id".to_string()));
    }

    #[test]
    fn before_a_from_any_column_is_offered() {
        assert!(labels("SELECT user|").contains(&"user_id".to_string()));
    }

    #[test]
    fn the_replaced_range_is_the_word_being_typed() {
        let completions = complete("SELECT ema FROM users", 10, &catalog(), Engine::Postgres)
            .expect("completions");
        assert_eq!(completions.replace, 7..10);
        assert_eq!(completions.prefix, "ema");
        let completions = complete("SELECT u. FROM users u", 9, &catalog(), Engine::Postgres)
            .expect("completions");
        assert_eq!(completions.replace, 9..9);
    }

    #[test]
    fn engine_keywords_are_offered_on_their_engine_only() {
        assert_eq!(labels_on("PRAG|", Engine::Sqlite), ["PRAGMA"]);
        assert!(labels_on("PRAG|", Engine::Postgres).is_empty());
    }
}
