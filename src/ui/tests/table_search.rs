//! The quick search across every column of a table.

use super::*;

#[gpui_kit::test]
fn a_search_matches_any_column(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    set_search(cx, &view, "alpha");

    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where (lower(cast(id as text)) like lower(?) escape '!' \
             or lower(cast(name as text)) like lower(?) escape '!' \
             or lower(cast(score as text)) like lower(?) escape '!' \
             or lower(cast(payload as text)) like lower(?) escape '!') \
             limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1, "only one row says alpha");
        assert_eq!(view.search_for_test(), "alpha");
    });
}

#[gpui_kit::test]
fn a_search_matches_a_number_column_by_its_spelling(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    set_search(cx, &view, "1.5");

    view.update(cx, |view, _cx| {
        assert_eq!(view.loaded_rows_for_test(), 1, "only the 1.5 row matches");
        assert_eq!(view.error_for_test(), None);
    });
}

#[gpui_kit::test]
fn a_search_is_case_insensitive(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    set_search(cx, &view, "ALPHA");

    view.update(cx, |view, _cx| {
        assert_eq!(view.loaded_rows_for_test(), 1, "ALPHA should find alpha");
    });
}

#[gpui_kit::test]
fn special_characters_are_bound_not_pasted_and_match_literally(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    set_search(cx, &view, "100%_!");

    view.update(cx, |view, cx| {
        let (sql, params) = view.query_with_params(cx);
        assert!(
            !sql.contains("100"),
            "the term belongs in a parameter, not in the statement: {sql}"
        );
        // `%` and `_` are escaped so they match literally, `!` is escaped
        // so it is not read as the escape character itself, and the whole
        // term is wrapped as a substring — once per column.
        assert_eq!(
            params,
            vec![
                Some("%100!%!_!!%".to_string()),
                Some("%100!%!_!!%".to_string()),
                Some("%100!%!_!!%".to_string()),
                Some("%100!%!_!!%".to_string()),
            ]
        );
        assert_eq!(view.loaded_rows_for_test(), 0, "nothing holds that term");
        assert_eq!(view.error_for_test(), None);
    });
}

#[gpui_kit::test]
fn an_empty_search_clears_the_clause(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    set_search(cx, &view, "alpha");
    view.update(cx, |view, _cx| {
        assert_eq!(view.loaded_rows_for_test(), 1);
    });

    set_search(cx, &view, "");
    view.update(cx, |view, cx| {
        assert_eq!(view.query(cx), "select * from items limit 500 offset 0");
        assert_eq!(view.loaded_rows_for_test(), 2, "both rows are back");
        assert_eq!(view.search_for_test(), "");
    });

    // Whitespace alone is empty too.
    set_search(cx, &view, "   ");
    view.update(cx, |view, cx| {
        assert_eq!(view.query(cx), "select * from items limit 500 offset 0");
    });
}

#[gpui_kit::test]
fn a_search_combines_with_existing_filters(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    add_filter(cx, &view, "id", Operator::Greater, "0");
    set_search(cx, &view, "alpha");

    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where id > ? and (lower(cast(id as text)) like lower(?) escape '!' \
             or lower(cast(name as text)) like lower(?) escape '!' \
             or lower(cast(score as text)) like lower(?) escape '!' \
             or lower(cast(payload as text)) like lower(?) escape '!') \
             limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1);
    });
}

#[gpui_kit::test]
fn searching_asks_before_it_discards_staged_edits(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");
    set_search(cx, &view, "alpha");

    view.update(cx, |view, cx| {
        assert!(view.pending_for_test(), "searching should be held back");
        assert_eq!(view.grid_for_test().read(cx).staged(cx).len(), 1);
    });

    click_in_session(cx, handle, "discard-and-continue");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.loaded_rows_for_test(),
            1,
            "the search should be in force"
        );
        assert!(view.grid_for_test().read(cx).staged(cx).is_empty());
    });
}
