//! The read-only structure tab: columns, indexes, and foreign keys.

use super::*;
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
