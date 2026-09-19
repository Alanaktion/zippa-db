//! Schema search: the catalog load, and what a result opens.

use super::*;
use crate::db::catalog::CatalogEntry;
use crate::db::{CatalogKind, StoredKind, StoredObject};

fn items() -> DatabaseObject {
    DatabaseObject {
        schema: None,
        name: "items".into(),
        kind: ObjectKind::Table,
    }
}

fn column(name: &str, type_name: &str) -> CatalogEntry {
    CatalogEntry::member(CatalogKind::Column, items(), name.into(), type_name.into())
}

#[gpui_kit::test]
fn the_catalog_lists_columns_and_indexes(cx: &mut TestAppContext) {
    let (database, handle) = session_with_objects(cx);
    run_external(&database, "CREATE INDEX items_name_idx ON items(name)");

    handle
        .update(cx, |session, _, cx| session.refresh(cx))
        .unwrap();
    cx.run_until_parked();

    let catalog = handle
        .update(cx, |session, _, _| session.catalog())
        .unwrap();
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.kind == CatalogKind::Column && entry.name == "score")
    );
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.kind == CatalogKind::Index && entry.name == "items_name_idx")
    );
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.kind == CatalogKind::Table && entry.name == "items")
    );
}

#[gpui_kit::test]
fn refresh_picks_up_a_new_column(cx: &mut TestAppContext) {
    let (database, handle) = session_with_objects(cx);
    run_external(&database, "ALTER TABLE items ADD COLUMN note TEXT");

    handle
        .update(cx, |session, _, cx| session.refresh(cx))
        .unwrap();
    cx.run_until_parked();

    let catalog = handle
        .update(cx, |session, _, _| session.catalog())
        .unwrap();
    assert!(
        catalog
            .entries
            .iter()
            .any(|entry| entry.kind == CatalogKind::Column && entry.name == "note")
    );
}

#[gpui_kit::test]
fn a_column_result_opens_the_table_and_selects_the_column(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // `items` is `id`, `name`, `score`, `payload`, so `score` is the third
    // value column. The selection is reported in the result's own columns, not
    // the shown ones, so that is index 2.
    let entry = column("score", "REAL");
    handle
        .update(cx, |session, window, cx| {
            session.open_catalog_entry(entry, window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let view = handle
        .update(cx, |session, _, cx| session.active_table_view(cx))
        .unwrap()
        .expect("a column result should open its table's data tab");
    let (row, cell) = view.read_with(cx, |view, cx| {
        view.grid_for_test().read(cx).selection_for_test(cx)
    });
    assert_eq!(row, Some(0));
    assert_eq!(cell, Some((0, 2)));
}

#[gpui_kit::test]
fn an_index_result_opens_the_structure_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let entry = CatalogEntry::member(
        CatalogKind::Index,
        items(),
        "items_name_idx".into(),
        "name".into(),
    );
    handle
        .update(cx, |session, window, cx| {
            session.open_catalog_entry(entry, window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    let schema = handle
        .update(cx, |session, _, cx| session.active_schema_view(cx))
        .unwrap();
    assert!(
        schema.is_some(),
        "an index result should open the table's structure"
    );
}

#[gpui_kit::test]
fn a_routine_result_copies_its_name(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let entry = CatalogEntry::routine(StoredObject {
        schema: None,
        name: "recalc".into(),
        arguments: Some("int".into()),
        kind: StoredKind::Function,
    });
    handle
        .update(cx, |session, window, cx| {
            session.open_catalog_entry(entry, window, cx)
        })
        .unwrap();

    assert_eq!(clipboard(cx).as_deref(), Some("recalc(int)"));
}

#[gpui_kit::test]
fn the_sidebar_button_opens_the_dialog(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);
    cx.run_until_parked();

    click(cx, &handle, "search-schema");
    cx.run_until_parked();
    // Draw again so the dialog's body — the chips, the list, the rows — is
    // really laid out, not merely queued.
    draw_workspace(cx, &handle);

    cx.update_window(handle.window.into(), |_, window, cx| {
        assert!(
            window.has_active_dialog(cx),
            "the schema search dialog should be open"
        );
    })
    .unwrap();
}
