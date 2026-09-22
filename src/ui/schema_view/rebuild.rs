//! The SQLite table rebuild `SchemaView` falls back to.
//!
//! SQLite's own `ALTER TABLE` only reaches a rename and an add or drop of a
//! plain column. A column's type, nullability, or default, or a primary or
//! foreign key, need sqlite.org's "Making Other Kinds Of Table Schema Changes":
//! copy the table into a new one with the edited definition, drop the old one,
//! rename the new one into its place, and put its indexes, triggers, and views
//! back. [`plan`] generates that body; `Connection::rebuild_table` runs it as
//! one transaction with foreign keys off around it.
//!
//! The generated SQL is still DDL for one screen, kept beside the view rather
//! than in `db/`, like `sql.rs` next to it.

use crate::db::{DatabaseObject, Engine, RebuildSource, quote_identifier};

use super::sql::{ColumnEdit, ForeignKeyEdit, IndexEdit, column_clause, target};

/// What `SchemaView` has staged to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// Statements that run one at a time, as they always have.
    Statements(Vec<String>),
    /// One SQLite table rebuild, run atomically on a dedicated connection.
    Rebuild(Vec<String>),
}

impl Change {
    pub(crate) fn statements(&self) -> &[String] {
        match self {
            Change::Statements(statements) | Change::Rebuild(statements) => statements,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.statements().is_empty()
    }

    pub(crate) fn is_rebuild(&self) -> bool {
        matches!(self, Change::Rebuild(_))
    }
}

/// Every statement the form's edits call for, on `engine`.
///
/// Postgres and MySQL restate a table in place, so they stay a flat list of
/// statements. SQLite does too while its native `ALTER TABLE` will do, and
/// switches to one [`Change::Rebuild`] as soon as it will not.
pub(crate) fn plan(
    engine: Engine,
    object: &DatabaseObject,
    columns: &[ColumnEdit],
    indexes: &[IndexEdit],
    foreign_keys: &[ForeignKeyEdit],
    source: Option<&RebuildSource>,
) -> Result<Change, String> {
    if engine == Engine::Sqlite {
        return sqlite_plan(object, columns, indexes, foreign_keys, source);
    }

    let target = target(object, engine);
    let mut statements = super::sql::generate_alter_statements(engine, &target, columns)?;
    statements.extend(super::sql::generate_index_statements(
        engine, object, indexes,
    )?);
    statements.extend(super::sql::generate_foreign_key_statements(
        engine,
        object,
        foreign_keys,
    )?);
    Ok(Change::Statements(statements))
}

fn sqlite_plan(
    object: &DatabaseObject,
    columns: &[ColumnEdit],
    indexes: &[IndexEdit],
    foreign_keys: &[ForeignKeyEdit],
    source: Option<&RebuildSource>,
) -> Result<Change, String> {
    if !needs_rebuild(columns, indexes, foreign_keys, source) {
        return native_plan(object, columns, indexes, foreign_keys);
    }

    let source = source.ok_or_else(|| {
        "the table's stored definition could not be read, so it cannot be rebuilt".to_string()
    })?;
    generate(object, columns, indexes, foreign_keys, source)
}

/// Whether the edits reach something SQLite's plain `ALTER TABLE` cannot do.
fn needs_rebuild(
    columns: &[ColumnEdit],
    indexes: &[IndexEdit],
    foreign_keys: &[ForeignKeyEdit],
    source: Option<&RebuildSource>,
) -> bool {
    columns.iter().any(ColumnEdit::rebuilds)
        || indexes.iter().any(|edit| {
            // Adding a primary key, dropping one, or dropping the auto-index
            // behind a `UNIQUE` constraint (which SQLite will not let go with
            // a plain `DROP INDEX`): all need the table restated. A plain
            // index does not.
            let dropped_auto = edit.dropped
                && edit.original.as_ref().is_some_and(|original| {
                    !original.is_primary_key && !has_stored_sql(source, &original.name)
                });
            (edit.is_new() && edit.primary_key && !edit.dropped && !edit.columns.is_empty())
                || (edit.dropped
                    && edit
                        .original
                        .as_ref()
                        .is_some_and(|original| original.is_primary_key))
                || dropped_auto
        })
        || foreign_keys.iter().any(ForeignKeyEdit::wants_a_statement)
}

/// Whether `name` is one of the table's explicit indexes rather than the
/// auto-index behind a `UNIQUE`/`PRIMARY KEY` constraint.
fn has_stored_sql(source: Option<&RebuildSource>, name: &str) -> bool {
    source.is_some_and(|source| source.indexes.iter().any(|stored| stored.name == name))
}

/// The changes SQLite's own `ALTER TABLE` can express: a rename, an add, a
/// drop, a plain index, all as separate statements in the usual order.
fn native_plan(
    object: &DatabaseObject,
    columns: &[ColumnEdit],
    indexes: &[IndexEdit],
    foreign_keys: &[ForeignKeyEdit],
) -> Result<Change, String> {
    let engine = Engine::Sqlite;
    let target = target(object, engine);
    let mut statements = Vec::new();

    for edit in columns {
        if !edit.changed() {
            continue;
        }
        if edit.name.trim().is_empty() {
            return Err("a column needs a name".to_string());
        }
        // `needs_rebuild` established that nothing here retypes, drops
        // nullability, or changes a default, so a rename is all that is left.
        let original = edit
            .original
            .as_ref()
            .expect("a changed column has an original");
        statements.push(format!(
            "ALTER TABLE {target} RENAME COLUMN {} TO {}",
            quote_identifier(&original.name, engine),
            quote_identifier(&edit.name, engine)
        ));
    }

    for edit in columns {
        if edit.is_new() && !edit.dropped && !edit.name.trim().is_empty() {
            statements.push(format!(
                "ALTER TABLE {target} ADD COLUMN {}",
                column_clause(engine, edit)
            ));
        }
    }

    for edit in columns {
        if edit.dropped
            && let Some(original) = &edit.original
        {
            statements.push(format!(
                "ALTER TABLE {target} DROP COLUMN {}",
                quote_identifier(&original.name, engine)
            ));
        }
    }

    statements.extend(super::sql::generate_index_statements(
        engine, object, indexes,
    )?);
    statements.extend(super::sql::generate_foreign_key_statements(
        engine,
        object,
        foreign_keys,
    )?);
    Ok(Change::Statements(statements))
}

/// The whole copy-and-swap, as the statements `Connection::rebuild_table`
/// runs in one transaction.
fn generate(
    object: &DatabaseObject,
    columns: &[ColumnEdit],
    indexes: &[IndexEdit],
    foreign_keys: &[ForeignKeyEdit],
    source: &RebuildSource,
) -> Result<Change, String> {
    let engine = Engine::Sqlite;
    let name = object.name.as_str();
    let table = quote_identifier(name, engine);
    // SQLite has no schemas, so the scratch name is the only qualification.
    let scratch = quote_identifier(&format!("new_{name}"), engine);

    // Anything the rebuild would otherwise drop on the floor stops here,
    // rather than quietly changing the table.
    if let Some(reason) = unsupported(&source.table_sql) {
        return Err(reason);
    }

    let renames = rename_map(columns);

    // The new table's columns, in the form's order. A dropped column is left
    // out; a column added by hand starts empty on the other side.
    let mut definitions = Vec::new();
    let mut copied: Vec<(String, String)> = Vec::new();
    let mut dropped = Vec::new();
    for edit in columns {
        if edit.dropped {
            if let Some(original) = &edit.original {
                dropped.push(original.name.clone());
            }
            continue;
        }
        if edit.name.trim().is_empty() {
            if edit.is_new() {
                continue;
            }
            return Err("a column needs a name".to_string());
        }

        definitions.push(column_clause(engine, edit));
        if let Some(original) = &edit.original {
            copied.push((edit.name.clone(), original.name.clone()));
        }
    }

    let mut constraints: Vec<String> = Vec::new();
    // A key or constraint whose column is being dropped goes with it, rather
    // than leaving the new table naming a column that is no longer there.
    let uses_dropped = |columns: &[String]| {
        columns
            .iter()
            .any(|column| dropped.iter().any(|name| name.eq_ignore_ascii_case(column)))
    };
    let mut primary_key = primary_key(columns, indexes, &renames);
    if uses_dropped(&primary_key) {
        primary_key.clear();
    }
    if !primary_key.is_empty() {
        constraints.push(format!("PRIMARY KEY ({})", columns_list(&primary_key)));
    }

    let mut recreations = Vec::new();
    for edit in indexes {
        if edit.dropped {
            continue;
        }

        let Some(original) = &edit.original else {
            // An index added by hand.
            if edit.primary_key || edit.columns.is_empty() || uses_dropped(&edit.columns) {
                continue;
            }
            if edit.name.trim().is_empty() {
                return Err("an index needs a name".to_string());
            }
            let columns: Vec<String> = edit
                .columns
                .iter()
                .map(|column| renamed(column, &renames))
                .collect();
            recreations.push(format!(
                "CREATE {}INDEX {} ON {table} ({})",
                if edit.unique { "UNIQUE " } else { "" },
                quote_identifier(&edit.name, engine),
                columns_list(&columns)
            ));
            continue;
        };

        // The primary key is restated with the table, not as an index.
        if original.is_primary_key {
            continue;
        }
        match source
            .indexes
            .iter()
            .find(|stored| stored.name == original.name)
        {
            // An explicit index: replayed as written, so a partial or
            // expression index keeps what `IndexDef` cannot describe.
            Some(stored) => {
                refuse_dropped_reference(&stored.sql, &dropped, "index", &stored.name)?;
                recreations.push(rewrite_identifiers(&stored.sql, &renames));
            }
            // No SQL of its own: the auto-index behind a `UNIQUE` constraint,
            // which goes back as a table constraint.
            None if original.unique && !uses_dropped(&original.columns) => {
                let columns: Vec<String> = original
                    .columns
                    .iter()
                    .map(|column| renamed(column, &renames))
                    .collect();
                constraints.push(format!("UNIQUE ({})", columns_list(&columns)));
            }
            None => {}
        }
    }

    for edit in foreign_keys {
        if edit.dropped
            || (edit.is_new() && !edit.wants_a_statement())
            || uses_dropped(&edit.columns)
        {
            continue;
        }
        constraints.push(foreign_key_clause(engine, edit, &renames));
    }

    // A view that reads a dropped column would be left dangling by the copy,
    // so it stops the rebuild the same way an index does.
    for view in &source.views {
        if mentions(&view.sql, name) {
            refuse_dropped_reference(&view.sql, &dropped, "view", &view.name)?;
        }
    }

    let mut statements = Vec::new();
    let mut body = definitions;
    body.extend(constraints);
    statements.push(format!(
        "CREATE TABLE {scratch} (\n    {}\n)",
        body.join(",\n    ")
    ));

    if !copied.is_empty() {
        let destinations: Vec<String> = copied
            .iter()
            .map(|(destination, _)| quote_identifier(destination, engine))
            .collect();
        let origins: Vec<String> = copied
            .iter()
            .map(|(_, origin)| quote_identifier(origin, engine))
            .collect();
        statements.push(format!(
            "INSERT INTO {scratch} ({}) SELECT {} FROM {table}",
            destinations.join(", "),
            origins.join(", ")
        ));
    }

    statements.push(format!("DROP TABLE {table}"));
    statements.push(format!("ALTER TABLE {scratch} RENAME TO {table}"));
    statements.extend(recreations);

    for trigger in &source.triggers {
        refuse_dropped_reference(&trigger.sql, &dropped, "trigger", &trigger.name)?;
        statements.push(rewrite_identifiers(&trigger.sql, &renames));
    }

    // A view survives the table being dropped and renamed, so it only needs
    // touching when a column it reads changed name.
    if !renames.is_empty() {
        for view in &source.views {
            if !mentions(&view.sql, name) {
                continue;
            }
            statements.push(format!(
                "DROP VIEW IF EXISTS {}",
                quote_identifier(&view.name, engine)
            ));
            statements.push(rewrite_identifiers(&view.sql, &renames));
        }
    }

    Ok(Change::Rebuild(statements))
}

/// Why a table's stored definition cannot be rebuilt, if it cannot.
///
/// `TableSchema` models columns, indexes, and foreign keys, and nothing else,
/// so a `CHECK`, a collation, a generated column, or a table setting would be
/// silently lost by the copy. Rather than change the table behind the user's
/// back, the rebuild is refused.
fn unsupported(create_sql: &str) -> Option<String> {
    let words: Vec<String> = tokenize(create_sql)
        .into_iter()
        .filter(|token| token.kind == Kind::Word)
        .map(|token| create_sql[token.start..token.end].to_ascii_uppercase())
        .collect();
    let has = |word: &str| words.iter().any(|candidate| candidate == word);

    if words.first().map(String::as_str) == Some("CREATE") && has("VIRTUAL") {
        return Some(
            "this is a virtual table, which cannot be rebuilt; change it in a query tab instead"
                .to_string(),
        );
    }
    if has("CHECK") {
        return Some(
            "this table has a CHECK constraint, which a rebuild would not carry; change it in a \
             query tab instead"
                .to_string(),
        );
    }
    if has("COLLATE") {
        return Some(
            "this table has a COLLATE clause, which a rebuild would not carry; change it in a \
             query tab instead"
                .to_string(),
        );
    }
    if has("GENERATED") {
        return Some(
            "this table has a generated column, which a rebuild would not carry; change it in a \
             query tab instead"
                .to_string(),
        );
    }
    if has("AUTOINCREMENT") {
        return Some(
            "this table's key uses AUTOINCREMENT, which a rebuild would not carry; change it in a \
             query tab instead"
                .to_string(),
        );
    }
    if has("WITHOUT") || has("STRICT") {
        return Some(
            "this table is WITHOUT ROWID or STRICT, which a rebuild would not carry; change it in \
             a query tab instead"
                .to_string(),
        );
    }
    None
}

/// The table's primary key columns, in key order, as the form now has them.
///
/// A rowid-alias key (`INTEGER PRIMARY KEY`) is a flag on its column and no
/// index row, so it comes from the columns; any other key the form lists as an
/// index, whose column order is the key's own and not the table's.
fn primary_key(
    columns: &[ColumnEdit],
    indexes: &[IndexEdit],
    renames: &[(String, String)],
) -> Vec<String> {
    if let Some(added) = indexes
        .iter()
        .find(|edit| edit.is_new() && edit.primary_key && !edit.dropped && !edit.columns.is_empty())
    {
        return added
            .columns
            .iter()
            .map(|column| renamed(column, renames))
            .collect();
    }
    if let Some(existing) = indexes.iter().find(|edit| {
        !edit.dropped
            && !edit.is_new()
            && edit
                .original
                .as_ref()
                .is_some_and(|original| original.is_primary_key)
    }) {
        return existing
            .original
            .as_ref()
            .expect("an existing key has its definition")
            .columns
            .iter()
            .map(|column| renamed(column, renames))
            .collect();
    }
    if indexes.iter().any(|edit| {
        edit.dropped
            && edit
                .original
                .as_ref()
                .is_some_and(|original| original.is_primary_key)
    }) {
        return Vec::new();
    }
    columns
        .iter()
        .filter(|edit| {
            !edit.dropped
                && edit
                    .original
                    .as_ref()
                    .is_some_and(|original| original.is_primary_key)
        })
        .map(|edit| edit.name.clone())
        .collect()
}

fn foreign_key_clause(
    engine: Engine,
    edit: &ForeignKeyEdit,
    renames: &[(String, String)],
) -> String {
    let local: Vec<String> = edit
        .columns
        .iter()
        .map(|column| renamed(column, renames))
        .collect();
    let referenced = quote_identifier(&edit.referenced_table, engine);

    let mut clause = String::new();
    if !edit.name.trim().is_empty() {
        clause.push_str("CONSTRAINT ");
        clause.push_str(&quote_identifier(&edit.name, engine));
        clause.push(' ');
    }
    clause.push_str(&format!(
        "FOREIGN KEY ({}) REFERENCES {referenced} ({})",
        columns_list(&local),
        columns_list(&edit.referenced_columns)
    ));
    clause.push_str(" ON DELETE ");
    clause.push_str(edit.on_delete.label());
    clause.push_str(" ON UPDATE ");
    clause.push_str(edit.on_update.label());
    clause
}

fn columns_list(columns: &[String]) -> String {
    columns
        .iter()
        .map(|column| quote_identifier(column, Engine::Sqlite))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Every column the form has renamed, old name to new.
fn rename_map(columns: &[ColumnEdit]) -> Vec<(String, String)> {
    columns
        .iter()
        .filter_map(|edit| {
            let original = edit.original.as_ref()?;
            (!edit.dropped && original.name != edit.name)
                .then(|| (original.name.clone(), edit.name.clone()))
        })
        .collect()
}

fn renamed(column: &str, renames: &[(String, String)]) -> String {
    renames
        .iter()
        .find(|(old, _)| old.eq_ignore_ascii_case(column))
        .map(|(_, new)| new.clone())
        .unwrap_or_else(|| column.to_string())
}

/// Refuse to replay `sql` when it reads a column the rebuild is dropping; its
/// `CREATE` would fail and roll the whole rebuild back for no clear reason.
fn refuse_dropped_reference(
    sql: &str,
    dropped: &[String],
    kind: &str,
    name: &str,
) -> Result<(), String> {
    for column in dropped {
        if mentions(sql, column) {
            return Err(format!(
                "the {kind} {name} uses the dropped column {column}; remove it first"
            ));
        }
    }
    Ok(())
}

/// Whether `sql` names `identifier` anywhere outside a string or a comment.
fn mentions(sql: &str, identifier: &str) -> bool {
    tokenize(sql).iter().any(|token| {
        matches!(token.kind, Kind::Word | Kind::Quoted)
            && unquote(&sql[token.start..token.end]).eq_ignore_ascii_case(identifier)
    })
}

/// `sql` with every identifier that names a renamed column swapped for its new
/// name, so a stored index/trigger/view follows the rename it was written for.
fn rewrite_identifiers(sql: &str, renames: &[(String, String)]) -> String {
    if renames.is_empty() {
        return sql.to_string();
    }

    let mut output = String::with_capacity(sql.len());
    let mut cursor = 0;
    for token in tokenize(sql) {
        output.push_str(&sql[cursor..token.start]);
        let text = &sql[token.start..token.end];
        let replacement = match token.kind {
            Kind::Word | Kind::Quoted => {
                let identifier = unquote(text);
                renames
                    .iter()
                    .find(|(old, _)| old.eq_ignore_ascii_case(&identifier))
                    .map(|(_, new)| match token.kind {
                        Kind::Quoted => quote_like(text, new),
                        _ if is_bare(new) => new.clone(),
                        _ => quote_like("\"\"", new),
                    })
            }
            _ => None,
        };
        output.push_str(replacement.as_deref().unwrap_or(text));
        cursor = token.end;
    }
    output.push_str(&sql[cursor..]);
    output
}

/// `value` quoted the way `original` was, so a rewrite keeps the stored SQL's
/// own style.
fn quote_like(original: &str, value: &str) -> String {
    match original.as_bytes().first() {
        Some(b'[') => format!("[{value}]"),
        Some(b'`') => format!("`{}`", value.replace('`', "``")),
        _ => format!("\"{}\"", value.replace('"', "\"\"")),
    }
}

/// Whether `name` can be written bare, without quotes.
fn is_bare(name: &str) -> bool {
    let mut characters = name.chars();
    match characters.next() {
        Some(character) if character.is_ascii_alphabetic() || character == '_' => {}
        _ => return false,
    }
    name.chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Word,
    /// A `'...'` string literal, whose contents are data and never an
    /// identifier.
    String,
    /// A `"..."`, `` `...` ``, or `[...]` quoted identifier.
    Quoted,
    Punct,
    Space,
    Comment,
}

struct Token {
    start: usize,
    end: usize,
    kind: Kind,
}

/// Split `sql` into the tokens the identifier scan needs, treating comments
/// and string literals as opaque so nothing inside them is mistaken for a
/// name.
fn tokenize(sql: &str) -> Vec<Token> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let start = index;
        let kind = match bytes[index] {
            b' ' | b'\t' | b'\r' | b'\n' => {
                while index < bytes.len() && matches!(bytes[index], b' ' | b'\t' | b'\r' | b'\n') {
                    index += 1;
                }
                Kind::Space
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                Kind::Comment
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index < bytes.len()
                    && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
                Kind::Comment
            }
            quote @ (b'\'' | b'"' | b'`') => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == quote {
                        if bytes.get(index + 1) == Some(&quote) {
                            index += 2;
                            continue;
                        }
                        index += 1;
                        break;
                    }
                    index += 1;
                }
                if quote == b'\'' {
                    Kind::String
                } else {
                    Kind::Quoted
                }
            }
            b'[' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b']' {
                    index += 1;
                }
                if index < bytes.len() {
                    index += 1;
                }
                Kind::Quoted
            }
            byte if byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' => {
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric()
                        || bytes[index] == b'_'
                        || bytes[index] == b'$')
                {
                    index += 1;
                }
                Kind::Word
            }
            _ => {
                index += 1;
                Kind::Punct
            }
        };
        tokens.push(Token {
            start,
            end: index,
            kind,
        });
    }
    tokens
}

/// The identifier `text` holds, with its quotes removed and a doubled quote
/// unescaped.
fn unquote(text: &str) -> String {
    let Some(quote) = text.chars().next() else {
        return String::new();
    };
    let close = if quote == '[' { ']' } else { quote };
    if !matches!(quote, '"' | '`' | '[' | '\'') || text.len() < 2 || !text.ends_with(close) {
        return text.to_string();
    }

    let inner = &text[quote.len_utf8()..text.len() - close.len_utf8()];
    inner.replace(&format!("{quote}{quote}"), &quote.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::{ColumnDef, ReferentialAction, StoredDefinition};
    use crate::db::{ForeignKeyDef, IndexDef};

    fn column(name: &str, type_name: &str, nullable: bool, default: Option<&str>) -> ColumnEdit {
        ColumnEdit {
            original: Some(ColumnDef {
                name: name.to_string(),
                type_name: type_name.to_string(),
                nullable,
                default: default.map(str::to_string),
                is_primary_key: false,
                mysql_extra: Default::default(),
            }),
            name: name.to_string(),
            type_name: type_name.to_string(),
            nullable,
            default: default.map(str::to_string),
            dropped: false,
        }
    }

    fn table(name: &str) -> DatabaseObject {
        DatabaseObject {
            schema: None,
            name: name.to_string(),
            kind: crate::db::ObjectKind::Table,
        }
    }

    fn source(create: &str) -> RebuildSource {
        RebuildSource {
            table_sql: create.to_string(),
            ..RebuildSource::default()
        }
    }

    const ITEMS: &str = "CREATE TABLE items (\n    id INTEGER PRIMARY KEY,\n    name TEXT\n)";

    #[test]
    fn a_retype_becomes_a_copy_and_swap() {
        let mut score = column("score", "REAL", true, None);
        score.type_name = "TEXT".to_string();

        let change = generate(
            &table("items"),
            &[
                column("id", "INTEGER", true, None),
                column("name", "TEXT", true, None),
                score,
            ],
            &[],
            &[],
            &source(ITEMS),
        )
        .expect("a plain table should rebuild");

        let Change::Rebuild(statements) = change else {
            panic!("a retype should be a rebuild");
        };
        assert_eq!(
            statements,
            [
                "CREATE TABLE new_items (\n    id INTEGER,\n    name TEXT,\n    score TEXT\n)",
                "INSERT INTO new_items (id, name, score) SELECT id, name, score FROM items",
                "DROP TABLE items",
                "ALTER TABLE new_items RENAME TO items",
            ]
        );
    }

    #[test]
    fn an_added_column_is_left_out_of_the_copy() {
        let added = ColumnEdit {
            original: None,
            name: "note".to_string(),
            type_name: "TEXT".to_string(),
            nullable: true,
            default: None,
            dropped: false,
        };
        let mut score = column("score", "REAL", true, None);
        score.type_name = "TEXT".to_string();

        let change = generate(
            &table("items"),
            &[column("id", "INTEGER", true, None), score, added],
            &[],
            &[],
            &source(ITEMS),
        )
        .unwrap();

        let Change::Rebuild(statements) = change else {
            panic!("expected a rebuild");
        };
        assert!(
            statements[1].contains("INSERT INTO new_items (id, score) SELECT id, score FROM items")
        );
    }

    #[test]
    fn a_rename_is_spelled_out_in_the_copy() {
        let mut renamed = column("name", "TEXT", true, None);
        renamed.name = "headline".to_string();

        let change = generate(
            &table("items"),
            &[column("id", "INTEGER", true, None), renamed],
            &[],
            &[],
            &source(ITEMS),
        )
        .unwrap();

        let Change::Rebuild(statements) = change else {
            panic!("expected a rebuild");
        };
        assert_eq!(
            statements[1],
            "INSERT INTO new_items (id, headline) SELECT id, name FROM items"
        );
    }

    #[test]
    fn an_explicit_index_is_replayed() {
        let mut source = source(ITEMS);
        source.indexes.push(StoredDefinition {
            name: "items_name_idx".to_string(),
            sql: "CREATE INDEX items_name_idx ON items (name)".to_string(),
        });
        let index = IndexEdit {
            original: Some(IndexDef {
                name: "items_name_idx".to_string(),
                columns: vec!["name".to_string()],
                unique: false,
                is_primary_key: false,
            }),
            name: "items_name_idx".to_string(),
            columns: vec!["name".to_string()],
            unique: false,
            primary_key: false,
            dropped: false,
        };

        let mut score = column("score", "REAL", true, None);
        score.type_name = "TEXT".to_string();
        let change = generate(
            &table("items"),
            &[
                column("id", "INTEGER", true, None),
                column("name", "TEXT", true, None),
                score,
            ],
            &[index],
            &[],
            &source,
        )
        .unwrap();

        let Change::Rebuild(statements) = change else {
            panic!("expected a rebuild");
        };
        assert!(
            statements
                .iter()
                .any(|statement| statement == "CREATE INDEX items_name_idx ON items (name)")
        );
    }

    #[test]
    fn a_unique_constraint_goes_back_as_one() {
        let source = source(ITEMS);
        // The auto-index behind `UNIQUE` has no SQL of its own.
        let index = IndexEdit {
            original: Some(IndexDef {
                name: "sqlite_autoindex_items_1".to_string(),
                columns: vec!["name".to_string()],
                unique: true,
                is_primary_key: false,
            }),
            name: "sqlite_autoindex_items_1".to_string(),
            columns: vec!["name".to_string()],
            unique: true,
            primary_key: false,
            dropped: false,
        };
        let mut score = column("score", "REAL", true, None);
        score.type_name = "TEXT".to_string();

        let change = generate(
            &table("items"),
            &[
                column("id", "INTEGER", true, None),
                column("name", "TEXT", true, None),
                score,
            ],
            &[index],
            &[],
            &source,
        )
        .unwrap();

        let Change::Rebuild(statements) = change else {
            panic!("expected a rebuild");
        };
        assert!(statements[0].contains("UNIQUE (name)"));
    }

    #[test]
    fn a_check_constraint_refuses_the_rebuild() {
        let create = "CREATE TABLE items (id INTEGER PRIMARY KEY, n INTEGER CHECK (n > 0))";
        let mut n = column("n", "INTEGER", true, None);
        n.type_name = "TEXT".to_string();

        let error = generate(
            &table("items"),
            &[column("id", "INTEGER", true, None), n],
            &[],
            &[],
            &source(create),
        )
        .unwrap_err();
        assert!(error.contains("CHECK"));
    }

    #[test]
    fn a_dropped_column_that_an_index_reads_is_refused() {
        let mut source = source(ITEMS);
        source.indexes.push(StoredDefinition {
            name: "items_name_idx".to_string(),
            sql: "CREATE INDEX items_name_idx ON items (name)".to_string(),
        });

        let mut name = column("name", "TEXT", true, None);
        name.dropped = true;
        let mut score = column("score", "REAL", true, None);
        score.type_name = "TEXT".to_string();
        let index = IndexEdit {
            original: Some(IndexDef {
                name: "items_name_idx".to_string(),
                columns: vec!["name".to_string()],
                unique: false,
                is_primary_key: false,
            }),
            name: "items_name_idx".to_string(),
            columns: vec!["name".to_string()],
            unique: false,
            primary_key: false,
            dropped: false,
        };

        let error = generate(
            &table("items"),
            &[column("id", "INTEGER", true, None), name, score],
            &[index],
            &[],
            &source,
        )
        .unwrap_err();
        assert!(error.contains("items_name_idx") && error.contains("name"));
    }

    #[test]
    fn a_foreign_key_row_becomes_a_table_constraint() {
        let edit = ForeignKeyEdit {
            original: None,
            name: "items_tag_fkey".to_string(),
            columns: vec!["name".to_string()],
            referenced_schema: None,
            referenced_table: "tags".to_string(),
            referenced_columns: vec!["id".to_string()],
            on_delete: ReferentialAction::Cascade,
            on_update: ReferentialAction::NoAction,
            dropped: false,
        };
        let mut score = column("score", "REAL", true, None);
        score.type_name = "TEXT".to_string();

        let change = generate(
            &table("items"),
            &[
                column("id", "INTEGER", true, None),
                column("name", "TEXT", true, None),
                score,
            ],
            &[],
            &[edit],
            &source("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, score REAL)"),
        )
        .unwrap();

        let Change::Rebuild(statements) = change else {
            panic!("expected a rebuild");
        };
        assert!(statements[0].contains(
            "CONSTRAINT items_tag_fkey FOREIGN KEY (name) REFERENCES tags (id) \
                 ON DELETE CASCADE ON UPDATE NO ACTION"
        ));
    }

    #[test]
    fn a_constraint_whose_column_is_dropped_goes_with_it() {
        // A composite key and a unique constraint, both reading a column the
        // form is dropping.
        let pk = IndexEdit {
            original: Some(IndexDef {
                name: "sqlite_autoindex_items_1".to_string(),
                columns: vec!["name".to_string(), "score".to_string()],
                unique: true,
                is_primary_key: true,
            }),
            name: "sqlite_autoindex_items_1".to_string(),
            columns: vec!["name".to_string(), "score".to_string()],
            unique: true,
            primary_key: true,
            dropped: false,
        };
        let unique = IndexEdit {
            original: Some(IndexDef {
                name: "sqlite_autoindex_items_2".to_string(),
                columns: vec!["score".to_string()],
                unique: true,
                is_primary_key: false,
            }),
            name: "sqlite_autoindex_items_2".to_string(),
            columns: vec!["score".to_string()],
            unique: true,
            primary_key: false,
            dropped: false,
        };
        let mut score = column("score", "REAL", true, None);
        score.dropped = true;

        let change = generate(
            &table("items"),
            &[column("id", "INTEGER", true, None), column("name", "TEXT", true, None), score],
            &[pk, unique],
            &[],
            &source("CREATE TABLE items (id INTEGER, name TEXT, score REAL, UNIQUE (score), PRIMARY KEY (name, score))"),
        )
        .unwrap();

        let Change::Rebuild(statements) = change else {
            panic!("expected a rebuild");
        };
        assert!(!statements[0].contains("PRIMARY KEY"));
        assert!(!statements[0].contains("UNIQUE"));
        assert!(!statements[1].contains("score"));
    }

    #[test]
    fn identifiers_are_rewritten_outside_strings_and_comments() {
        let sql = "CREATE INDEX i ON items (name) -- name\n/* name */ WHERE body = 'name'";
        assert_eq!(
            rewrite_identifiers(sql, &[("name".to_string(), "title".to_string())]),
            "CREATE INDEX i ON items (title) -- name\n/* name */ WHERE body = 'name'"
        );
    }

    #[test]
    fn a_composite_primary_key_keeps_its_own_column_order() {
        let pk = IndexEdit {
            original: Some(IndexDef {
                name: "sqlite_autoindex_items_1".to_string(),
                columns: vec!["score".to_string(), "name".to_string()],
                unique: true,
                is_primary_key: true,
            }),
            name: "sqlite_autoindex_items_1".to_string(),
            columns: vec!["name".to_string(), "score".to_string()],
            unique: true,
            primary_key: true,
            dropped: false,
        };
        let mut score = column("score", "REAL", true, None);
        score.type_name = "TEXT".to_string();

        let change = generate(
            &table("items"),
            &[
                column("id", "INTEGER", true, None),
                column("name", "TEXT", true, None),
                score,
            ],
            &[pk],
            &[],
            &source(
                "CREATE TABLE items (id INTEGER, name TEXT, score REAL, PRIMARY KEY (score, name))",
            ),
        )
        .unwrap();

        let Change::Rebuild(statements) = change else {
            panic!("expected a rebuild");
        };
        assert!(statements[0].contains("PRIMARY KEY (score, name)"));
    }

    #[test]
    fn a_free_of_change_records_nothing() {
        let mut score = column("score", "REAL", true, None);
        score.type_name = "REAL".to_string();
        let change = sqlite_plan(
            &table("items"),
            &[column("id", "INTEGER", true, None), score],
            &[],
            &[],
            Some(&source(ITEMS)),
        )
        .unwrap();
        assert!(change.is_empty());
    }

    #[test]
    fn a_foreign_key_only_change_rebuilds_even_without_a_column_edit() {
        let edit = ForeignKeyEdit {
            original: Some(ForeignKeyDef {
                name: "fk_1".to_string(),
                columns: vec!["name".to_string()],
                referenced_schema: None,
                referenced_table: "tags".to_string(),
                referenced_columns: vec!["id".to_string()],
                on_delete: ReferentialAction::NoAction,
                on_update: ReferentialAction::NoAction,
            }),
            name: "fk_1".to_string(),
            columns: vec!["name".to_string()],
            referenced_schema: None,
            referenced_table: "tags".to_string(),
            referenced_columns: vec!["id".to_string()],
            on_delete: ReferentialAction::NoAction,
            on_update: ReferentialAction::NoAction,
            dropped: true,
        };

        let change = sqlite_plan(
            &table("items"),
            &[
                column("id", "INTEGER", true, None),
                column("name", "TEXT", true, None),
            ],
            &[],
            &[edit],
            Some(&source(ITEMS)),
        )
        .unwrap();
        assert!(change.is_rebuild());
    }
}
