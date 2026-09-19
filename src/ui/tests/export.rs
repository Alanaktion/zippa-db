//! Exporting a table, or the rows picked out of it, to a file.

use super::*;

#[gpui_kit::test]
fn exporting_a_table_writes_csv(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();
    let scratch = ScratchDir::new();
    let path = scratch.path.join("items.csv");

    export_table(cx, &view, Format::Csv, &path);

    // `payload` holds a blob the driver only described, so it is written empty
    // rather than as `<3 bytes>`.
    assert_eq!(
        std::fs::read_to_string(&path).expect("the export was not written"),
        "id,name,score,payload\n1,alpha,1.5,\n2,,,\n"
    );
    let notice = view
        .read_with(cx, |view, _| view.notice_for_test())
        .expect("the export should say what it did");
    assert!(notice.contains("Exported 2 rows"), "{notice}");
    assert!(
        notice.contains("1 value could not be read back"),
        "the stand-in should be reported: {notice}"
    );
}

#[gpui_kit::test]
fn exporting_a_table_writes_json_with_typed_values(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();
    let scratch = ScratchDir::new();
    let path = scratch.path.join("items.json");

    export_table(cx, &view, Format::Json, &path);

    assert_eq!(
        std::fs::read_to_string(&path).expect("the export was not written"),
        concat!(
            "[\n",
            "  {\n",
            "    \"id\": 1,\n",
            "    \"name\": \"alpha\",\n",
            "    \"score\": 1.5,\n",
            "    \"payload\": null\n",
            "  },\n",
            "  {\n",
            "    \"id\": 2,\n",
            "    \"name\": null,\n",
            "    \"score\": null,\n",
            "    \"payload\": null\n",
            "  }\n",
            "]"
        )
    );
}

#[gpui_kit::test]
fn exporting_a_table_writes_sql_inserts(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();
    let scratch = ScratchDir::new();
    let path = scratch.path.join("items.sql");

    export_table(cx, &view, Format::Sql, &path);

    assert_eq!(
        std::fs::read_to_string(&path).expect("the export was not written"),
        "insert into items (id, name, score, payload) values (1, 'alpha', 1.5, NULL);\n\
         insert into items (id, name, score, payload) values (2, NULL, NULL, NULL);\n"
    );
}

#[gpui_kit::test]
fn an_export_honours_the_filters_and_the_sort(cx: &mut TestAppContext) {
    let create = "CREATE TABLE scores (id INTEGER, name TEXT);\n\
         INSERT INTO scores VALUES (1, 'b'), (2, 'a'), (3, 'c');";
    let (_database, _handle, view) = table_view_on(cx, "scores", create);
    cx.run_until_parked();

    add_filter(cx, &view, "id", Operator::GreaterOrEqual, "2");
    view.update(cx, |view, cx| {
        view.sort_for_test("name", ColumnSort::Ascending, cx)
    });
    cx.run_until_parked();

    let scratch = ScratchDir::new();
    let path = scratch.path.join("scores.csv");
    export_table(cx, &view, Format::Csv, &path);

    // The page limit is left off, but the filter and the order are not.
    assert_eq!(
        std::fs::read_to_string(&path).expect("the export was not written"),
        "id,name\n2,a\n3,c\n"
    );
}

#[gpui_kit::test]
fn exporting_a_table_without_a_key_drops_the_row_id(cx: &mut TestAppContext) {
    // A table with no primary key is read as `select rowid, *`, and the row id
    // is an addressing detail, not a column of the table.
    let create = "CREATE TABLE logs (message TEXT);\n\
         INSERT INTO logs VALUES ('first'), ('second');";
    let (_database, _handle, view) = table_view_on(cx, "logs", create);
    cx.run_until_parked();

    let scratch = ScratchDir::new();
    let path = scratch.path.join("logs.csv");
    export_table(cx, &view, Format::Csv, &path);

    assert_eq!(
        std::fs::read_to_string(&path).expect("the export was not written"),
        "message\nfirst\nsecond\n"
    );
}

#[gpui_kit::test]
fn exporting_picked_rows_writes_only_them(cx: &mut TestAppContext) {
    let create = "CREATE TABLE people (id INTEGER, name TEXT);\n\
         INSERT INTO people VALUES (1, 'ada'), (2, 'grace'), (3, 'alan');";
    let (_database, handle, view) = table_view_on(cx, "people", create);
    cx.run_until_parked();

    pick_row(cx, handle, 1);

    let scratch = ScratchDir::new();
    let path = scratch.path.join("people.sql");
    export_picked_rows(cx, &view, Format::Sql, &path);

    assert_eq!(
        std::fs::read_to_string(&path).expect("the export was not written"),
        "insert into people (id, name) values (2, 'grace');\n"
    );
}

#[gpui_kit::test]
fn cancelling_the_save_dialog_writes_nothing(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    view.update(cx, |view, cx| view.export(Format::Csv, cx));
    cx.simulate_new_path_selection(|_| None);
    cx.run_until_parked();

    assert!(
        view.read_with(cx, |view, _| view.notice_for_test())
            .is_none(),
        "a cancelled dialog should leave no notice behind"
    );
}
