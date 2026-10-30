//! The tools the model can call, and the code that runs them.
//!
//! Each tool is one small operation through an existing path: the catalog
//! snapshot, `Connection::table_schema`, or a classified single statement. The
//! access level is checked here, in code, on every call — a model that names a
//! tool it was never offered gets a refusal, not the tool.

use std::sync::Arc;

use serde_json::{Value, json};

use crate::db::catalog::{self, Catalog, CatalogKind, Query};
use crate::db::query::QueryResult;
use crate::db::schema::TableSchema;
use crate::db::statement;
use futures::future::BoxFuture;

use crate::db::{AgentAccess, Connection, DatabaseObject, WriteApproval};

use super::gate;

/// Rows a `run_select` hands back; the statement itself is limited to this
/// too, so the server does not read more than the model could see.
pub const MAX_ROWS: usize = 100;
/// Characters of one cell kept, so a document column cannot fill the context.
pub const MAX_CELL: usize = 200;
/// Hits `search_schema` and `list_tables` return before saying "and more".
pub const MAX_LISTED: usize = 200;

/// What a tool asks the window to do, for the tools that act on the UI rather
/// than the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    OpenTable(DatabaseObject),
    ProposeQuery(String),
    /// A write changed the schema: the session's catalog snapshot is stale
    /// until it is re-read.
    RefreshCatalog,
}

/// What a tool call came to: the text the model reads back, and an optional
/// request for the window.
#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
    pub ui: Option<UiAction>,
}

impl ToolOutput {
    fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: true,
            ui: None,
        }
    }
}

/// A tool as offered to the model.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    /// The lowest access level that is offered the tool.
    pub minimum: AgentAccess,
}

impl ToolDef {
    /// The `tools` array entry of a chat-completions request.
    pub fn to_json(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }
}

fn string_param(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn object_params(properties: Value, required: &[&str]) -> Value {
    json!({ "type": "object", "properties": properties, "required": required })
}

/// Every tool, whatever the access level.
fn all() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "list_tables",
            description: "List the tables and views in the current database.",
            parameters: object_params(json!({}), &[]),
            minimum: AgentAccess::Schema,
        },
        ToolDef {
            name: "search_schema",
            description: "Find tables, views, columns, indexes, routines and triggers by name. \
                Supports prefixes kind:column, table:orders and type:int.",
            parameters: object_params(
                json!({ "query": string_param("What to look for") }),
                &["query"],
            ),
            minimum: AgentAccess::Schema,
        },
        ToolDef {
            name: "describe_table",
            description: "Columns, types, keys, indexes and foreign keys of one table or view, \
                with a view's definition.",
            parameters: object_params(
                json!({
                    "name": string_param("Table or view name"),
                    "schema": string_param("Schema, when the name is ambiguous"),
                }),
                &["name"],
            ),
            minimum: AgentAccess::Schema,
        },
        ToolDef {
            name: "open_table",
            description: "Open a table's rows in a tab for the user.",
            parameters: object_params(
                json!({
                    "name": string_param("Table or view name"),
                    "schema": string_param("Schema, when the name is ambiguous"),
                }),
                &["name"],
            ),
            minimum: AgentAccess::Schema,
        },
        ToolDef {
            name: "propose_query",
            description: "Put SQL in a new query tab without running it. The user decides \
                whether to run it.",
            parameters: object_params(
                json!({ "sql": string_param("The SQL to propose") }),
                &["sql"],
            ),
            minimum: AgentAccess::Schema,
        },
        ToolDef {
            name: "run_write",
            description: "Run one INSERT, UPDATE or DELETE statement (at Full access also DDL \
                and grants). The user may be asked to approve it first, and may refuse; \
                a refusal is final for that statement. Each statement runs in a session \
                of its own, so SET, USE, temporary tables and other session state do not \
                carry over to later statements.",
            parameters: object_params(
                json!({ "sql": string_param("One write statement") }),
                &["sql"],
            ),
            minimum: AgentAccess::Crud,
        },
        ToolDef {
            name: "run_select",
            description: "Run one SELECT (or WITH … SELECT) statement and read its rows. \
                At most 100 rows come back.",
            parameters: object_params(
                json!({ "sql": string_param("One read statement") }),
                &["sql"],
            ),
            minimum: AgentAccess::ReadOnlySql,
        },
        ToolDef {
            name: "explain_query",
            description: "Show the server's plan for one SELECT statement without running it.",
            parameters: object_params(
                json!({ "sql": string_param("One read statement") }),
                &["sql"],
            ),
            minimum: AgentAccess::ReadOnlySql,
        },
    ]
}

/// The tools offered at `access`.
pub fn definitions(access: AgentAccess) -> Vec<ToolDef> {
    if !access.enabled() {
        return Vec::new();
    }
    all()
        .into_iter()
        .filter(|tool| access >= tool.minimum)
        .collect()
}

/// A write waiting for a person's yes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    pub sql: String,
    pub kind: gate::Kind,
    pub access: AgentAccess,
    pub approval: WriteApproval,
}

/// Whoever answers a write the model asked for: the panel, which shows the SQL
/// and waits for Approve or Reject. Approval is for that one statement only.
pub trait Approver: Send + Sync {
    fn approve(&self, request: WriteRequest) -> BoxFuture<'_, bool>;
}

/// Approves nothing. For contexts with no one to ask.
#[cfg(test)]
pub struct NeverApprove;

#[cfg(test)]
impl Approver for NeverApprove {
    fn approve(&self, _: WriteRequest) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
}

/// What a tool needs to run.
pub struct ToolContext {
    pub connection: Arc<Connection>,
    pub catalog: Arc<Catalog>,
    pub access: AgentAccess,
    pub approval: WriteApproval,
}

/// Run the tool `name` with the model's `arguments` (a JSON object as text).
///
/// The access check is repeated here whatever was offered, so it holds even for
/// a name the model made up or remembered from an earlier turn.
pub async fn call(
    context: &ToolContext,
    approver: &dyn Approver,
    name: &str,
    arguments: &str,
) -> ToolOutput {
    let Some(tool) = all().into_iter().find(|tool| tool.name == name) else {
        return ToolOutput::error(format!("there is no tool named {name}"));
    };
    if !context.access.enabled() || context.access < tool.minimum {
        return ToolOutput::error(format!(
            "{name} is not available at the {} access level",
            context.access.label()
        ));
    }
    let arguments: Value = match serde_json::from_str(if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    }) {
        Ok(value) => value,
        Err(error) => {
            return ToolOutput::error(format!("the arguments were not valid JSON: {error}"));
        }
    };
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };

    match name {
        "list_tables" => list_tables(&context.catalog),
        "search_schema" => search_schema(&context.catalog, &text("query")),
        "describe_table" => describe_table(context, &text("name"), &text("schema")).await,
        "open_table" => match find_object(&context.catalog, &text("name"), &text("schema")) {
            Ok(object) => ToolOutput {
                text: format!("Opened {} for the user.", object.label()),
                is_error: false,
                ui: Some(UiAction::OpenTable(object)),
            },
            Err(message) => ToolOutput::error(message),
        },
        "propose_query" => {
            let sql = text("sql");
            if sql.trim().is_empty() {
                return ToolOutput::error("the SQL is empty");
            }
            ToolOutput {
                text: "The query is in a new tab. It has not been run; the user will decide."
                    .into(),
                is_error: false,
                ui: Some(UiAction::ProposeQuery(sql)),
            }
        }
        "run_select" => run_select(context, &text("sql"), false).await,
        "explain_query" => run_select(context, &text("sql"), true).await,
        "run_write" => run_write(context, approver, &text("sql")).await,
        _ => ToolOutput::error(format!("there is no tool named {name}")),
    }
}

fn list_tables(catalog: &Catalog) -> ToolOutput {
    let mut lines = Vec::new();
    let mut total = 0;
    for entry in &catalog.entries {
        if !matches!(entry.kind, CatalogKind::Table | CatalogKind::View) {
            continue;
        }
        total += 1;
        if lines.len() < MAX_LISTED {
            let label = entry
                .object
                .as_ref()
                .map_or(entry.name.clone(), |o| o.label());
            lines.push(format!("{} {label}", entry.kind.label().to_lowercase()));
        }
    }
    if total == 0 {
        return ToolOutput::ok("The database has no tables or views.");
    }
    if total > lines.len() {
        lines.push(format!(
            "… and {} more; use search_schema to narrow",
            total - lines.len()
        ));
    }
    ToolOutput::ok(lines.join("\n"))
}

fn search_schema(catalog: &Catalog, raw: &str) -> ToolOutput {
    let query = Query::parse(raw);
    let (hits, matched) = catalog::search(&catalog.entries, &query);
    if hits.is_empty() {
        return ToolOutput::ok("Nothing matched.");
    }
    let mut lines: Vec<String> = hits
        .iter()
        .take(MAX_LISTED)
        .map(|hit| {
            let entry = &catalog.entries[hit.index];
            let owner = entry
                .object
                .as_ref()
                .filter(|_| !matches!(entry.kind, CatalogKind::Table | CatalogKind::View))
                .map(|object| format!(" in {}", object.label()))
                .unwrap_or_default();
            let detail = if entry.detail.is_empty() {
                String::new()
            } else {
                format!(" ({})", entry.detail)
            };
            format!(
                "{} {}{owner}{detail}",
                entry.kind.label().to_lowercase(),
                entry.name
            )
        })
        .collect();
    if matched > lines.len() {
        lines.push(format!("… {matched} matched in all"));
    }
    ToolOutput::ok(lines.join("\n"))
}

/// The table or view the model means, from the catalog snapshot. A bare name
/// that two schemas share is reported rather than guessed.
fn find_object(catalog: &Catalog, name: &str, schema: &str) -> Result<DatabaseObject, String> {
    let wanted = name.trim().to_lowercase();
    if wanted.is_empty() {
        return Err("name the table or view".into());
    }
    let matches: Vec<&DatabaseObject> = catalog
        .entries
        .iter()
        .filter(|entry| matches!(entry.kind, CatalogKind::Table | CatalogKind::View))
        .filter_map(|entry| entry.object.as_ref())
        .filter(|object| {
            object.name.to_lowercase() == wanted
                && (schema.trim().is_empty()
                    || object.schema.as_deref().map(str::to_lowercase)
                        == Some(schema.trim().to_lowercase()))
        })
        .collect();
    match matches.as_slice() {
        [] => Err(format!(
            "no table or view named {name}; use list_tables or search_schema"
        )),
        [one] => Ok((*one).clone()),
        many => Err(format!(
            "{name} is in several schemas ({}); pass schema",
            many.iter()
                .map(|o| o.label())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

async fn describe_table(context: &ToolContext, name: &str, schema: &str) -> ToolOutput {
    let object = match find_object(&context.catalog, name, schema) {
        Ok(object) => object,
        Err(message) => return ToolOutput::error(message),
    };
    let connection = context.connection.clone();
    let target = object.clone();
    let loaded = crate::db::runtime::spawn(async move {
        let schema = connection.table_schema(&target).await?;
        let definition = if target.kind == crate::db::ObjectKind::View {
            connection.view_definition(&target).await.ok()
        } else {
            None
        };
        anyhow::Ok((schema, definition))
    })
    .await;
    match loaded {
        Ok(Ok((schema, definition))) => ToolOutput::ok(describe(&object, &schema, definition)),
        Ok(Err(error)) => ToolOutput::error(format!("{error:#}")),
        Err(_) => ToolOutput::error("cancelled"),
    }
}

fn describe(object: &DatabaseObject, schema: &TableSchema, definition: Option<String>) -> String {
    let mut out = format!("{}\ncolumns:\n", object.label());
    for column in &schema.columns {
        out.push_str(&format!(
            "- {} {}{}{}{}\n",
            column.name,
            column.type_name,
            if column.nullable { "" } else { " not null" },
            if column.is_primary_key {
                " primary key"
            } else {
                ""
            },
            column
                .default
                .as_ref()
                .map(|default| format!(" default {default}"))
                .unwrap_or_default(),
        ));
    }
    if !schema.indexes.is_empty() {
        out.push_str("indexes:\n");
        for index in &schema.indexes {
            out.push_str(&format!(
                "- {} ({}){}\n",
                index.name,
                index.columns.join(", "),
                if index.unique { " unique" } else { "" }
            ));
        }
    }
    if !schema.foreign_keys.is_empty() {
        out.push_str("foreign keys:\n");
        for key in &schema.foreign_keys {
            out.push_str(&format!(
                "- ({}) references {} ({})\n",
                key.columns.join(", "),
                key.referenced_table,
                key.referenced_columns.join(", ")
            ));
        }
    }
    if let Some(definition) = definition {
        out.push_str(&format!("definition:\n{definition}\n"));
    }
    out
}

async fn run_write(context: &ToolContext, approver: &dyn Approver, sql: &str) -> ToolOutput {
    let engine = context.connection.config.engine;
    let sql = match gate::single(sql, engine) {
        Ok(sql) => sql,
        Err(message) => return ToolOutput::error(message),
    };
    let kind = match gate::classify(&sql, engine) {
        Ok(gate::Kind::Read) => {
            return ToolOutput::error("that statement only reads; use run_select");
        }
        Ok(kind) => kind,
        Err(message) => return ToolOutput::error(message),
    };
    if let Err(message) = gate::allowed(&kind, context.access) {
        return ToolOutput::error(message);
    }
    // Read before the Ask arm moves `kind` into its request: only DDL
    // rewrites the schema the catalog snapshot was read from.
    let is_ddl = kind == gate::Kind::Other;
    match gate::write_decision(
        context.access,
        context.connection.config.safety,
        context.approval,
    ) {
        gate::WriteDecision::Refuse(why) => return ToolOutput::error(why),
        gate::WriteDecision::Run => {}
        gate::WriteDecision::Ask => {
            let request = WriteRequest {
                sql: sql.clone(),
                kind,
                access: context.access,
                approval: context.approval,
            };
            if !approver.approve(request).await {
                return ToolOutput::error(
                    "the user rejected this statement and it did not run; do not retry it, \
                     propose an alternative instead",
                );
            }
        }
    }
    let connection = context.connection.clone();
    match crate::db::runtime::spawn(async move { connection.execute_as_agent(&sql).await }).await {
        Ok(Ok(affected)) => {
            let mut out = ToolOutput::ok(format!(
                "{affected} row{} affected",
                if affected == 1 { "" } else { "s" }
            ));
            // DDL rewrites the schema the panel's catalog snapshot was read
            // from, so the session re-reads it once the turn ends. DML
            // leaves the schema alone and asks for nothing.
            if is_ddl {
                out.ui = Some(UiAction::RefreshCatalog);
            }
            out
        }
        Ok(Err(error)) => ToolOutput::error(format!("{error:#}")),
        Err(_) => ToolOutput::error("cancelled"),
    }
}

async fn run_select(context: &ToolContext, sql: &str, explain: bool) -> ToolOutput {
    let engine = context.connection.config.engine;
    let sql = match gate::single(sql, engine) {
        Ok(sql) => sql,
        Err(message) => return ToolOutput::error(message),
    };
    // `EXPLAIN` of a write is a write, and `ANALYZE` runs what it explains:
    // only a plain read gets through either tool.
    match gate::classify(&sql, engine) {
        Ok(kind) => {
            if let Err(message) = gate::allowed(&kind, context.access) {
                return ToolOutput::error(message);
            }
            if kind != gate::Kind::Read {
                return ToolOutput::error(
                    "this tool only runs reads; writes are not available through it",
                );
            }
        }
        Err(message) => return ToolOutput::error(message),
    }

    let sent = if explain {
        format!("EXPLAIN {sql}")
    } else {
        // The cap is enforced in SQL where the statement allows it, so a
        // model-written `LIMIT 100000` does not have the server read the
        // full result first. Where it does not — an `OFFSET` with no
        // `LIMIT`, a `LIMIT` written as an expression, a locking clause —
        // the read below still stops after one row more than is shown.
        statement::clamp_limit(&sql, MAX_ROWS, engine).unwrap_or(sql)
    };
    let connection = context.connection.clone();
    match crate::db::runtime::spawn(async move {
        connection.run_query_as_agent(&sent, MAX_ROWS + 1).await
    })
    .await
    {
        Ok(Ok(result)) => ToolOutput::ok(format_result(&result)),
        Ok(Err(error)) => ToolOutput::error(format!("{error:#}")),
        Err(_) => ToolOutput::error("cancelled"),
    }
}

/// A result as the model reads it: JSON, capped in rows and in cell length,
/// and saying when it was cut.
pub fn format_result(result: &QueryResult) -> String {
    let rows: Vec<Vec<Value>> = result
        .rows
        .iter()
        .take(MAX_ROWS)
        .map(|row| {
            row.iter()
                .map(|cell| match cell {
                    None => Value::Null,
                    Some(text) if text.chars().count() > MAX_CELL => {
                        let cut: String = text.chars().take(MAX_CELL).collect();
                        Value::String(format!("{cut}… (cut)"))
                    }
                    Some(text) => Value::String(text.clone()),
                })
                .collect()
        })
        .collect();
    let mut out = json!({ "columns": result.columns, "rows": rows });
    // The read stops one row past the cap, so the total is not known; only
    // that there was more.
    if result.rows.len() > MAX_ROWS {
        out["note"] = json!(format!(
            "the result has more than {MAX_ROWS} rows; only the first {MAX_ROWS} are shown"
        ));
    }
    out.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ObjectKind;
    use crate::db::catalog::CatalogEntry;

    fn object(name: &str, schema: Option<&str>) -> DatabaseObject {
        DatabaseObject {
            schema: schema.map(String::from),
            name: name.into(),
            kind: ObjectKind::Table,
        }
    }

    fn catalog(objects: Vec<DatabaseObject>) -> Catalog {
        let entries: Vec<_> = objects.into_iter().map(CatalogEntry::object).collect();
        Catalog {
            total: entries.len(),
            entries,
        }
    }

    #[test]
    fn each_level_is_offered_only_its_tools() {
        let names = |access| -> Vec<&str> { definitions(access).iter().map(|t| t.name).collect() };
        assert!(names(AgentAccess::None).is_empty());
        assert!(names(AgentAccess::Schema).contains(&"describe_table"));
        assert!(!names(AgentAccess::Schema).contains(&"run_select"));
        assert!(!names(AgentAccess::Schema).contains(&"explain_query"));
        assert!(names(AgentAccess::ReadOnlySql).contains(&"run_select"));
        assert!(names(AgentAccess::Schema).contains(&"propose_query"));
    }

    #[test]
    fn finding_a_table_ignores_case_and_reports_ambiguity() {
        let catalog = catalog(vec![
            object("Orders", Some("public")),
            object("orders", Some("audit")),
        ]);
        assert!(
            find_object(&catalog, "orders", "")
                .unwrap_err()
                .contains("several schemas")
        );
        assert_eq!(
            find_object(&catalog, "ORDERS", "audit")
                .unwrap()
                .schema
                .as_deref(),
            Some("audit")
        );
        assert!(find_object(&catalog, "nope", "").is_err());
    }

    #[test]
    fn results_are_capped_in_rows_and_cells() {
        let result = QueryResult {
            columns: vec!["a".into()],
            rows: (0..150).map(|_| vec![Some("x".repeat(500))]).collect(),
            ..QueryResult::default()
        };
        let text = format_result(&result);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["rows"].as_array().unwrap().len(), MAX_ROWS);
        assert!(value["note"].is_string());
        assert!(value["rows"][0][0].as_str().unwrap().chars().count() < 300);
    }

    #[test]
    fn a_large_limit_is_enforced_in_sql_not_just_in_the_display() {
        let (_database, context) = live(AgentAccess::ReadOnlySql);
        futures::executor::block_on(async {
            // 150 rows behind a `LIMIT 100000`: if the cap were only the
            // display truncation, the result would carry the "only the
            // first 100" note; enforced in SQL it is exactly 100 rows with
            // no note at all.
            let out = call(
                &context,
                &NeverApprove,
                "run_select",
                r#"{"sql":"WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM cnt WHERE x < 150) SELECT x FROM cnt LIMIT 100000"}"#,
            )
            .await;
            assert!(!out.is_error, "{}", out.text);
            let value: Value = serde_json::from_str(&out.text).unwrap();
            assert_eq!(value["rows"].as_array().unwrap().len(), MAX_ROWS);
            assert!(value.get("note").is_none(), "{}", out.text);
        });
    }

    #[test]
    fn a_read_clamp_limit_cannot_bound_is_still_cut_at_the_cap() {
        let (_database, context) = live(AgentAccess::ReadOnlySql);
        futures::executor::block_on(async {
            // A `LIMIT` written as an expression is left as written by
            // `clamp_limit`, so the server returns every row; the read
            // stops one past the cap and the note says there was more.
            let out = call(
                &context,
                &NeverApprove,
                "run_select",
                r#"{"sql":"WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM cnt WHERE x < 5000) SELECT x FROM cnt LIMIT (5000)"}"#,
            )
            .await;
            assert!(!out.is_error, "{}", out.text);
            let value: Value = serde_json::from_str(&out.text).unwrap();
            assert_eq!(value["rows"].as_array().unwrap().len(), MAX_ROWS);
            assert!(
                value["note"].as_str().unwrap().contains("more than"),
                "{}",
                out.text
            );
        });
    }

    #[test]
    fn listing_names_tables() {
        let catalog = catalog(vec![object("users", None)]);
        assert!(list_tables(&catalog).text.contains("table users"));
        assert!(list_tables(&Catalog::default()).text.contains("no tables"));
    }

    fn live(access: AgentAccess) -> (crate::db::tests::TempDatabase, ToolContext) {
        live_with(access, WriteApproval::Ask, crate::db::SafetyMode::Staged)
    }

    fn live_with(
        access: AgentAccess,
        approval: WriteApproval,
        safety: crate::db::SafetyMode,
    ) -> (crate::db::tests::TempDatabase, ToolContext) {
        use crate::db::runtime::block_on as setup;
        let database = setup(crate::db::tests::TempDatabase::new());
        let config = crate::db::ConnectionConfig {
            safety,
            ..database.config()
        };
        let connection = setup(Connection::open(config, None)).unwrap();
        let catalog = setup(connection.catalog()).unwrap();
        let context = ToolContext {
            connection: Arc::new(connection),
            catalog: Arc::new(catalog),
            access,
            approval,
        };
        (database, context)
    }

    #[test]
    fn reads_run_are_logged_as_the_agent_and_writes_are_refused() {
        let (_database, context) = live(AgentAccess::ReadOnlySql);
        futures::executor::block_on(async {
            let out = call(
                &context,
                &NeverApprove,
                "run_select",
                r#"{"sql":"select name from items"}"#,
            )
            .await;
            assert!(!out.is_error, "{}", out.text);
            assert!(out.text.contains("alpha"));
            let log = context.connection.query_log().snapshot();
            assert!(
                log.iter()
                    .any(|entry| entry.source == crate::db::QuerySource::Agent)
            );

            for sql in [
                "delete from items",
                "select 1; delete from items",
                "with x as (select 1) delete from items",
                "begin",
            ] {
                let out = call(
                    &context,
                    &NeverApprove,
                    "run_select",
                    &json!({ "sql": sql }).to_string(),
                )
                .await;
                assert!(out.is_error, "{sql}");
            }
            let connection = context.connection.clone();
            let count = crate::db::runtime::spawn(async move {
                connection.run_query("select count(*) from items").await
            })
            .await
            .unwrap()
            .unwrap();
            assert_eq!(count.rows[0][0].as_deref(), Some("2"));
        });
    }

    #[test]
    fn schema_access_reads_no_rows() {
        let (_database, context) = live(AgentAccess::Schema);
        futures::executor::block_on(async {
            let out = call(
                &context,
                &NeverApprove,
                "run_select",
                r#"{"sql":"select 1"}"#,
            )
            .await;
            assert!(out.is_error);
            let out = call(
                &context,
                &NeverApprove,
                "describe_table",
                r#"{"name":"items"}"#,
            )
            .await;
            assert!(!out.is_error, "{}", out.text);
            assert!(out.text.contains("name"));
            assert!(!out.text.contains("alpha"));
            let out = call(
                &context,
                &NeverApprove,
                "propose_query",
                r#"{"sql":"select 1"}"#,
            )
            .await;
            assert_eq!(out.ui, Some(UiAction::ProposeQuery("select 1".into())));
        });
    }

    #[test]
    fn explain_never_runs_a_write() {
        let (_database, context) = live(AgentAccess::Full);
        futures::executor::block_on(async {
            let out = call(
                &context,
                &NeverApprove,
                "explain_query",
                r#"{"sql":"delete from items"}"#,
            )
            .await;
            assert!(out.is_error);
        });
    }

    /// Answers every request the same way and counts them.
    struct Answer(bool, std::sync::atomic::AtomicUsize);

    impl Approver for Answer {
        fn approve(&self, _: WriteRequest) -> BoxFuture<'_, bool> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let answer = self.0;
            Box::pin(async move { answer })
        }
    }

    fn rows(context: &ToolContext) -> String {
        let connection = context.connection.clone();
        let result = crate::db::runtime::block_on(async move {
            connection.run_query("select count(*) from items").await
        })
        .unwrap();
        result.rows[0][0].clone().unwrap()
    }

    const DELETE: &str = r#"{"sql":"delete from items where id = 1"}"#;

    #[test]
    fn an_approved_write_runs_once_and_a_rejected_one_does_not() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (_database, context) = live(AgentAccess::Crud);
        let yes = Answer(true, AtomicUsize::new(0));
        let out = futures::executor::block_on(call(&context, &yes, "run_write", DELETE));
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(yes.1.load(Ordering::SeqCst), 1);
        assert_eq!(rows(&context), "1");

        let no = Answer(false, AtomicUsize::new(0));
        let out = futures::executor::block_on(call(
            &context,
            &no,
            "run_write",
            r#"{"sql":"delete from items"}"#,
        ));
        assert!(out.is_error && out.text.contains("rejected"));
        assert_eq!(rows(&context), "1");
        let log = context.connection.query_log().snapshot();
        assert!(
            log.iter()
                .any(|e| e.source == crate::db::QuerySource::Agent && e.sql.starts_with("delete"))
        );
    }

    #[test]
    fn auto_approval_skips_the_question_and_never_refuses() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let ask = Answer(false, AtomicUsize::new(0));
        let (_d, auto) = live_with(
            AgentAccess::Crud,
            WriteApproval::Auto,
            crate::db::SafetyMode::AutoApply,
        );
        let out = futures::executor::block_on(call(&auto, &ask, "run_write", DELETE));
        assert!(!out.is_error, "{}", out.text);
        assert_eq!(ask.1.load(Ordering::SeqCst), 0);

        let (_d, never) = live_with(
            AgentAccess::Crud,
            WriteApproval::Never,
            crate::db::SafetyMode::Staged,
        );
        let yes = Answer(true, AtomicUsize::new(0));
        let out = futures::executor::block_on(call(&never, &yes, "run_write", DELETE));
        assert!(out.is_error);
        assert_eq!(yes.1.load(Ordering::SeqCst), 0);
        assert_eq!(rows(&never), "2");
    }

    #[test]
    fn a_read_only_connection_and_low_access_refuse_writes() {
        use std::sync::atomic::AtomicUsize;
        let yes = Answer(true, AtomicUsize::new(0));
        let (_d, read_only) = live_with(
            AgentAccess::Full,
            WriteApproval::Auto,
            crate::db::SafetyMode::ReadOnly,
        );
        let out = futures::executor::block_on(call(&read_only, &yes, "run_write", DELETE));
        assert!(out.is_error);
        assert_eq!(rows(&read_only), "2");

        let (_d, low) = live(AgentAccess::ReadOnlySql);
        let out = futures::executor::block_on(call(&low, &yes, "run_write", DELETE));
        assert!(out.is_error && out.text.contains("not available"));
        assert_eq!(rows(&low), "2");

        // DDL needs Full; CRUD asks for nothing and gets refused.
        let (_d, crud) = live_with(
            AgentAccess::Crud,
            WriteApproval::Auto,
            crate::db::SafetyMode::Staged,
        );
        let out = futures::executor::block_on(call(
            &crud,
            &yes,
            "run_write",
            r#"{"sql":"drop table items"}"#,
        ));
        assert!(out.is_error);
        assert_eq!(rows(&crud), "2");
    }
}
