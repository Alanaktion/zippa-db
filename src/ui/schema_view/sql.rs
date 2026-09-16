//! The `ALTER TABLE` statements `SchemaView` generates for its column edits.
//!
//! Kept beside the view rather than in `db/`, same as `table_view/sql.rs`:
//! this is DDL generation for one screen, not a shared query path.

use crate::db::schema::ColumnDef;
use crate::db::{DatabaseObject, Engine, quote_identifier};

/// One column row as the user has edited it, ready to be diffed against
/// `original` and turned into SQL.
#[derive(Debug, Clone)]
pub(crate) struct ColumnEdit {
    /// `None` for a column added by hand; it has nothing on the server yet.
    pub original: Option<ColumnDef>,
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    /// Raw expression text, bound for `DEFAULT` verbatim — same convention as
    /// `ColumnDef::default`.
    pub default: Option<String>,
    /// Only meaningful when `original` is `Some`: a dropped new column is
    /// simply removed from the row list instead, the way a table view's
    /// draft row is discarded rather than marked.
    pub dropped: bool,
}

impl ColumnEdit {
    pub(crate) fn is_new(&self) -> bool {
        self.original.is_none()
    }

    fn renamed(&self) -> bool {
        self.original.as_ref().is_some_and(|o| o.name != self.name)
    }

    fn retyped(&self) -> bool {
        self.original
            .as_ref()
            .is_some_and(|o| o.type_name != self.type_name)
    }

    fn nullability_changed(&self) -> bool {
        self.original
            .as_ref()
            .is_some_and(|o| o.nullable != self.nullable)
    }

    fn default_changed(&self) -> bool {
        self.original
            .as_ref()
            .is_some_and(|o| o.default != self.default)
    }

    /// Whether an existing, kept column has anything different about it.
    pub(crate) fn changed(&self) -> bool {
        !self.dropped
            && !self.is_new()
            && (self.renamed()
                || self.retyped()
                || self.nullability_changed()
                || self.default_changed())
    }
}

/// `object`, qualified and quoted, the way `TableView::target` reads it.
pub(crate) fn target(object: &DatabaseObject, engine: Engine) -> String {
    match &object.schema {
        Some(schema) => format!(
            "{}.{}",
            quote_identifier(schema, engine),
            quote_identifier(&object.name, engine)
        ),
        None => quote_identifier(&object.name, engine),
    }
}

/// A short, fixed list of common types for the column type dropdown.
///
/// Not exhaustive — every engine has far more — but enough for the common
/// case without standing up a searchable picker for a handful of choices.
pub(crate) fn common_types(engine: Engine) -> &'static [&'static str] {
    match engine {
        Engine::Postgres => &[
            "text",
            "varchar(255)",
            "integer",
            "bigint",
            "smallint",
            "boolean",
            "numeric",
            "real",
            "double precision",
            "date",
            "timestamp",
            "timestamptz",
            "uuid",
            "jsonb",
        ],
        Engine::MySql => &[
            "varchar(255)",
            "text",
            "int",
            "bigint",
            "smallint",
            "boolean",
            "decimal(10,2)",
            "float",
            "double",
            "date",
            "datetime",
            "timestamp",
            "json",
        ],
        // SQLite is dynamically typed; these are its five storage classes.
        Engine::Sqlite => &["TEXT", "INTEGER", "REAL", "NUMERIC", "BLOB"],
    }
}

/// `name type [NOT NULL] [DEFAULT expr]`, the shape an `ADD COLUMN` and every
/// engine's full column redefinition share.
fn column_clause(engine: Engine, edit: &ColumnEdit) -> String {
    let mut clause = format!(
        "{} {}",
        quote_identifier(&edit.name, engine),
        edit.type_name
    );
    if !edit.nullable {
        clause.push_str(" NOT NULL");
    }
    if let Some(default) = &edit.default {
        clause.push_str(" DEFAULT ");
        clause.push_str(default);
    }
    clause
}

/// Every statement `edits` calls for, in order: modify existing columns first
/// (matches `TableView::commit_rows`' update-then-insert-then-delete order),
/// then add the new ones, then drop the ones marked for it.
///
/// `Err` only for a change phase 2 cannot express — SQLite's column rebuild,
/// left to phase 3 — or a changed column left with an empty name.
pub(crate) fn generate_alter_statements(
    engine: Engine,
    target: &str,
    edits: &[ColumnEdit],
) -> Result<Vec<String>, String> {
    let mut statements = Vec::new();

    for edit in edits {
        if !edit.changed() {
            continue;
        }
        if edit.name.trim().is_empty() {
            return Err("a column needs a name".to_string());
        }
        statements.extend(modify_statements(engine, target, edit)?);
    }

    for edit in edits {
        if edit.is_new() && !edit.dropped && !edit.name.trim().is_empty() {
            statements.push(format!(
                "ALTER TABLE {target} ADD COLUMN {}",
                column_clause(engine, edit)
            ));
        }
    }

    for edit in edits {
        if edit.dropped
            && let Some(original) = &edit.original
        {
            statements.push(format!(
                "ALTER TABLE {target} DROP COLUMN {}",
                quote_identifier(&original.name, engine)
            ));
        }
    }

    Ok(statements)
}

fn modify_statements(
    engine: Engine,
    target: &str,
    edit: &ColumnEdit,
) -> Result<Vec<String>, String> {
    // `changed()` already established this.
    let original = edit
        .original
        .as_ref()
        .expect("a changed column has an original");

    match engine {
        Engine::Postgres => Ok(postgres_modify(target, original, edit)),
        Engine::MySql => Ok(vec![mysql_modify(target, original, edit)]),
        Engine::Sqlite => sqlite_modify(target, original, edit),
    }
}

/// Postgres: one statement per changed property, in an order where each later
/// one can still name the column by its current name.
fn postgres_modify(target: &str, original: &ColumnDef, edit: &ColumnEdit) -> Vec<String> {
    let engine = Engine::Postgres;
    let mut statements = Vec::new();

    if edit.renamed() {
        statements.push(format!(
            "ALTER TABLE {target} RENAME COLUMN {} TO {}",
            quote_identifier(&original.name, engine),
            quote_identifier(&edit.name, engine)
        ));
    }
    let current = quote_identifier(&edit.name, engine);

    if edit.retyped() {
        statements.push(format!(
            "ALTER TABLE {target} ALTER COLUMN {current} TYPE {} USING {current}::{}",
            edit.type_name, edit.type_name
        ));
    }
    if edit.nullability_changed() {
        let clause = if edit.nullable {
            "DROP NOT NULL"
        } else {
            "SET NOT NULL"
        };
        statements.push(format!(
            "ALTER TABLE {target} ALTER COLUMN {current} {clause}"
        ));
    }
    if edit.default_changed() {
        match &edit.default {
            Some(expr) => statements.push(format!(
                "ALTER TABLE {target} ALTER COLUMN {current} SET DEFAULT {expr}"
            )),
            None => statements.push(format!(
                "ALTER TABLE {target} ALTER COLUMN {current} DROP DEFAULT"
            )),
        }
    }

    statements
}

/// MySQL restates the whole column either way: `CHANGE COLUMN` for a rename
/// (it has no standalone rename), `MODIFY COLUMN` otherwise.
fn mysql_modify(target: &str, original: &ColumnDef, edit: &ColumnEdit) -> String {
    let engine = Engine::MySql;
    if edit.renamed() {
        format!(
            "ALTER TABLE {target} CHANGE COLUMN {} {}",
            quote_identifier(&original.name, engine),
            column_clause(engine, edit)
        )
    } else {
        format!(
            "ALTER TABLE {target} MODIFY COLUMN {}",
            column_clause(engine, edit)
        )
    }
}

/// SQLite's plain `ALTER TABLE` only reaches a rename; anything else about an
/// existing column needs the rebuild procedure phase 3 adds.
fn sqlite_modify(
    target: &str,
    original: &ColumnDef,
    edit: &ColumnEdit,
) -> Result<Vec<String>, String> {
    if edit.retyped() || edit.nullability_changed() || edit.default_changed() {
        return Err(format!(
            "changing {}'s type, nullability, or default is not supported on SQLite \
             without rebuilding the table",
            original.name
        ));
    }

    Ok(vec![format!(
        "ALTER TABLE {target} RENAME COLUMN {} TO {}",
        quote_identifier(&original.name, Engine::Sqlite),
        quote_identifier(&edit.name, Engine::Sqlite)
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ObjectKind;

    fn column(name: &str, type_name: &str, nullable: bool, default: Option<&str>) -> ColumnDef {
        ColumnDef {
            name: name.to_string(),
            type_name: type_name.to_string(),
            nullable,
            default: default.map(str::to_string),
            is_primary_key: false,
        }
    }

    fn kept(original: ColumnDef) -> ColumnEdit {
        ColumnEdit {
            name: original.name.clone(),
            type_name: original.type_name.clone(),
            nullable: original.nullable,
            default: original.default.clone(),
            original: Some(original),
            dropped: false,
        }
    }

    fn added(name: &str, type_name: &str, nullable: bool, default: Option<&str>) -> ColumnEdit {
        ColumnEdit {
            original: None,
            name: name.to_string(),
            type_name: type_name.to_string(),
            nullable,
            default: default.map(str::to_string),
            dropped: false,
        }
    }

    #[test]
    fn target_is_qualified_only_when_the_object_has_a_schema() {
        let table = DatabaseObject {
            schema: None,
            name: "items".into(),
            kind: ObjectKind::Table,
        };
        assert_eq!(target(&table, Engine::Postgres), "items");

        let qualified = DatabaseObject {
            schema: Some("app".into()),
            ..table
        };
        assert_eq!(target(&qualified, Engine::Postgres), "app.items");
    }

    #[test]
    fn postgres_generates_one_statement_per_changed_property() {
        let mut edit = kept(column("name", "text", true, None));
        edit.name = "full_name".to_string();
        edit.type_name = "varchar(255)".to_string();
        edit.nullable = false;
        edit.default = Some("'anon'".to_string());

        let statements =
            generate_alter_statements(Engine::Postgres, "items", &[edit]).expect("should generate");
        assert_eq!(
            statements,
            [
                "ALTER TABLE items RENAME COLUMN name TO full_name",
                "ALTER TABLE items ALTER COLUMN full_name TYPE varchar(255) USING full_name::varchar(255)",
                "ALTER TABLE items ALTER COLUMN full_name SET NOT NULL",
                "ALTER TABLE items ALTER COLUMN full_name SET DEFAULT 'anon'",
            ]
        );
    }

    #[test]
    fn postgres_add_and_drop_column() {
        let mut dropped = kept(column("score", "real", true, None));
        dropped.dropped = true;
        let new = added("rank", "integer", false, Some("0"));

        let statements = generate_alter_statements(Engine::Postgres, "items", &[dropped, new])
            .expect("should generate");
        assert_eq!(
            statements,
            [
                "ALTER TABLE items ADD COLUMN rank integer NOT NULL DEFAULT 0",
                "ALTER TABLE items DROP COLUMN score",
            ]
        );
    }

    #[test]
    fn mysql_renames_with_change_column_and_retypes_with_modify() {
        let mut renamed = kept(column("name", "varchar(255)", true, None));
        renamed.name = "full_name".to_string();

        let statements =
            generate_alter_statements(Engine::MySql, "items", &[renamed]).expect("should generate");
        assert_eq!(
            statements,
            ["ALTER TABLE items CHANGE COLUMN name full_name varchar(255)"]
        );

        let mut retyped = kept(column("score", "int", true, None));
        retyped.type_name = "bigint".to_string();
        retyped.nullable = false;

        let statements =
            generate_alter_statements(Engine::MySql, "items", &[retyped]).expect("should generate");
        assert_eq!(
            statements,
            ["ALTER TABLE items MODIFY COLUMN score bigint NOT NULL"]
        );
    }

    #[test]
    fn sqlite_allows_a_rename_but_refuses_a_retype() {
        let mut renamed = kept(column("name", "TEXT", true, None));
        renamed.name = "full_name".to_string();

        let statements = generate_alter_statements(Engine::Sqlite, "items", &[renamed])
            .expect("a rename alone should generate");
        assert_eq!(
            statements,
            ["ALTER TABLE items RENAME COLUMN name TO full_name"]
        );

        let mut retyped = kept(column("score", "INTEGER", true, None));
        retyped.type_name = "REAL".to_string();

        let error = generate_alter_statements(Engine::Sqlite, "items", &[retyped])
            .expect_err("a retype should be refused");
        assert!(error.contains("score") && error.contains("rebuilding"));
    }

    #[test]
    fn a_changed_column_cannot_be_renamed_to_nothing() {
        let mut edit = kept(column("name", "text", true, None));
        edit.name = String::new();

        let error = generate_alter_statements(Engine::Postgres, "items", &[edit])
            .expect_err("an empty name should be refused");
        assert!(error.contains("name"));
    }

    #[test]
    fn a_blank_added_row_generates_nothing() {
        let statements =
            generate_alter_statements(Engine::Postgres, "items", &[added("", "text", true, None)])
                .expect("should generate");
        assert!(statements.is_empty());
    }

    #[test]
    fn nothing_changed_generates_nothing() {
        let statements = generate_alter_statements(
            Engine::Postgres,
            "items",
            &[kept(column("name", "text", true, None))],
        )
        .expect("should generate");
        assert!(statements.is_empty());
    }
}
