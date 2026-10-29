//! A table or view's definition as runnable SQL text, plus the read-only
//! extras the structure tab shows beside its editable forms: check
//! constraints, unique/exclusion constraints, and triggers.
//!
//! [`Connection::definition`] is what the structure tab's SQL panel shows:
//! the current database definition, never the form's pending edits. SQLite
//! and MySQL read it straight from the catalog; Postgres has no
//! `SHOW CREATE`, so it is generated from catalog data by the pure
//! [`render_postgres_table`] over already-fetched rows, which keeps the
//! formatting unit-testable without a server.

use anyhow::{Context as _, Result};

use super::config::Engine;
use super::connection::{Connection, DatabaseObject};
use super::query::Cell;
use super::sql::{quote_identifier, quote_literal, quote_literal_for};

/// One `CHECK` constraint: its name (empty when the server stored none) and
/// its full text, e.g. `CHECK (score > 0)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckDef {
    pub name: String,
    pub expression: String,
}

/// One unique or exclusion constraint: its name and its full definition
/// text, e.g. `UNIQUE (email)`. Primary keys are not listed here — the
/// form's index section already shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstraintDef {
    pub name: String,
    pub definition: String,
}

/// One trigger: when it fires, what it fires on, and its full `CREATE
/// TRIGGER` text where the engine reports one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerDef {
    pub name: String,
    /// `BEFORE`, `AFTER`, or `INSTEAD OF`; empty when unknown.
    pub timing: String,
    /// `INSERT`, `UPDATE`, `DELETE`, comma-joined when several; empty when
    /// unknown.
    pub events: String,
    /// `ROW` or `STATEMENT`; empty when unknown.
    pub orientation: String,
    pub definition: String,
}

/// The read-only extras for one table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TableExtras {
    pub checks: Vec<CheckDef>,
    pub constraints: Vec<ConstraintDef>,
    pub triggers: Vec<TriggerDef>,
    /// A trigger read that failed without failing the rest — MySQL's
    /// `SHOW CREATE TRIGGER` needs the `TRIGGER` privilege, so a table's
    /// DDL still shows when its triggers cannot be read.
    pub trigger_error: Option<String>,
}

fn text(row: &[Cell], index: usize) -> String {
    row.get(index).cloned().flatten().unwrap_or_default()
}

fn text_opt(row: &[Cell], index: usize) -> Option<String> {
    row.get(index).cloned().flatten()
}

/// Join stored statements into one text, each terminated with `;`.
fn join_statements(statements: Vec<String>) -> String {
    statements
        .into_iter()
        .map(|statement| {
            let statement = statement.trim().trim_end_matches(';').trim_end();
            format!("{statement};")
        })
        .filter(|statement| statement.len() > 1)
        .collect::<Vec<_>>()
        .join("\n")
}

impl Connection {
    /// Complete, runnable DDL for a table or view: the `CREATE` statement,
    /// then its indexes, then its triggers, each terminated with `;`.
    ///
    /// For a view this is the stored statement [`Self::view_definition`]
    /// returns. For a Postgres table the text is generated from catalog
    /// data and carries a header comment naming what it omits.
    pub async fn definition(&self, object: &DatabaseObject) -> Result<String> {
        if object.kind == super::connection::ObjectKind::View {
            return self.view_definition(object).await;
        }
        match self.config.engine {
            Engine::Sqlite => self.sqlite_definition(object).await,
            Engine::MySql => self.mysql_definition(object).await,
            Engine::Postgres => self.postgres_definition(object).await,
        }
    }

    /// The read-only extras for a table: checks, unique/exclusion
    /// constraints, and triggers. Views have none.
    pub async fn table_extras(&self, object: &DatabaseObject) -> Result<TableExtras> {
        if object.kind == super::connection::ObjectKind::View {
            return Ok(TableExtras::default());
        }
        match self.config.engine {
            Engine::Sqlite => self.sqlite_extras(object).await,
            Engine::MySql => self.mysql_extras(object).await,
            Engine::Postgres => self.postgres_extras(object).await,
        }
    }

    async fn sqlite_definition(&self, object: &DatabaseObject) -> Result<String> {
        let sql = format!(
            "SELECT sql FROM sqlite_master WHERE tbl_name = {} AND sql IS NOT NULL \
             ORDER BY CASE type WHEN 'table' THEN 0 WHEN 'index' THEN 1 WHEN 'trigger' THEN 2 ELSE 3 END, name",
            quote_literal(&object.name)
        );
        let result = self.run_query(&sql).await?;
        let statements: Vec<String> = result
            .rows
            .iter()
            .filter_map(|row| row.first().cloned().flatten())
            .collect();
        if statements.is_empty() {
            anyhow::bail!(
                "the server reported no definition for the table {}",
                object.label()
            );
        }
        Ok(join_statements(statements))
    }

    async fn sqlite_extras(&self, object: &DatabaseObject) -> Result<TableExtras> {
        let sql = format!(
            "SELECT type, name, sql FROM sqlite_master WHERE tbl_name = {} AND sql IS NOT NULL \
             ORDER BY type, name",
            quote_literal(&object.name)
        );
        let result = self.run_query(&sql).await?;
        let mut extras = TableExtras::default();
        for row in &result.rows {
            match text(row, 0).as_str() {
                "table" => extras.checks = parse_sqlite_checks(&text(row, 2)),
                "trigger" => extras
                    .triggers
                    .push(parse_sqlite_trigger(&text(row, 1), &text(row, 2))),
                _ => {}
            }
        }
        Ok(extras)
    }

    async fn mysql_definition(&self, object: &DatabaseObject) -> Result<String> {
        let sql = format!(
            "SHOW CREATE TABLE {}",
            quote_identifier(&object.name, Engine::MySql)
        );
        let result = self.run_query(&sql).await?;
        let create = result
            .rows
            .first()
            .and_then(|row| row.get(1).cloned().flatten())
            .filter(|create| !create.trim().is_empty())
            .with_context(|| {
                format!(
                    "the server reported no definition for the table {}",
                    object.label()
                )
            })?;
        let mut statements = vec![create];
        // A trigger read that fails (missing TRIGGER privilege) must not
        // take the table's own DDL down with it; the extras carry the error.
        if let Ok(triggers) = self.mysql_trigger_rows(object).await {
            statements.extend(triggers.into_iter().map(|trigger| trigger.definition));
        }
        Ok(join_statements(statements))
    }

    /// One row per trigger on `object`, reconstructed from
    /// `information_schema` so no column position of `SHOW CREATE TRIGGER`
    /// is assumed. The caller decides what a failure means.
    async fn mysql_trigger_rows(&self, object: &DatabaseObject) -> Result<Vec<TriggerDef>> {
        let sql = format!(
            "SELECT TRIGGER_NAME, ACTION_TIMING, EVENT_MANIPULATION, ACTION_ORIENTATION, ACTION_STATEMENT \
             FROM information_schema.triggers \
             WHERE EVENT_OBJECT_SCHEMA = DATABASE() AND EVENT_OBJECT_TABLE = {} \
             ORDER BY TRIGGER_NAME",
            quote_literal_for(Engine::MySql, &object.name)
        );
        let result = self.run_query(&sql).await?;
        Ok(result
            .rows
            .iter()
            .map(|row| {
                let name = text(row, 0);
                let timing = text(row, 1);
                let event = text(row, 2);
                let orientation = text(row, 3);
                let body = text(row, 4);
                TriggerDef {
                    definition: format!(
                        "CREATE TRIGGER {} {} {} ON {} FOR EACH {} {}",
                        quote_identifier(&name, Engine::MySql),
                        timing,
                        event,
                        quote_identifier(&object.name, Engine::MySql),
                        orientation,
                        body
                    ),
                    name,
                    timing,
                    events: event,
                    orientation,
                }
            })
            .collect())
    }

    async fn mysql_extras(&self, object: &DatabaseObject) -> Result<TableExtras> {
        let checks_sql = format!(
            "SELECT cc.CONSTRAINT_NAME, cc.CHECK_CLAUSE \
             FROM information_schema.check_constraints cc \
             JOIN information_schema.table_constraints tc \
               ON tc.CONSTRAINT_NAME = cc.CONSTRAINT_NAME \
               AND tc.CONSTRAINT_SCHEMA = cc.CONSTRAINT_SCHEMA \
             WHERE tc.TABLE_SCHEMA = DATABASE() AND tc.TABLE_NAME = {} \
             ORDER BY cc.CONSTRAINT_NAME",
            quote_literal_for(Engine::MySql, &object.name)
        );
        let checks_result = self.run_query(&checks_sql).await?;
        let checks = checks_result
            .rows
            .iter()
            .map(|row| CheckDef {
                name: text(row, 0),
                expression: format!("CHECK ({})", text(row, 1)),
            })
            .collect();

        let unique_sql = format!(
            "SELECT tc.CONSTRAINT_NAME, \
             GROUP_CONCAT(kcu.COLUMN_NAME ORDER BY kcu.ORDINAL_POSITION SEPARATOR ', ') \
             FROM information_schema.table_constraints tc \
             JOIN information_schema.key_column_usage kcu \
               ON kcu.CONSTRAINT_NAME = tc.CONSTRAINT_NAME \
               AND kcu.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA \
             WHERE tc.TABLE_SCHEMA = DATABASE() AND tc.TABLE_NAME = {} \
               AND tc.CONSTRAINT_TYPE = 'UNIQUE' \
             GROUP BY tc.CONSTRAINT_NAME ORDER BY tc.CONSTRAINT_NAME",
            quote_literal_for(Engine::MySql, &object.name)
        );
        let unique_result = self.run_query(&unique_sql).await?;
        let constraints = unique_result
            .rows
            .iter()
            .map(|row| ConstraintDef {
                name: text(row, 0),
                definition: format!("UNIQUE ({})", text(row, 1)),
            })
            .collect();

        let (triggers, trigger_error) = match self.mysql_trigger_rows(object).await {
            Ok(triggers) => (triggers, None),
            Err(error) => (
                Vec::new(),
                Some(format!("could not read triggers ({error:#})")),
            ),
        };

        Ok(TableExtras {
            checks,
            constraints,
            triggers,
            trigger_error,
        })
    }

    async fn postgres_definition(&self, object: &DatabaseObject) -> Result<String> {
        let schema = object.schema.as_deref().unwrap_or("public");
        let parts = self.postgres_table_parts(schema, &object.name).await?;
        if parts.columns.is_empty() {
            anyhow::bail!(
                "the server reported no definition for the table {}",
                object.label()
            );
        }
        Ok(render_postgres_table(&parts))
    }

    async fn postgres_table_parts(&self, schema: &str, table: &str) -> Result<PostgresTableParts> {
        let schema_lit = quote_literal(schema);
        let table_lit = quote_literal(table);
        let class_filter = format!("n.nspname = {schema_lit} AND cl.relname = {table_lit}");

        let columns_sql = format!(
            "SELECT a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull, \
             pg_get_expr(d.adbin, d.adrelid), a.attidentity, a.attgenerated, \
             CASE WHEN a.attcollation <> 0 AND a.attcollation <> t.typcollation \
               THEN c.collname ELSE NULL END \
             FROM pg_attribute a \
             JOIN pg_type t ON t.oid = a.atttypid \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             LEFT JOIN pg_collation c ON c.oid = a.attcollation \
             JOIN pg_class cl ON cl.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = cl.relnamespace \
             WHERE {class_filter} AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum"
        );
        let columns_result = self.run_query(&columns_sql).await?;
        let columns = columns_result
            .rows
            .iter()
            .map(|row| PostgresColumn {
                name: text(row, 0),
                type_name: text(row, 1),
                not_null: text(row, 2) == "1",
                default: text_opt(row, 3),
                identity: text(row, 4),
                generated: text(row, 5),
                collation: text_opt(row, 6),
            })
            .collect();

        let constraints_sql = format!(
            "SELECT con.conname, pg_get_constraintdef(con.oid) \
             FROM pg_constraint con \
             JOIN pg_class cl ON cl.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = cl.relnamespace \
             WHERE {class_filter} ORDER BY con.conname"
        );
        let constraints_result = self.run_query(&constraints_sql).await?;
        let constraints = constraints_result
            .rows
            .iter()
            .map(|row| (text(row, 0), text(row, 1)))
            .collect();

        let indexes_sql = format!(
            "SELECT pg_get_indexdef(ix.indexrelid) \
             FROM pg_index ix \
             JOIN pg_class t ON t.oid = ix.indrelid \
             JOIN pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_namespace n ON n.oid = t.relnamespace \
             LEFT JOIN pg_constraint con ON con.conindid = ix.indexrelid \
             WHERE {class_filter} AND con.oid IS NULL ORDER BY i.relname"
        );
        let indexes_result = self.run_query(&indexes_sql).await?;
        let indexes = indexes_result
            .rows
            .iter()
            .map(|row| text(row, 0))
            .filter(|index| !index.is_empty())
            .collect();

        let triggers_sql = format!(
            "SELECT tgname, pg_get_triggerdef(oid) \
             FROM pg_trigger tg \
             JOIN pg_class cl ON cl.oid = tg.tgrelid \
             JOIN pg_namespace n ON n.oid = cl.relnamespace \
             WHERE {class_filter} AND NOT tg.tgisinternal ORDER BY tg.tgname"
        );
        let triggers_result = self.run_query(&triggers_sql).await?;
        let triggers = triggers_result
            .rows
            .iter()
            .map(|row| text(row, 1))
            .filter(|trigger| !trigger.is_empty())
            .collect();

        let table_comment_sql = format!(
            "SELECT obj_description(cl.oid) FROM pg_class cl \
             JOIN pg_namespace n ON n.oid = cl.relnamespace \
             WHERE {class_filter}"
        );
        let table_comment_result = self.run_query(&table_comment_sql).await?;
        let table_comment = table_comment_result
            .rows
            .first()
            .and_then(|row| row.first().cloned().flatten());

        let column_comments_sql = format!(
            "SELECT a.attname, col_description(a.attrelid, a.attnum) \
             FROM pg_attribute a \
             JOIN pg_class cl ON cl.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = cl.relnamespace \
             WHERE {class_filter} AND a.attnum > 0 AND NOT a.attisdropped \
               AND col_description(a.attrelid, a.attnum) IS NOT NULL \
             ORDER BY a.attnum"
        );
        let column_comments_result = self.run_query(&column_comments_sql).await?;
        let column_comments = column_comments_result
            .rows
            .iter()
            .map(|row| (text(row, 0), text(row, 1)))
            .collect();

        Ok(PostgresTableParts {
            schema: schema.to_string(),
            name: table.to_string(),
            columns,
            constraints,
            indexes,
            triggers,
            table_comment,
            column_comments,
        })
    }

    async fn postgres_extras(&self, object: &DatabaseObject) -> Result<TableExtras> {
        let schema = object.schema.as_deref().unwrap_or("public");
        let schema_lit = quote_literal(schema);
        let table_lit = quote_literal(&object.name);
        let class_filter = format!("n.nspname = {schema_lit} AND cl.relname = {table_lit}");

        let checks_sql = format!(
            "SELECT con.conname, pg_get_constraintdef(con.oid) \
             FROM pg_constraint con \
             JOIN pg_class cl ON cl.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = cl.relnamespace \
             WHERE {class_filter} AND con.contype = 'c' ORDER BY con.conname"
        );
        let checks_result = self.run_query(&checks_sql).await?;
        let checks = checks_result
            .rows
            .iter()
            .map(|row| CheckDef {
                name: text(row, 0),
                expression: text(row, 1),
            })
            .collect();

        let constraints_sql = format!(
            "SELECT con.conname, pg_get_constraintdef(con.oid) \
             FROM pg_constraint con \
             JOIN pg_class cl ON cl.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = cl.relnamespace \
             WHERE {class_filter} AND con.contype IN ('u', 'x') ORDER BY con.conname"
        );
        let constraints_result = self.run_query(&constraints_sql).await?;
        let constraints = constraints_result
            .rows
            .iter()
            .map(|row| ConstraintDef {
                name: text(row, 0),
                definition: text(row, 1),
            })
            .collect();

        let triggers_sql = format!(
            "SELECT tg.tgname, pg_get_triggerdef(tg.oid) \
             FROM pg_trigger tg \
             JOIN pg_class cl ON cl.oid = tg.tgrelid \
             JOIN pg_namespace n ON n.oid = cl.relnamespace \
             WHERE {class_filter} AND NOT tg.tgisinternal ORDER BY tg.tgname"
        );
        let triggers_result = self.run_query(&triggers_sql).await?;
        let names: Vec<String> = triggers_result
            .rows
            .iter()
            .map(|row| text(row, 0))
            .collect();
        let definitions: Vec<String> = triggers_result
            .rows
            .iter()
            .map(|row| text(row, 1))
            .collect();

        let info_sql = format!(
            "SELECT TRIGGER_NAME, ACTION_TIMING, EVENT_MANIPULATION, ACTION_ORIENTATION \
             FROM information_schema.triggers \
             WHERE EVENT_OBJECT_SCHEMA = {schema_lit} AND EVENT_OBJECT_TABLE = {table_lit} \
             ORDER BY TRIGGER_NAME, EVENT_MANIPULATION"
        );
        let info_result = self.run_query(&info_sql).await.ok();
        let mut triggers = Vec::new();
        for (name, definition) in names.into_iter().zip(definitions) {
            let mut timing = String::new();
            let mut events: Vec<String> = Vec::new();
            let mut orientation = String::new();
            if let Some(info) = &info_result {
                for row in &info.rows {
                    if text(row, 0) == name {
                        timing = text(row, 1);
                        orientation = text(row, 3);
                        let event = text(row, 2);
                        if !events.contains(&event) {
                            events.push(event);
                        }
                    }
                }
            }
            triggers.push(TriggerDef {
                name,
                timing,
                events: events.join(", "),
                orientation,
                definition,
            });
        }

        Ok(TableExtras {
            checks,
            constraints,
            triggers,
            trigger_error: None,
        })
    }
}

/// One Postgres column as the catalog reports it, for
/// [`render_postgres_table`].
pub(crate) struct PostgresColumn {
    pub name: String,
    pub type_name: String,
    pub not_null: bool,
    /// `pg_get_expr` output: a default, or a generated column's expression.
    pub default: Option<String>,
    /// `attidentity`: ``, `a` (always), or `d` (by default).
    pub identity: String,
    /// `attgenerated`: `` or `s` (stored).
    pub generated: String,
    /// A non-default collation, if any.
    pub collation: Option<String>,
}

/// Everything [`render_postgres_table`] needs, already fetched.
pub(crate) struct PostgresTableParts {
    pub schema: String,
    pub name: String,
    pub columns: Vec<PostgresColumn>,
    /// `(name, pg_get_constraintdef output)` for every constraint.
    pub constraints: Vec<(String, String)>,
    /// `pg_get_indexdef` output for every non-constraint index.
    pub indexes: Vec<String>,
    /// `pg_get_triggerdef` output for every non-internal trigger.
    pub triggers: Vec<String>,
    pub table_comment: Option<String>,
    /// `(column, comment)` for commented columns, in column order.
    pub column_comments: Vec<(String, String)>,
}

/// Render a Postgres `CREATE TABLE` from catalog rows.
///
/// The header comment says what the output leaves out — partitioning,
/// inheritance, tablespaces, storage parameters, grants, and user-defined
/// types are referenced but never created by it — so nobody mistakes it
/// for a dump.
pub(crate) fn render_postgres_table(parts: &PostgresTableParts) -> String {
    let qualified = format!(
        "{}.{}",
        quote_identifier(&parts.schema, Engine::Postgres),
        quote_identifier(&parts.name, Engine::Postgres)
    );
    let mut lines = vec![
        "-- Generated by Zippa DB: partitioning, inheritance, tablespaces, \
         storage parameters, grants, and user-defined types are not included."
            .to_string(),
        format!("CREATE TABLE {qualified} ("),
    ];
    let mut members: Vec<String> = parts.columns.iter().map(render_postgres_column).collect();
    for (name, definition) in &parts.constraints {
        members.push(format!(
            "    CONSTRAINT {} {definition}",
            quote_identifier(name, Engine::Postgres)
        ));
    }
    lines.push(members.join(",\n"));
    lines.push(");".to_string());
    for index in &parts.indexes {
        lines.push(format!("{index};"));
    }
    for trigger in &parts.triggers {
        lines.push(format!("{trigger};"));
    }
    if let Some(comment) = &parts.table_comment {
        lines.push(format!(
            "COMMENT ON TABLE {qualified} IS {};",
            quote_literal(comment)
        ));
    }
    for (column, comment) in &parts.column_comments {
        lines.push(format!(
            "COMMENT ON COLUMN {qualified}.{} IS {};",
            quote_identifier(column, Engine::Postgres),
            quote_literal(comment)
        ));
    }
    lines.join("\n")
}

fn render_postgres_column(column: &PostgresColumn) -> String {
    let mut rendered = format!(
        "    {} {}",
        quote_identifier(&column.name, Engine::Postgres),
        column.type_name
    );
    if let Some(collation) = &column.collation {
        rendered.push_str(&format!(
            " COLLATE {}",
            quote_identifier(collation, Engine::Postgres)
        ));
    }
    if column.generated == "s" {
        // A generated column's expression arrives as its `pg_get_expr`
        // default; it is not a default value.
        if let Some(expression) = &column.default {
            rendered.push_str(&format!(" GENERATED ALWAYS AS ({expression}) STORED"));
        }
    } else {
        match column.identity.as_str() {
            "a" => rendered.push_str(" GENERATED ALWAYS AS IDENTITY"),
            "d" => rendered.push_str(" GENERATED BY DEFAULT AS IDENTITY"),
            _ => {
                if let Some(default) = &column.default {
                    rendered.push_str(&format!(" DEFAULT {default}"));
                }
            }
        }
    }
    if column.not_null {
        rendered.push_str(" NOT NULL");
    }
    rendered
}

/// Split a SQLite `CREATE TABLE`'s column-definition list on top-level
/// commas, respecting quotes, brackets, and nested parentheses.
fn split_sqlite_definitions(body: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth: usize = 0;
    let mut quote: Option<char> = None;
    let mut bracket = false;
    let mut start = 0;
    let chars: Vec<(usize, char)> = body.char_indices().collect();
    let mut index = 0;
    while index < chars.len() {
        let (byte, ch) = chars[index];
        if let Some(open) = quote {
            // A doubled quote inside a string is an escaped quote, not the
            // end of it.
            if ch == open && !(index + 1 < chars.len() && chars[index + 1].1 == open) {
                quote = None;
            } else if ch == open {
                index += 1;
            }
        } else if bracket {
            if ch == ']' {
                bracket = false;
            }
        } else {
            match ch {
                '\'' | '"' | '`' => quote = Some(ch),
                '[' => bracket = true,
                '(' => depth += 1,
                ')' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    parts.push(body[start..byte].trim());
                    start = byte + 1;
                }
                _ => {}
            }
        }
        index += 1;
    }
    parts.push(body[start..].trim());
    parts.into_iter().filter(|part| !part.is_empty()).collect()
}

/// The balanced `(...)` starting at `open`, or `None` when unbalanced.
fn balanced_parens(text: &str, open: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut depth = 0;
    let mut quote: Option<u8> = None;
    let mut index = open;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(open_quote) = quote {
            if byte == open_quote {
                quote = None;
            }
        } else {
            match byte {
                b'\'' | b'"' | b'`' => quote = Some(byte),
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(&text[open..=index]);
                    }
                }
                _ => {}
            }
        }
        index += 1;
    }
    None
}

/// A word boundary check: `word` at `at` must not run into an identifier
/// character on either side.
fn word_at(text: &str, at: usize, word: &str) -> bool {
    let before = at
        .checked_sub(1)
        .and_then(|index| text.as_bytes().get(index))
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
    let after = text
        .as_bytes()
        .get(at + word.len())
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
    !before && !after
}

/// Find `CHECK (...)` in one column-definition part, case-insensitively.
fn find_sqlite_check(part: &str) -> Option<String> {
    let upper = part.to_ascii_uppercase();
    let mut offset = 0;
    while let Some(found) = upper[offset..].find("CHECK") {
        let at = offset + found;
        if word_at(part, at, "CHECK") {
            let rest = part[at + "CHECK".len()..].trim_start();
            let open = part.len() - rest.len();
            if rest.starts_with('(')
                && let Some(parens) = balanced_parens(part, open)
            {
                return Some(format!("CHECK {parens}"));
            }
        }
        offset = at + "CHECK".len();
    }
    None
}

/// The first identifier of a definition part, unquoted: a column name, or a
/// table-constraint keyword.
fn first_identifier(part: &str) -> String {
    let trimmed = part.trim_start();
    if let Some(rest) = trimmed.strip_prefix('"') {
        return rest.split('"').next().unwrap_or_default().to_string();
    }
    if let Some(rest) = trimmed.strip_prefix('`') {
        return rest.split('`').next().unwrap_or_default().to_string();
    }
    if let Some(rest) = trimmed.strip_prefix('[') {
        return rest.split(']').next().unwrap_or_default().to_string();
    }
    trimmed
        .split(|ch: char| ch.is_whitespace() || ch == '(')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// `CHECK` constraints out of a SQLite `CREATE TABLE` text: SQLite reports
/// them nowhere else. A named table constraint keeps its name; a column
/// check takes the column's name; an unnamed table check has no name.
fn parse_sqlite_checks(table_sql: &str) -> Vec<CheckDef> {
    let Some(open) = table_sql.find('(') else {
        return Vec::new();
    };
    let Some(body) = balanced_parens(table_sql, open) else {
        return Vec::new();
    };
    // Drop the outer parentheses.
    let inner = &body[1..body.len() - 1];
    split_sqlite_definitions(inner)
        .into_iter()
        .filter_map(|part| {
            let expression = find_sqlite_check(part)?;
            let upper = part.to_ascii_uppercase();
            let name = if let Some(at) = find_word(&upper, "CONSTRAINT") {
                // A column check names its constraint mid-definition:
                // `score REAL CONSTRAINT score_positive CHECK (...)`.
                let rest = part[at + "CONSTRAINT".len()..].trim_start();
                first_identifier(rest)
            } else if upper.trim_start().starts_with("CHECK") {
                String::new()
            } else {
                first_identifier(part)
            };
            Some(CheckDef { name, expression })
        })
        .collect()
}

/// The byte offset of `word` in `upper` (already uppercased), at a word
/// boundary, or `None`.
fn find_word(upper: &str, word: &str) -> Option<usize> {
    let mut offset = 0;
    while let Some(found) = upper[offset..].find(word) {
        let at = offset + found;
        if word_at(upper, at, word) {
            return Some(at);
        }
        offset = at + word.len();
    }
    None
}

/// Timing, events, and orientation out of a SQLite `CREATE TRIGGER` text,
/// whose catalog row holds nothing but the text itself.
fn parse_sqlite_trigger(name: &str, sql: &str) -> TriggerDef {
    // Walk the head's tokens: CREATE TRIGGER [IF NOT EXISTS] name
    // timing event [OF columns] ON table ...
    let tokens = tokenize_trigger_head(sql);
    let mut timing = String::new();
    let mut events = String::new();
    let mut index = 0;
    // Tokens: CREATE TRIGGER [IF NOT EXISTS] name ... — keywords match
    // case-insensitively, but column names keep their own case.
    let is = |token: Option<&String>, word: &str| {
        token.is_some_and(|token| token.eq_ignore_ascii_case(word))
    };
    if is(tokens.get(index), "CREATE") {
        index += 1;
    }
    if is(tokens.get(index), "TRIGGER") {
        index += 1;
    }
    if is(tokens.get(index), "IF") {
        index += 3; // IF NOT EXISTS
    }
    index += 1; // the trigger name
    if let Some(token) = tokens.get(index) {
        if token.eq_ignore_ascii_case("INSTEAD") {
            timing = "INSTEAD OF".to_string();
            index += 2;
        } else {
            timing = token.to_ascii_uppercase();
            index += 1;
        }
    }
    if let Some(token) = tokens.get(index) {
        events = token.to_ascii_uppercase();
        index += 1;
        if events == "UPDATE" && is(tokens.get(index), "OF") {
            // `UPDATE OF a, b` — the column list runs until `ON`.
            let mut columns = Vec::new();
            index += 1;
            while let Some(token) = tokens.get(index) {
                if token.eq_ignore_ascii_case("ON") {
                    break;
                }
                if token != "," {
                    columns.push(token.trim_matches(['"', '`', '[', ']']).to_string());
                }
                index += 1;
            }
            events = format!("UPDATE OF {}", columns.join(", "));
        }
    }
    TriggerDef {
        name: name.to_string(),
        timing,
        events,
        orientation: "ROW".to_string(),
        definition: sql.to_string(),
    }
}

/// Words of a `CREATE TRIGGER` head, with quoted names kept whole and commas
/// as their own tokens, in their original case.
fn tokenize_trigger_head(sql: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for ch in sql.chars() {
        if let Some(open) = quote {
            current.push(ch);
            if ch == open {
                quote = None;
            }
        } else {
            match ch {
                '"' | '`' | '[' => {
                    quote = Some(if ch == '[' { ']' } else { ch });
                    current.push(ch);
                }
                ',' => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                    tokens.push(",".to_string());
                }
                ch if ch.is_whitespace() => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(ch),
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(
        name: &str,
        type_name: &str,
        not_null: bool,
        default: Option<&str>,
        identity: &str,
        generated: &str,
        collation: Option<&str>,
    ) -> PostgresColumn {
        PostgresColumn {
            name: name.to_string(),
            type_name: type_name.to_string(),
            not_null,
            default: default.map(str::to_string),
            identity: identity.to_string(),
            generated: generated.to_string(),
            collation: collation.map(str::to_string),
        }
    }

    #[test]
    fn postgres_render_covers_identity_generated_defaults_and_comments() {
        let parts = PostgresTableParts {
            schema: "public".to_string(),
            name: "orders".to_string(),
            columns: vec![
                column("id", "integer", true, None, "d", "", None),
                column(
                    "status",
                    "character varying(20)",
                    false,
                    Some("'new'::character varying"),
                    "",
                    "",
                    None,
                ),
                column(
                    "total",
                    "numeric(10,2)",
                    true,
                    None,
                    "",
                    "s",
                    None,
                ),
                column("code", "text", false, None, "", "", Some("C")),
            ],
            constraints: vec![
                (
                    "orders_pkey".to_string(),
                    "PRIMARY KEY (id)".to_string(),
                ),
                (
                    "orders_total_check".to_string(),
                    "CHECK ((total > (0)::numeric))".to_string(),
                ),
                (
                    "orders_customer_fkey".to_string(),
                    "FOREIGN KEY (customer_id) REFERENCES customers(id)".to_string(),
                ),
            ],
            indexes: vec![
                "CREATE UNIQUE INDEX orders_code_key ON public.orders USING btree (code) WHERE code IS NOT NULL"
                    .to_string(),
            ],
            triggers: vec![
                "CREATE TRIGGER orders_audit AFTER INSERT ON public.orders FOR EACH ROW EXECUTE FUNCTION audit()"
                    .to_string(),
            ],
            table_comment: Some("A customer's order".to_string()),
            column_comments: vec![("status".to_string(), "lifecycle state".to_string())],
        };
        // The generated column's expression arrives as its default slot.
        let mut parts = parts;
        parts.columns[2].default = Some("price * qty".to_string());

        let rendered = render_postgres_table(&parts);
        assert_eq!(
            rendered,
            "-- Generated by Zippa DB: partitioning, inheritance, tablespaces, \
             storage parameters, grants, and user-defined types are not included.\n\
             CREATE TABLE public.orders (\n\
             \x20   id integer GENERATED BY DEFAULT AS IDENTITY NOT NULL,\n\
             \x20   status character varying(20) DEFAULT 'new'::character varying,\n\
             \x20   total numeric(10,2) GENERATED ALWAYS AS (price * qty) STORED NOT NULL,\n\
             \x20   code text COLLATE \"C\",\n\
             \x20   CONSTRAINT orders_pkey PRIMARY KEY (id),\n\
             \x20   CONSTRAINT orders_total_check CHECK ((total > (0)::numeric)),\n\
             \x20   CONSTRAINT orders_customer_fkey FOREIGN KEY (customer_id) REFERENCES customers(id)\n\
             );\n\
             CREATE UNIQUE INDEX orders_code_key ON public.orders USING btree (code) WHERE code IS NOT NULL;\n\
             CREATE TRIGGER orders_audit AFTER INSERT ON public.orders FOR EACH ROW EXECUTE FUNCTION audit();\n\
             COMMENT ON TABLE public.orders IS 'A customer''s order';\n\
             COMMENT ON COLUMN public.orders.status IS 'lifecycle state';"
        );
    }

    #[test]
    fn sqlite_check_parsing_finds_named_column_and_table_checks() {
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, \
             score REAL CONSTRAINT score_positive CHECK (score > 0), \
             name TEXT CHECK (length(name) > 0), \
             CHECK (id <> 0))";
        let checks = parse_sqlite_checks(sql);
        assert_eq!(
            checks,
            vec![
                CheckDef {
                    name: "score_positive".to_string(),
                    expression: "CHECK (score > 0)".to_string(),
                },
                CheckDef {
                    name: "name".to_string(),
                    expression: "CHECK (length(name) > 0)".to_string(),
                },
                CheckDef {
                    name: String::new(),
                    expression: "CHECK (id <> 0)".to_string(),
                },
            ]
        );
    }

    #[test]
    fn sqlite_trigger_parsing_reads_timing_and_update_columns() {
        let trigger = parse_sqlite_trigger(
            "t_guard",
            "CREATE TRIGGER t_guard BEFORE UPDATE OF score ON t BEGIN SELECT 1; END",
        );
        assert_eq!(trigger.timing, "BEFORE");
        assert_eq!(trigger.events, "UPDATE OF score");
        assert_eq!(trigger.orientation, "ROW");
    }
}
