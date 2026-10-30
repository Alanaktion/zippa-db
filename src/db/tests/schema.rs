//! Catalog, definitions, schema reads, and table rebuilds.

use super::*;

#[tokio::test]
async fn catalog_reads_columns_indexes_and_triggers() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    connection
        .run_query("CREATE INDEX items_name_idx ON items(name)")
        .await
        .expect("could not create the index");
    connection
        .run_query("CREATE TRIGGER items_guard AFTER INSERT ON items BEGIN SELECT 1; END")
        .await
        .expect("could not create the trigger");

    let catalog = connection
        .catalog()
        .await
        .expect("could not read the catalog");
    let find = |kind, name: &str| {
        catalog
            .entries
            .iter()
            .find(|entry| entry.kind == kind && entry.name == name)
    };

    let id = find(CatalogKind::Column, "id").expect("the id column is missing");
    assert_eq!(id.detail, "INTEGER");
    assert_eq!(id.owner().map(|object| object.name.as_str()), Some("items"));

    let index = find(CatalogKind::Index, "items_name_idx").expect("the index is missing");
    assert_eq!(index.detail, "name");
    assert_eq!(
        index.owner().map(|object| object.name.as_str()),
        Some("items")
    );

    let trigger = find(CatalogKind::Trigger, "items_guard").expect("the trigger is missing");
    assert_eq!(
        trigger.owner().map(|object| object.name.as_str()),
        Some("items")
    );

    // Views bring their columns; SQLite's own tables are left out.
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.kind == CatalogKind::View && entry.name == "named_items")
    );
    assert!(
        !catalog
            .entries
            .iter()
            .any(|entry| entry.name.starts_with("sqlite_"))
    );
    assert_eq!(
        catalog
            .entries
            .iter()
            .filter(|entry| entry.kind == CatalogKind::Table)
            .count(),
        1
    );
    assert_eq!(catalog.truncated(), None);

    connection.close().await;
}

#[tokio::test]
async fn a_failing_schema_statement_rolls_the_whole_change_back() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let error = connection
        .execute_script(&[
            "ALTER TABLE items ADD COLUMN note TEXT".to_string(),
            "ALTER TABLE not_a_table ADD COLUMN note TEXT".to_string(),
        ])
        .await
        .expect_err("the second statement should fail");
    assert!(
        format!("{error:#}").contains("statement 2"),
        "the error should name which statement failed: {error:#}"
    );

    let result = connection
        .run_query("SELECT name FROM pragma_table_info('items') WHERE name = 'note'")
        .await
        .expect("the query failed");
    assert!(
        result.rows.is_empty(),
        "the column added before the failure should have been rolled back too"
    );

    connection.close().await;
}

#[tokio::test]
async fn view_definition_returns_the_views_create_statement() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let view = DatabaseObject {
        kind: ObjectKind::View,
        ..table("named_items")
    };
    let definition = connection
        .view_definition(&view)
        .await
        .expect("could not read the view's definition");

    assert!(
        definition.to_ascii_uppercase().starts_with("CREATE VIEW"),
        "the definition should be a CREATE VIEW statement, got: {definition}"
    );
    assert!(
        definition.contains("named_items"),
        "the definition should name the view, got: {definition}"
    );
    assert!(
        definition.contains("SELECT id, name FROM items"),
        "the definition should hold the view's query, got: {definition}"
    );

    connection.close().await;
}

#[tokio::test]
async fn view_definition_refuses_a_view_the_server_does_not_know() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let view = DatabaseObject {
        kind: ObjectKind::View,
        ..table("no_such_view")
    };
    let error = connection
        .view_definition(&view)
        .await
        .expect_err("a missing view should not report a definition");

    assert!(
        format!("{error:#}").contains("no definition"),
        "the error should say there is no definition, got: {error:#}"
    );

    connection.close().await;
}

#[tokio::test]
async fn definition_returns_create_index_and_trigger_in_order() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    for statement in [
        "CREATE TABLE guarded (id INTEGER PRIMARY KEY, score REAL CHECK (score > 0))",
        "CREATE INDEX guarded_score_idx ON guarded(score)",
        "CREATE TRIGGER guarded_guard AFTER INSERT ON guarded BEGIN SELECT 1; END",
    ] {
        connection
            .run_query(statement)
            .await
            .expect("could not seed the table");
    }

    let definition = connection
        .definition(&table("guarded"))
        .await
        .expect("could not read the definition");

    let create = definition
        .find("CREATE TABLE guarded")
        .expect("the definition should start with the table");
    let index = definition
        .find("CREATE INDEX guarded_score_idx")
        .expect("the definition should hold the index");
    let trigger = definition
        .find("CREATE TRIGGER guarded_guard")
        .expect("the definition should hold the trigger");
    assert!(
        create < index && index < trigger,
        "create, then indexes, then triggers, got: {definition}"
    );
    assert!(
        definition.contains("CHECK (score > 0)"),
        "the create text should hold the check, got: {definition}"
    );

    // Round trip: the output recreates an equivalent schema elsewhere.
    let other = TempDatabase::new().await;
    let other_connection = Connection::open(other.config(), None)
        .await
        .expect("could not open the second test database");
    other_connection
        .execute_script(std::slice::from_ref(&definition))
        .await
        .expect("the definition should run on a fresh database");
    let schema = other_connection
        .table_schema(&table("guarded"))
        .await
        .expect("could not read the recreated schema");
    assert_eq!(
        schema
            .columns
            .iter()
            .map(|column| column.name.as_str())
            .collect::<Vec<_>>(),
        ["id", "score"]
    );
    assert_eq!(schema.indexes.len(), 1);
    assert_eq!(schema.indexes[0].name, "guarded_score_idx");

    connection.close().await;
    other_connection.close().await;
}

#[tokio::test]
async fn definition_of_a_view_returns_its_create_statement() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let definition = connection
        .definition(&DatabaseObject {
            kind: ObjectKind::View,
            ..table("named_items")
        })
        .await
        .expect("could not read the view's definition");
    assert!(
        definition.contains("CREATE VIEW"),
        "the definition should be a CREATE VIEW statement, got: {definition}"
    );

    connection.close().await;
}

#[tokio::test]
async fn definition_refuses_a_table_the_server_does_not_know() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let error = connection
        .definition(&table("no_such_table"))
        .await
        .expect_err("a missing table should not report a definition");
    assert!(
        format!("{error:#}").contains("no definition"),
        "the error should say there is no definition, got: {error:#}"
    );

    connection.close().await;
}

#[tokio::test]
async fn table_extras_lists_checks_and_triggers() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    for statement in [
        "CREATE TABLE guarded (id INTEGER PRIMARY KEY, score REAL CHECK (score > 0))",
        "CREATE TRIGGER guarded_guard BEFORE UPDATE OF score ON guarded BEGIN SELECT 1; END",
    ] {
        connection
            .run_query(statement)
            .await
            .expect("could not seed the table");
    }

    let extras = connection
        .table_extras(&table("guarded"))
        .await
        .expect("could not read the extras");
    assert_eq!(extras.checks.len(), 1);
    assert_eq!(extras.checks[0].name, "score");
    assert_eq!(extras.checks[0].expression, "CHECK (score > 0)");
    assert_eq!(extras.triggers.len(), 1);
    assert_eq!(extras.triggers[0].name, "guarded_guard");
    assert_eq!(extras.triggers[0].timing, "BEFORE");
    assert_eq!(extras.triggers[0].events, "UPDATE OF score");
    assert_eq!(extras.triggers[0].orientation, "ROW");
    assert!(extras.trigger_error.is_none());

    connection.close().await;
}

#[tokio::test]
async fn table_schema_reads_columns_and_marks_the_primary_key() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    let schema = connection
        .table_schema(&table("items"))
        .await
        .expect("could not read the schema");

    let names: Vec<&str> = schema.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["id", "name", "score", "payload"]);

    // `id INTEGER PRIMARY KEY` is a `rowid` alias with no backing index, so
    // this only comes from the dedicated primary key read, not the indexes.
    assert!(schema.columns[0].is_primary_key);
    assert!(!schema.columns[1].is_primary_key);
    assert!(schema.indexes.is_empty());
    assert!(schema.foreign_keys.is_empty());

    connection.close().await;
}

#[tokio::test]
async fn table_schema_reads_a_unique_index_and_a_foreign_key() {
    let database = TempDatabase::new().await;
    let connection = Connection::open(database.config(), None)
        .await
        .expect("could not open the test database");

    connection
        .execute(
            "CREATE TABLE tags (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
            Vec::new(),
        )
        .await
        .expect("could not create tags");
    connection
        .execute(
            "CREATE UNIQUE INDEX tags_label_idx ON tags(label)",
            Vec::new(),
        )
        .await
        .expect("could not create the index");
    connection
        .execute(
            "CREATE TABLE tagged_items ( \
                item_id INTEGER, \
                tag_id INTEGER, \
                FOREIGN KEY(item_id) REFERENCES items(id) ON DELETE CASCADE, \
                FOREIGN KEY(tag_id) REFERENCES tags(id) \
             )",
            Vec::new(),
        )
        .await
        .expect("could not create tagged_items");

    let tags = connection
        .table_schema(&table("tags"))
        .await
        .expect("could not read the tags schema");
    let index = tags
        .indexes
        .iter()
        .find(|index| index.name == "tags_label_idx")
        .expect("the unique index should be listed");
    assert!(index.unique);
    assert!(!index.is_primary_key);
    assert_eq!(index.columns, ["label"]);

    let tagged = connection
        .table_schema(&table("tagged_items"))
        .await
        .expect("could not read the tagged_items schema");
    assert_eq!(tagged.foreign_keys.len(), 2);

    let to_items = tagged
        .foreign_keys
        .iter()
        .find(|fk| fk.referenced_table == "items")
        .expect("the foreign key to items should be listed");
    assert_eq!(to_items.columns, ["item_id"]);
    assert_eq!(to_items.referenced_columns, ["id"]);
    assert_eq!(to_items.on_delete, ReferentialAction::Cascade);
    assert_eq!(to_items.on_update, ReferentialAction::NoAction);

    connection.close().await;
}

#[tokio::test]
async fn a_failed_table_rebuild_is_rolled_back() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::default()).await;

    // The first statement would have created the scratch table; the second
    // fails, so the whole rebuild has to come back off.
    let error = connection
        .rebuild_table(vec![
            "CREATE TABLE scratch (x INTEGER)".to_string(),
            "this is not sql".to_string(),
        ])
        .await
        .expect_err("a broken statement should fail the rebuild");
    assert!(format!("{error:#}").contains("statement 2 of 2"));

    let leftover = connection
        .run_query("select name from sqlite_master where name = 'scratch'")
        .await
        .expect("could not read the schema");
    assert!(leftover.rows.is_empty(), "the scratch table should be gone");

    connection.close().await;
}

#[tokio::test]
async fn a_read_only_connection_refuses_a_table_rebuild() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::ReadOnly).await;

    let error = connection
        .rebuild_table(vec!["CREATE TABLE scratch (x INTEGER)".to_string()])
        .await
        .expect_err("a read-only connection must not rebuild");
    assert!(format!("{error:#}").contains("read-only"));

    connection.close().await;
}

#[tokio::test]
async fn a_rebuild_leaves_the_data_and_the_schema_behind() {
    let database = TempDatabase::new().await;
    let connection = open_with(&database, SafetyMode::default()).await;

    connection
        .rebuild_table(vec![
            "CREATE TABLE new_items (id INTEGER, name TEXT, score TEXT, payload BLOB)".to_string(),
            "INSERT INTO new_items (id, name, score, payload) \
             SELECT id, name, score, payload FROM items"
                .to_string(),
            "DROP TABLE items".to_string(),
            "ALTER TABLE new_items RENAME TO items".to_string(),
        ])
        .await
        .expect("a plain rebuild should run");

    let score = connection
        .run_query("select score from items where id = 1")
        .await
        .expect("could not read the rebuilt table");
    assert_eq!(score.rows[0][0].as_deref(), Some("1.5"));

    // The old table is gone and the new definition is what is left.
    let shape = connection
        .run_query("select name from pragma_table_info('items') order by cid")
        .await
        .expect("could not read the rebuilt columns");
    let columns: Vec<String> = shape
        .rows
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    assert_eq!(columns, ["id", "name", "score", "payload"]);

    connection.close().await;
}
