//! The structure tab: read-only indexes and foreign keys, editable columns.

use super::*;
use crate::db::SafetyMode;
use crate::ui::session::tab::ObjectViewMode;

#[gpui_kit::test]
fn opening_a_tables_structure_shows_its_columns(cx: &mut TestAppContext) {
    let (_database, _handle, view) = schema_view(cx);

    let schema = view
        .read_with(cx, |view, _| view.schema_for_test())
        .expect("the structure should have loaded");

    let names: Vec<&str> = schema
        .columns
        .iter()
        .map(|column| column.name.as_str())
        .collect();
    assert_eq!(names, ["id", "name", "score", "payload"]);
    assert!(
        schema.columns[0].is_primary_key,
        "id is the table's primary key"
    );
    assert!(!schema.columns[1].is_primary_key);
    assert!(schema.indexes.is_empty());
    assert!(schema.foreign_keys.is_empty());
    assert!(
        view.read_with(cx, |view, _| view.error_for_test())
            .is_none()
    );
}

#[gpui_kit::test]
fn the_tab_title_says_structure(cx: &mut TestAppContext) {
    let (_database, handle, _view) = schema_view(cx);

    assert_eq!(
        handle
            .update(cx, |session, _, _| session.tab_titles())
            .unwrap(),
        ["Query 1", "items — Structure"]
    );
}

#[gpui_kit::test]
fn a_structure_tab_already_open_is_brought_forward(cx: &mut TestAppContext) {
    let (_database, handle, _view) = schema_view(cx);

    let object = DatabaseObject {
        schema: None,
        name: "items".into(),
        kind: ObjectKind::Table,
    };
    handle
        .update(cx, |session, window, cx| {
            session.open_schema_for_test(&object, window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    assert_eq!(
        handle
            .update(cx, |session, _, _| session.tab_titles())
            .unwrap(),
        ["Query 1", "items — Structure"],
        "opening the same table's structure again should not duplicate the tab"
    );
}

#[gpui_kit::test]
fn renaming_a_column_previews_and_applies_a_rename_column_statement(cx: &mut TestAppContext) {
    let (database, _handle, view) = schema_view(cx);
    cx.run_until_parked();

    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.set_column_name_for_test(1, "full_name", window, cx);
            view.preview_for_test(window, cx);
        })
        .unwrap();

    assert_eq!(
        view.read_with(cx, |view, _| view.pending_statements_for_test()),
        Some(vec![
            "ALTER TABLE items RENAME COLUMN name TO full_name".to_string()
        ])
    );

    view.downgrade()
        .update_in(cx, |view, _window, cx| view.apply_for_test(cx))
        .unwrap();
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, _| view.notice_for_test()),
        Some("Changes applied".to_string())
    );
    let names = view.read_with(cx, |view, cx| view.column_names_for_test(cx));
    assert_eq!(names, ["id", "full_name", "score", "payload"]);

    // The rename really ran on the server, not just in the form.
    assert_eq!(
        runtime::block_on(async {
            let connection = Connection::open(database.config(), None)
                .await
                .expect("could not reopen the test database");
            let result = connection
                .run_query("select full_name from items where id = 1")
                .await
                .expect("the renamed column should be queryable");
            connection.close().await;
            result.rows[0][0].clone()
        }),
        Some("alpha".to_string())
    );
}

#[gpui_kit::test]
fn dropping_a_column_previews_and_applies_a_drop_column_statement(cx: &mut TestAppContext) {
    let (_database, _handle, view) = schema_view(cx);
    cx.run_until_parked();

    view.downgrade()
        .update_in(cx, |view, _window, cx| view.toggle_drop_for_test(2, cx))
        .unwrap();

    view.downgrade()
        .update_in(cx, |view, window, cx| view.preview_for_test(window, cx))
        .unwrap();
    assert_eq!(
        view.read_with(cx, |view, _| view.pending_statements_for_test()),
        Some(vec!["ALTER TABLE items DROP COLUMN score".to_string()])
    );

    view.downgrade()
        .update_in(cx, |view, _window, cx| view.apply_for_test(cx))
        .unwrap();
    cx.run_until_parked();

    let names = view.read_with(cx, |view, cx| view.column_names_for_test(cx));
    assert_eq!(names, ["id", "name", "payload"]);
}

#[gpui_kit::test]
fn adding_a_column_previews_and_applies_an_add_column_statement(cx: &mut TestAppContext) {
    let (_database, _handle, view) = schema_view(cx);
    cx.run_until_parked();

    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.add_column_for_test(window, cx);
            let index = view.column_names_for_test(cx).len() - 1;
            view.set_column_name_for_test(index, "note", window, cx);
            view.set_column_nullable_for_test(index, true, cx);
            view.preview_for_test(window, cx);
        })
        .unwrap();

    let statements = view
        .read_with(cx, |view, _| view.pending_statements_for_test())
        .expect("adding a named column should generate a statement");
    assert_eq!(statements.len(), 1);
    assert!(statements[0].starts_with("ALTER TABLE items ADD COLUMN note "));

    view.downgrade()
        .update_in(cx, |view, _window, cx| view.apply_for_test(cx))
        .unwrap();
    cx.run_until_parked();

    let names = view.read_with(cx, |view, cx| view.column_names_for_test(cx));
    assert_eq!(names, ["id", "name", "score", "payload", "note"]);
}

#[gpui_kit::test]
fn retyping_a_column_on_sqlite_is_refused_without_a_rebuild(cx: &mut TestAppContext) {
    let (_database, _handle, view) = schema_view(cx);
    cx.run_until_parked();

    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.set_column_type_for_test(2, "TEXT", cx);
            view.preview_for_test(window, cx);
        })
        .unwrap();

    let error = view
        .read_with(cx, |view, _| view.error_for_test())
        .expect("a retype should be refused on SQLite");
    assert!(error.contains("score") && error.contains("rebuilding"));
    assert!(
        view.read_with(cx, |view, _| view.pending_statements_for_test())
            .is_none()
    );
}

#[gpui_kit::test]
fn changing_a_columns_default_on_sqlite_is_refused_without_a_rebuild(cx: &mut TestAppContext) {
    let (_database, _handle, view) = schema_view(cx);
    cx.run_until_parked();

    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.set_column_default_for_test(2, "0", window, cx);
            view.preview_for_test(window, cx);
        })
        .unwrap();

    let error = view
        .read_with(cx, |view, _| view.error_for_test())
        .expect("a default change should be refused on SQLite");
    assert!(error.contains("score") && error.contains("rebuilding"));
    assert!(
        view.read_with(cx, |view, _| view.pending_statements_for_test())
            .is_none()
    );
}

#[gpui_kit::test]
fn previewing_with_nothing_changed_says_so(cx: &mut TestAppContext) {
    let (_database, _handle, view) = schema_view(cx);
    cx.run_until_parked();

    view.downgrade()
        .update_in(cx, |view, window, cx| view.preview_for_test(window, cx))
        .unwrap();

    assert_eq!(
        view.read_with(cx, |view, _| view.error_for_test()),
        Some("nothing has changed".to_string())
    );
    assert!(
        view.read_with(cx, |view, _| view.pending_statements_for_test())
            .is_none()
    );
}

#[gpui_kit::test]
fn a_read_only_connection_cannot_edit_columns(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_safety(cx, SafetyMode::ReadOnly);
    let object = DatabaseObject {
        schema: None,
        name: "items".into(),
        kind: ObjectKind::Table,
    };
    handle
        .update(cx, |session, window, cx| {
            session.open_schema_for_test(&object, window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    let view = handle
        .update(cx, |session, _, _| session.active_schema_view())
        .unwrap()
        .expect("opening a table's structure should open a schema view");

    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.set_column_name_for_test(1, "full_name", window, cx);
            view.preview_for_test(window, cx);
        })
        .unwrap();

    // `preview` refuses outright on a read-only connection, so there is
    // nothing waiting to be applied.
    assert!(
        view.read_with(cx, |view, _| view.pending_statements_for_test())
            .is_none()
    );
}

#[gpui_kit::test]
fn adding_an_index_previews_and_applies_a_create_index_statement(cx: &mut TestAppContext) {
    let (_database, _handle, view) = schema_view(cx);
    cx.run_until_parked();

    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.add_index_for_test(window, cx);
            view.set_index_name_for_test(0, "items_name_idx", window, cx);
            view.toggle_index_column_for_test(0, "name", true, cx);
            view.preview_for_test(window, cx);
        })
        .unwrap();

    assert_eq!(
        view.read_with(cx, |view, cx| view.index_names_for_test(cx)),
        ["items_name_idx"]
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.pending_statements_for_test()),
        Some(vec![
            "CREATE INDEX items_name_idx ON items (name)".to_string()
        ])
    );

    view.downgrade()
        .update_in(cx, |view, _window, cx| view.apply_for_test(cx))
        .unwrap();
    cx.run_until_parked();

    let schema = view
        .read_with(cx, |view, _| view.schema_for_test())
        .expect("the structure should have reloaded");
    let index = schema
        .indexes
        .iter()
        .find(|index| index.name == "items_name_idx")
        .expect("the new index should be listed after reload");
    assert_eq!(index.columns, ["name"]);
    assert!(!index.unique);
}

#[gpui_kit::test]
fn dropping_an_index_previews_and_applies_a_drop_index_statement(cx: &mut TestAppContext) {
    let (database, _handle, view) = schema_view(cx);
    cx.run_until_parked();
    run_external(&database, "CREATE INDEX items_name_idx ON items(name)");

    view.downgrade()
        .update_in(cx, |view, _window, cx| view.reload_for_test(cx))
        .unwrap();
    cx.run_until_parked();

    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.toggle_index_drop_for_test(0, cx);
            view.preview_for_test(window, cx);
        })
        .unwrap();
    assert_eq!(
        view.read_with(cx, |view, _| view.pending_statements_for_test()),
        Some(vec!["DROP INDEX items_name_idx".to_string()])
    );

    view.downgrade()
        .update_in(cx, |view, _window, cx| view.apply_for_test(cx))
        .unwrap();
    cx.run_until_parked();

    let schema = view
        .read_with(cx, |view, _| view.schema_for_test())
        .expect("the structure should have reloaded");
    assert!(
        schema
            .indexes
            .iter()
            .all(|index| index.name != "items_name_idx")
    );
}

#[gpui_kit::test]
fn adding_a_composite_unique_index_keeps_the_picked_column_order(cx: &mut TestAppContext) {
    let (_database, _handle, view) = schema_view(cx);
    cx.run_until_parked();

    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.add_index_for_test(window, cx);
            view.set_index_name_for_test(0, "items_score_name_key", window, cx);
            // Picked out of column order on purpose: the index has to keep
            // the order picked, not the table's own column order.
            view.toggle_index_column_for_test(0, "score", true, cx);
            view.toggle_index_column_for_test(0, "name", true, cx);
            view.set_index_unique_for_test(0, true, cx);
            view.preview_for_test(window, cx);
        })
        .unwrap();

    assert_eq!(
        view.read_with(cx, |view, _| view.pending_statements_for_test()),
        Some(vec![
            "CREATE UNIQUE INDEX items_score_name_key ON items (score, name)".to_string()
        ])
    );
}

#[gpui_kit::test]
fn adding_a_primary_key_index_on_sqlite_is_refused_without_a_rebuild(cx: &mut TestAppContext) {
    let (_database, _handle, view) = schema_view(cx);
    cx.run_until_parked();

    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.add_index_for_test(window, cx);
            view.set_index_name_for_test(0, "items_pkey2", window, cx);
            view.toggle_index_column_for_test(0, "score", true, cx);
            view.set_index_primary_key_for_test(0, true, cx);
            view.preview_for_test(window, cx);
        })
        .unwrap();

    let error = view
        .read_with(cx, |view, _| view.error_for_test())
        .expect("adding a primary key should be refused on SQLite");
    assert!(error.contains("rebuilding"));
    assert!(
        view.read_with(cx, |view, _| view.pending_statements_for_test())
            .is_none()
    );
}

#[gpui_kit::test]
fn a_tables_data_and_structure_are_different_tabs(cx: &mut TestAppContext) {
    let (_database, handle, _view) = schema_view(cx);

    let object = DatabaseObject {
        schema: None,
        name: "items".into(),
        kind: ObjectKind::Table,
    };
    handle
        .update(cx, |session, window, cx| {
            session.open_object(&object, ObjectViewMode::Data, window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    assert_eq!(
        handle
            .update(cx, |session, _, _| session.tab_titles())
            .unwrap(),
        ["Query 1", "items — Structure", "items"]
    );
}
