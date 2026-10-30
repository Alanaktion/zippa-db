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
                "table" => {
                    extras.checks = parse_sqlite_checks(&text(row, 2));
                    extras.constraints = parse_sqlite_unique_constraints(&text(row, 2));
                }
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
        // take the table's own DDL down with it; the note below, and the
        // extras' error, say the triggers are missing instead of letting
        // the copy read as complete.
        let mut note = None;
        match self.mysql_trigger_rows(object).await {
            Ok(triggers) => {
                if triggers.is_empty() && self.has_trigger_privilege().await == Some(false) {
                    note = Some(
                        "-- Note: triggers could not be confirmed; the account may lack the \
                         TRIGGER privilege, so any triggers on this table are omitted."
                            .to_string(),
                    );
                }
                statements.extend(triggers.into_iter().map(|trigger| trigger.definition));
            }
            Err(error) => {
                note = Some(format!(
                    "-- Note: triggers could not be read ({error:#}); the DDL above omits them."
                ));
            }
        }
        let mut joined = join_statements(statements);
        if let Some(note) = note {
            joined.push_str(&format!("\n{note}"));
        }
        Ok(joined)
    }

    /// One row per trigger on `object`, reconstructed from
    /// `information_schema` so no column position of `SHOW CREATE TRIGGER`
    /// is assumed. The caller decides what a failure means.
    async fn mysql_trigger_rows(&self, object: &DatabaseObject) -> Result<Vec<TriggerDef>> {
        let sql = format!(
            "SELECT TRIGGER_NAME, ACTION_TIMING, EVENT_MANIPULATION, ACTION_ORIENTATION, ACTION_STATEMENT, DEFINER \
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
                // A replayed trigger without its definer runs as whoever
                // runs the replay, not as its original owner.
                let definer = parse_definer(&text(row, 5))
                    .map(|(user, host)| {
                        format!(
                            " DEFINER={}@{}",
                            quote_literal_for(Engine::MySql, &user),
                            quote_literal_for(Engine::MySql, &host)
                        )
                    })
                    .unwrap_or_default();
                TriggerDef {
                    definition: format!(
                        "CREATE{definer} TRIGGER {} {} {} ON {} FOR EACH {} {}",
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

    /// Whether the account holds the `TRIGGER` privilege, from `SHOW
    /// GRANTS`: `None` when the grants could not be read, so a failed probe
    /// never reports triggers as hidden. Only asked when a trigger read
    /// came back empty — MySQL shows an account only the triggers it can
    /// see, so an empty success otherwise reads as "no triggers".
    async fn has_trigger_privilege(&self) -> Option<bool> {
        let result = self.run_query("SHOW GRANTS").await.ok()?;
        Some(result.rows.iter().any(|row| {
            grant_privileges(&text(row, 0)).iter().any(|privilege| {
                privilege == "TRIGGER" || privilege == "ALL PRIVILEGES" || privilege == "ALL"
            })
        }))
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
               AND kcu.TABLE_NAME = tc.TABLE_NAME \
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
            Ok(triggers) => {
                let error =
                    if triggers.is_empty() && self.has_trigger_privilege().await == Some(false) {
                        Some(
                            "triggers could not be confirmed: the account may lack the TRIGGER \
                         privilege"
                                .to_string(),
                        )
                    } else {
                        None
                    };
                (triggers, error)
            }
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
               THEN c.collname ELSE NULL END, \
             CASE WHEN a.attcollation <> 0 AND a.attcollation <> t.typcollation \
               THEN cn.nspname ELSE NULL END \
             FROM pg_attribute a \
             JOIN pg_type t ON t.oid = a.atttypid \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             LEFT JOIN pg_collation c ON c.oid = a.attcollation \
             LEFT JOIN pg_namespace cn ON cn.oid = c.collnamespace \
             JOIN pg_class cl ON cl.oid = a.attrelid \
             JOIN pg_namespace n ON n.oid = cl.relnamespace \
             WHERE {class_filter} AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum"
        );
        let columns_result = self.run_query(&columns_sql).await?;
        let columns: Vec<PostgresColumn> = columns_result
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
                collation_schema: text_opt(row, 7),
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
             JOIN pg_class cl ON cl.oid = ix.indrelid \
             JOIN pg_class i ON i.oid = ix.indexrelid \
             JOIN pg_namespace n ON n.oid = cl.relnamespace \
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

        // Sequences a serial column's default draws on, via the ownership
        // link in `pg_depend` — never by parsing the `nextval(...)` text.
        // An identity column's sequence is left out: `CREATE TABLE` makes
        // it implicitly, so emitting one would collide with it.
        let sequences_sql = format!(
            "SELECT sn.nspname, sc.relname, format_type(s.seqtypid, NULL), a.attname \
             FROM pg_depend d \
             JOIN pg_class sc ON sc.oid = d.objid \
             JOIN pg_namespace sn ON sn.oid = sc.relnamespace \
             JOIN pg_sequence s ON s.seqrelid = sc.oid \
             JOIN pg_class tc ON tc.oid = d.refobjid \
             JOIN pg_namespace tn ON tn.oid = tc.relnamespace \
             JOIN pg_attribute a ON a.attrelid = tc.oid AND a.attnum = d.refobjsubid \
             WHERE d.classid = 'pg_class'::regclass AND d.refclassid = 'pg_class'::regclass \
               AND d.deptype = 'a' AND tn.nspname = {schema_lit} AND tc.relname = {table_lit}"
        );
        let sequences_result = self.run_query(&sequences_sql).await?;
        let sequences = sequences_result
            .rows
            .iter()
            .map(|row| PostgresSequence {
                schema: text(row, 0),
                name: text(row, 1),
                data_type: text(row, 2),
                owned_column: text(row, 3),
            })
            .filter(|sequence| {
                !columns.iter().any(|column| {
                    column.name == sequence.owned_column && !column.identity.is_empty()
                })
            })
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
            sequences,
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

/// `(user, host)` out of an `information_schema.triggers` DEFINER value
/// (`'user'@'host'`), or `None` when it does not parse — the trigger is
/// then rebuilt without a DEFINER, as before, rather than with a broken one.
fn parse_definer(definer: &str) -> Option<(String, String)> {
    let (user, rest) = split_definer_part(definer)?;
    let rest = rest.strip_prefix('@')?;
    let (host, tail) = split_definer_part(rest)?;
    if !tail.is_empty() {
        return None;
    }
    Some((user, host))
}

/// One DEFINER part: a `'...'` string with `''` as an escaped quote, or a
/// bare word up to `@`. Returns the unquoted value and what follows it.
fn split_definer_part(text: &str) -> Option<(String, &str)> {
    if let Some(quoted) = text.strip_prefix('\'') {
        let mut value = String::new();
        let mut chars = quoted.char_indices().peekable();
        while let Some((index, ch)) = chars.next() {
            if ch == '\'' {
                if chars.peek().is_some_and(|(_, next)| *next == '\'') {
                    chars.next();
                    value.push('\'');
                } else {
                    return Some((value, &quoted[index + 1..]));
                }
            } else {
                value.push(ch);
            }
        }
        None
    } else {
        let end = text.find('@').unwrap_or(text.len());
        Some((text[..end].to_string(), &text[end..]))
    }
}

/// The privilege list of one `SHOW GRANTS` line, uppercased, or empty when
/// the line grants a role rather than privileges (`GRANT 'role' TO ...`
/// has no `ON` clause to split on).
fn grant_privileges(grant: &str) -> Vec<String> {
    let upper = grant.to_ascii_uppercase();
    let Some(body) = upper.strip_prefix("GRANT ") else {
        return Vec::new();
    };
    let Some(on) = body.find(" ON ") else {
        return Vec::new();
    };
    body[..on]
        .split(',')
        .map(|privilege| privilege.trim().to_string())
        .filter(|privilege| !privilege.is_empty())
        .collect()
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
    /// `attgenerated`: ``, `s` (stored), or `v` (virtual, PostgreSQL 18).
    pub generated: String,
    /// A non-default collation, if any.
    pub collation: Option<String>,
    /// That collation's schema, so the rendered `COLLATE` does not depend
    /// on the replaying session's search path.
    pub collation_schema: Option<String>,
}

/// Everything [`render_postgres_table`] needs, already fetched.
pub(crate) struct PostgresTableParts {
    pub schema: String,
    pub name: String,
    pub columns: Vec<PostgresColumn>,
    /// `(name, pg_get_constraintdef output)` for every constraint.
    pub constraints: Vec<(String, String)>,
    /// Sequences a serial column's default draws on, owned by a column of
    /// this table. Identity columns' sequences are not here — `CREATE
    /// TABLE` makes those implicitly.
    pub sequences: Vec<PostgresSequence>,
    /// `pg_get_indexdef` output for every non-constraint index.
    pub indexes: Vec<String>,
    /// `pg_get_triggerdef` output for every non-internal trigger.
    pub triggers: Vec<String>,
    pub table_comment: Option<String>,
    /// `(column, comment)` for commented columns, in column order.
    pub column_comments: Vec<(String, String)>,
}

/// One sequence a serial column's default draws on, for
/// [`render_postgres_table`].
pub(crate) struct PostgresSequence {
    pub schema: String,
    pub name: String,
    /// `format_type` output, e.g. `bigint`.
    pub data_type: String,
    /// The column of this table that owns the sequence.
    pub owned_column: String,
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
    ];
    // A serial default's `nextval(...)` fails on a database without its
    // sequence, and re-running after a `DROP TABLE` fails because the drop
    // took the sequence with it — so the sequences come first.
    for sequence in &parts.sequences {
        lines.push(format!(
            "CREATE SEQUENCE {}.{} AS {};",
            quote_identifier(&sequence.schema, Engine::Postgres),
            quote_identifier(&sequence.name, Engine::Postgres),
            sequence.data_type
        ));
    }
    lines.push(format!("CREATE TABLE {qualified} ("));
    let mut members: Vec<String> = parts.columns.iter().map(render_postgres_column).collect();
    for (name, definition) in &parts.constraints {
        members.push(format!(
            "    CONSTRAINT {} {definition}",
            quote_identifier(name, Engine::Postgres)
        ));
    }
    lines.push(members.join(",\n"));
    lines.push(");".to_string());
    // Ownership waits until the table exists: `CREATE SEQUENCE ...
    // OWNED BY` needs its table, so the link is made here instead.
    for sequence in &parts.sequences {
        lines.push(format!(
            "ALTER SEQUENCE {}.{} OWNED BY {qualified}.{};",
            quote_identifier(&sequence.schema, Engine::Postgres),
            quote_identifier(&sequence.name, Engine::Postgres),
            quote_identifier(&sequence.owned_column, Engine::Postgres)
        ));
    }
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
        match &column.collation_schema {
            Some(schema) => rendered.push_str(&format!(
                " COLLATE {}.{}",
                quote_identifier(schema, Engine::Postgres),
                quote_identifier(collation, Engine::Postgres)
            )),
            None => rendered.push_str(&format!(
                " COLLATE {}",
                quote_identifier(collation, Engine::Postgres)
            )),
        }
    }
    if !column.generated.is_empty() {
        // A generated column's expression arrives as its `pg_get_expr`
        // default; it is not a default value. A virtual column (PostgreSQL
        // 18's default mode) renders without a mode, which the server reads
        // as virtual; rendering it as a `DEFAULT` would fail when the
        // expression references other columns.
        if let Some(expression) = &column.default {
            let mode = if column.generated == "s" {
                " STORED"
            } else {
                ""
            };
            rendered.push_str(&format!(" GENERATED ALWAYS AS ({expression}){mode}"));
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
/// commas, respecting quotes, brackets, and nested parentheses. Comments
/// hold no separators — a comma inside one never splits — and are dropped,
/// so a part that was only a comment comes out empty.
fn split_sqlite_definitions(body: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut depth: usize = 0;
    let mut quote: Option<char> = None;
    let mut bracket = false;
    let chars: Vec<char> = body.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if let Some(open) = quote {
            current.push(ch);
            // A doubled quote inside a string is an escaped quote, not the
            // end of it.
            if ch == open {
                if index + 1 < chars.len() && chars[index + 1] == open {
                    index += 1;
                    current.push(chars[index]);
                } else {
                    quote = None;
                }
            }
        } else if bracket {
            current.push(ch);
            if ch == ']' {
                bracket = false;
            }
        } else if ch == '-' && index + 1 < chars.len() && chars[index + 1] == '-' {
            // A `--` comment runs to the newline; the loop's increment
            // skips the newline itself.
            while index < chars.len() && chars[index] != '\n' {
                index += 1;
            }
        } else if ch == '/' && index + 1 < chars.len() && chars[index + 1] == '*' {
            // A `/* ... */` comment runs to its close; an unterminated one
            // runs to the end of the list.
            index += 2;
            while index + 1 < chars.len() && !(chars[index] == '*' && chars[index + 1] == '/') {
                index += 1;
            }
            if index + 1 < chars.len() {
                index += 1;
            }
        } else {
            match ch {
                '\'' | '"' | '`' => {
                    quote = Some(ch);
                    current.push(ch);
                }
                '[' => {
                    bracket = true;
                    current.push(ch);
                }
                '(' => {
                    depth += 1;
                    current.push(ch);
                }
                ')' => {
                    depth = depth.saturating_sub(1);
                    current.push(ch);
                }
                ',' if depth == 0 => {
                    let part = current.trim().to_string();
                    if !part.is_empty() {
                        parts.push(part);
                    }
                    current.clear();
                }
                _ => current.push(ch),
            }
        }
        index += 1;
    }
    let part = current.trim().to_string();
    if !part.is_empty() {
        parts.push(part);
    }
    parts
}

/// The balanced `(...)` starting at `open`, or `None` when unbalanced.
/// Parentheses inside quotes, brackets, and comments do not count.
fn balanced_parens(text: &str, open: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut depth = 0;
    let mut quote: Option<u8> = None;
    let mut bracket = false;
    let mut index = open;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(open_quote) = quote {
            if byte == open_quote {
                // A doubled quote inside a string is an escaped quote, not
                // the end of it.
                if index + 1 < bytes.len() && bytes[index + 1] == open_quote {
                    index += 1;
                } else {
                    quote = None;
                }
            }
        } else if bracket {
            if byte == b']' {
                bracket = false;
            }
        } else if byte == b'-' && bytes.get(index + 1) == Some(&b'-') {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            if index + 1 < bytes.len() {
                index += 1;
            }
        } else {
            match byte {
                b'\'' | b'"' | b'`' => quote = Some(byte),
                b'[' => bracket = true,
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

/// Every `CHECK (...)` in one column-definition part, each with the byte
/// offset of its `CHECK` keyword, case-insensitively. A `CHECK` inside a
/// string, a quoted identifier, or a comment never matches — a column can
/// hold a default like `'CHECK (x)'` alongside a real constraint — and a
/// column with two checks keeps both.
fn find_sqlite_checks(part: &str) -> Vec<(usize, String)> {
    let bytes = part.as_bytes();
    let mut found = Vec::new();
    let mut quote: Option<u8> = None;
    let mut bracket = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(open_quote) = quote {
            if byte == open_quote {
                // A doubled quote inside a string is an escaped quote, not
                // the end of it.
                if index + 1 < bytes.len() && bytes[index + 1] == open_quote {
                    index += 1;
                } else {
                    quote = None;
                }
            }
        } else if bracket {
            if byte == b']' {
                bracket = false;
            }
        } else if byte == b'-' && bytes.get(index + 1) == Some(&b'-') {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            if index + 1 < bytes.len() {
                index += 1;
            }
        } else if byte == b'[' {
            bracket = true;
        } else if byte == b'\'' || byte == b'"' || byte == b'`' {
            quote = Some(byte);
        } else if byte.eq_ignore_ascii_case(&b'C')
            && index + "CHECK".len() <= bytes.len()
            && part[index..index + "CHECK".len()].eq_ignore_ascii_case("CHECK")
            && word_at(part, index, "CHECK")
        {
            let rest = part[index + "CHECK".len()..].trim_start();
            let open = part.len() - rest.len();
            if rest.starts_with('(')
                && let Some(parens) = balanced_parens(part, open)
            {
                found.push((index, format!("CHECK {parens}")));
                // Skip past the expression: nothing inside it is another
                // top-level check. The loop's increment lands just past the
                // closing parenthesis.
                index = open + parens.len() - 1;
            } else {
                // `CHECK` as a name rather than a constraint; move past it.
                index += "CHECK".len() - 1;
            }
        }
        index += 1;
    }
    found
}

/// The byte offsets of `word` in `part` at parenthesis depth zero, outside
/// quotes, brackets, and comments — for finding a constraint keyword a
/// column definition actually carries, rather than one its default or
/// check expression merely mentions.
fn top_level_word_positions(part: &str, word: &str) -> Vec<usize> {
    let bytes = part.as_bytes();
    let mut found = Vec::new();
    let mut depth: usize = 0;
    let mut quote: Option<u8> = None;
    let mut bracket = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(open_quote) = quote {
            if byte == open_quote {
                if index + 1 < bytes.len() && bytes[index + 1] == open_quote {
                    index += 1;
                } else {
                    quote = None;
                }
            }
        } else if bracket {
            if byte == b']' {
                bracket = false;
            }
        } else if byte == b'-' && bytes.get(index + 1) == Some(&b'-') {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            if index + 1 < bytes.len() {
                index += 1;
            }
        } else if byte == b'[' {
            bracket = true;
        } else if byte == b'\'' || byte == b'"' || byte == b'`' {
            quote = Some(byte);
        } else if byte == b'(' {
            depth += 1;
        } else if byte == b')' {
            depth = depth.saturating_sub(1);
        } else if depth == 0
            && index + word.len() <= bytes.len()
            && part[index..index + word.len()].eq_ignore_ascii_case(word)
            && word_at(part, index, word)
        {
            found.push(index);
            index += word.len() - 1;
        }
        index += 1;
    }
    found
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

/// The byte offset of the `(` that opens a `CREATE TABLE`'s definition
/// list: the first one outside quotes, brackets, and comments, so a `(` in
/// a quoted table name does not start the list.
fn definition_list_open(table_sql: &str) -> Option<usize> {
    let bytes = table_sql.as_bytes();
    let mut quote: Option<u8> = None;
    let mut bracket = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(open_quote) = quote {
            if byte == open_quote {
                if index + 1 < bytes.len() && bytes[index + 1] == open_quote {
                    index += 1;
                } else {
                    quote = None;
                }
            }
        } else if bracket {
            if byte == b']' {
                bracket = false;
            }
        } else if byte == b'-' && bytes.get(index + 1) == Some(&b'-') {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/') {
                index += 1;
            }
            if index + 1 < bytes.len() {
                index += 1;
            }
        } else if byte == b'\'' || byte == b'"' || byte == b'`' {
            quote = Some(byte);
        } else if byte == b'[' {
            bracket = true;
        } else if byte == b'(' {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// `CHECK` constraints out of a SQLite `CREATE TABLE` text: SQLite reports
/// them nowhere else. A named table constraint keeps its name; a column
/// check takes the column's name; an unnamed table check has no name. A
/// column with two checks keeps both.
fn parse_sqlite_checks(table_sql: &str) -> Vec<CheckDef> {
    let Some(open) = definition_list_open(table_sql) else {
        return Vec::new();
    };
    let Some(body) = balanced_parens(table_sql, open) else {
        return Vec::new();
    };
    // Drop the outer parentheses.
    let inner = &body[1..body.len() - 1];
    let mut checks = Vec::new();
    for part in split_sqlite_definitions(inner) {
        let mut earlier: Vec<usize> = Vec::new();
        for (at, expression) in find_sqlite_checks(&part) {
            checks.push(CheckDef {
                name: check_name(&part, at, &earlier),
                expression,
            });
            earlier.push(at);
        }
    }
    checks
}

/// The name of the check at `check_at` in `part`: the `CONSTRAINT` name
/// when one names this check, else the column's name — or nothing for a
/// table-level check. A `CONSTRAINT` names only the next check after it, so
/// a second check in the same definition falls back to the column.
fn check_name(part: &str, check_at: usize, earlier_checks: &[usize]) -> String {
    let upper = part.to_ascii_uppercase();
    let mut constraint_at = None;
    let mut offset = 0;
    while let Some(found) = find_word(&upper[offset..], "CONSTRAINT") {
        let at = offset + found;
        if at >= check_at {
            break;
        }
        constraint_at = Some(at);
        offset = at + "CONSTRAINT".len();
    }
    if let Some(at) = constraint_at
        && !earlier_checks.iter().any(|earlier| *earlier > at)
    {
        // A column check names its constraint mid-definition:
        // `score REAL CONSTRAINT score_positive CHECK (...)`.
        let rest = part[at + "CONSTRAINT".len()..].trim_start();
        return first_identifier(rest);
    }
    if upper.trim_start().starts_with("CHECK") {
        String::new()
    } else {
        first_identifier(part)
    }
}

/// `UNIQUE` constraints out of a SQLite `CREATE TABLE` text: a bare table
/// `UNIQUE (email)`, a named `CONSTRAINT u UNIQUE (email)`, and a column's
/// own `email TEXT UNIQUE` all read as `UNIQUE (email)` — the shape the
/// other engines report — so the Constraints section agrees across engines.
fn parse_sqlite_unique_constraints(table_sql: &str) -> Vec<ConstraintDef> {
    let Some(open) = definition_list_open(table_sql) else {
        return Vec::new();
    };
    let Some(body) = balanced_parens(table_sql, open) else {
        return Vec::new();
    };
    // Drop the outer parentheses.
    let inner = &body[1..body.len() - 1];
    let mut constraints = Vec::new();
    for part in split_sqlite_definitions(inner) {
        let positions = top_level_word_positions(&part, "UNIQUE");
        let Some(&at) = positions.last() else {
            continue;
        };
        let trimmed = part.trim_start();
        let first_start = part.len() - trimmed.len();
        let upper = trimmed.to_ascii_uppercase();
        if let Some(after) = upper.strip_prefix("UNIQUE") {
            // A bare table constraint, when what follows is its column list;
            // a column merely named `unique` falls through to the column
            // branch below, where the leading word is its name, not a
            // constraint.
            let after = after.trim_start();
            if after.starts_with('(') {
                constraints.push(ConstraintDef {
                    name: String::new(),
                    definition: part[at..].trim().to_string(),
                });
                continue;
            }
            if at == first_start {
                continue;
            }
        } else if upper.starts_with("CONSTRAINT") {
            let after = trimmed["CONSTRAINT".len()..].trim_start();
            constraints.push(ConstraintDef {
                name: first_identifier(after),
                definition: part[at..].trim().to_string(),
            });
            continue;
        }
        if at == first_start {
            // The only `UNIQUE` is the column's own name.
            continue;
        }
        let column = first_identifier(&part);
        constraints.push(ConstraintDef {
            name: column.clone(),
            definition: format!("UNIQUE ({column})"),
        });
    }
    constraints
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
            collation_schema: None,
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
            sequences: vec![],
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

    #[test]
    fn sqlite_check_parsing_skips_comments_and_quoted_names() {
        // A comma inside a comment must not split the definition, and a
        // `(` in a quoted table name must not open the definition list.
        let sql = "CREATE TABLE \"my(table\" (x INT /* first, second */ CHECK (x > 0), \
             y TEXT -- a note, with a comma\n CHECK (length(y) > 0))";
        let checks = parse_sqlite_checks(sql);
        assert_eq!(
            checks,
            vec![
                CheckDef {
                    name: "x".to_string(),
                    expression: "CHECK (x > 0)".to_string(),
                },
                CheckDef {
                    name: "y".to_string(),
                    expression: "CHECK (length(y) > 0)".to_string(),
                },
            ]
        );
    }

    #[test]
    fn sqlite_check_parsing_keeps_every_check_on_one_column() {
        // Two checks on one column, the second unnamed: the first keeps a
        // `CONSTRAINT` name, the second falls back to the column.
        let sql = "CREATE TABLE t (v INT CONSTRAINT v_range CHECK (v > 0) CHECK (v < 10))";
        let checks = parse_sqlite_checks(sql);
        assert_eq!(
            checks,
            vec![
                CheckDef {
                    name: "v_range".to_string(),
                    expression: "CHECK (v > 0)".to_string(),
                },
                CheckDef {
                    name: "v".to_string(),
                    expression: "CHECK (v < 10)".to_string(),
                },
            ]
        );
    }

    #[test]
    fn sqlite_check_parsing_ignores_quoted_check_text() {
        // A `CHECK` inside a default string or a bracket-quoted name is
        // not a constraint.
        let sql = "CREATE TABLE t (x TEXT DEFAULT 'CHECK (x)' CHECK (length(x) > 0), \
             [CHECK (x)] TEXT CHECK (length([CHECK (x)]) > 0))";
        let checks = parse_sqlite_checks(sql);
        assert_eq!(
            checks,
            vec![
                CheckDef {
                    name: "x".to_string(),
                    expression: "CHECK (length(x) > 0)".to_string(),
                },
                CheckDef {
                    name: "CHECK (x)".to_string(),
                    expression: "CHECK (length([CHECK (x)]) > 0)".to_string(),
                },
            ]
        );
    }

    #[test]
    fn sqlite_unique_parsing_covers_table_and_column_constraints() {
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT UNIQUE, \
             name TEXT, UNIQUE (name), CONSTRAINT u_pair UNIQUE (id, email), \
             unique TEXT)";
        let constraints = parse_sqlite_unique_constraints(sql);
        assert_eq!(
            constraints,
            vec![
                ConstraintDef {
                    name: "email".to_string(),
                    definition: "UNIQUE (email)".to_string(),
                },
                ConstraintDef {
                    name: String::new(),
                    definition: "UNIQUE (name)".to_string(),
                },
                ConstraintDef {
                    name: "u_pair".to_string(),
                    definition: "UNIQUE (id, email)".to_string(),
                },
            ]
        );
    }

    #[test]
    fn mysql_definer_parsing_handles_quoted_and_bare_forms() {
        assert_eq!(
            parse_definer("'root'@'localhost'"),
            Some(("root".to_string(), "localhost".to_string()))
        );
        assert_eq!(
            parse_definer("'o''brien'@'%'"),
            Some(("o'brien".to_string(), "%".to_string()))
        );
        assert_eq!(
            parse_definer("root@localhost"),
            Some(("root".to_string(), "localhost".to_string()))
        );
        assert_eq!(parse_definer("'unterminated"), None);
        assert_eq!(parse_definer(""), None);
    }

    #[test]
    fn mysql_grant_parsing_reads_privileges_not_roles() {
        assert_eq!(
            grant_privileges("GRANT SELECT, TRIGGER ON `db`.* TO 'u'@'h'"),
            vec!["SELECT".to_string(), "TRIGGER".to_string()]
        );
        assert_eq!(
            grant_privileges("GRANT ALL PRIVILEGES ON *.* TO 'u'@'h'"),
            vec!["ALL PRIVILEGES".to_string()]
        );
        assert!(grant_privileges("GRANT 'auditor' TO 'u'@'h'").is_empty());
    }

    #[test]
    fn postgres_render_covers_virtual_generated_qualified_collation_and_sequences() {
        let mut name = column("label", "text", false, None, "", "", Some("en_US"));
        name.collation_schema = Some("public".to_string());
        let parts = PostgresTableParts {
            schema: "public".to_string(),
            name: "items".to_string(),
            columns: vec![
                column(
                    "id",
                    "integer",
                    true,
                    Some("nextval('items_id_seq'::regclass)"),
                    "",
                    "",
                    None,
                ),
                name,
                column(
                    "total",
                    "numeric",
                    false,
                    Some("price * qty"),
                    "",
                    "v",
                    None,
                ),
            ],
            constraints: vec![],
            sequences: vec![PostgresSequence {
                schema: "public".to_string(),
                name: "items_id_seq".to_string(),
                data_type: "bigint".to_string(),
                owned_column: "id".to_string(),
            }],
            indexes: vec![],
            triggers: vec![],
            table_comment: None,
            column_comments: vec![],
        };
        let rendered = render_postgres_table(&parts);
        assert_eq!(
            rendered,
            "-- Generated by Zippa DB: partitioning, inheritance, tablespaces, \
             storage parameters, grants, and user-defined types are not included.\n\
             CREATE SEQUENCE public.items_id_seq AS bigint;\n\
             CREATE TABLE public.items (\n\
             \x20   id integer DEFAULT nextval('items_id_seq'::regclass) NOT NULL,\n\
             \x20   label text COLLATE public.\"en_US\",\n\
             \x20   total numeric GENERATED ALWAYS AS (price * qty)\n\
             );\n\
             ALTER SEQUENCE public.items_id_seq OWNED BY public.items.id;"
        );
    }
}
