//! The filter bar above a table view.

use super::*;

#[gpui_kit::test]
fn a_filter_narrows_the_rows_the_table_shows(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    add_filter(cx, &view, "name", Operator::Equals, "alpha");

    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where name = ? limit 500 offset 0",
            "the value belongs in a parameter, not in the statement"
        );
        assert_eq!(view.loaded_rows_for_test(), 1, "only one row says alpha");
    });
}

#[gpui_kit::test]
fn filters_cover_the_operators_they_offer(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // `IS NULL` has no value to bind, so nothing is added to the statement.
    add_filter(cx, &view, "name", Operator::IsNull, "");
    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where name is null limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1, "one row has no name");
    });

    let filters = view.read_with(cx, |view, _| view.filters_for_test());
    filters.update(cx, |filters, cx| filters.clear(cx));
    cx.run_until_parked();

    // A pattern is text whatever the column holds, so the column is cast.
    add_filter(cx, &view, "id", Operator::Like, "1%");
    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where cast(id as text) like ? limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1);
    });

    filters.update(cx, |filters, cx| filters.clear(cx));
    cx.run_until_parked();

    add_filter(cx, &view, "id", Operator::GreaterOrEqual, "2");
    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where id >= ? limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1);
    });
}

#[gpui_kit::test]
fn an_in_filter_takes_a_subquery(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // `IN` and `NOT IN` are the one place a value is SQL: it goes into the
    // statement as written so a subquery can do the filtering.
    add_filter(
        cx,
        &view,
        "id",
        Operator::In,
        "select id from items where name is not null",
    );

    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where id in (select id from items where name is not null) \
             limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1, "one row has a name");
        assert_eq!(view.error_for_test(), None);
    });
}

#[gpui_kit::test]
fn several_filters_are_joined_with_and(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    add_filter(cx, &view, "id", Operator::Greater, "0");
    add_filter(cx, &view, "name", Operator::NotEquals, "alpha");

    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where id > ? and name <> ? limit 500 offset 0"
        );
        // The row with a NULL name is not `<> 'alpha'` — SQL says nothing
        // about it — so neither row comes back.
        assert_eq!(view.loaded_rows_for_test(), 0);
    });
}

#[gpui_kit::test]
fn an_unfinished_filter_narrows_nothing(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    // A line with no value typed into it yet is still being written.
    add_filter(cx, &view, "name", Operator::Equals, "");

    view.update(cx, |view, cx| {
        assert_eq!(view.query(cx), "select * from items limit 500 offset 0");
        assert_eq!(view.loaded_rows_for_test(), 2);
    });

    let filters = view.read_with(cx, |view, _| view.filters_for_test());
    filters.read_with(cx, |filters, cx| {
        assert_eq!(
            filters.filter_count_for_test(),
            1,
            "the line is still there"
        );
        assert!(
            filters.shows_value_for_test(0, cx),
            "a filter that needs a value should show the box for it"
        );
    });
}

#[gpui_kit::test]
fn a_null_filter_hides_its_value_box(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    add_filter(cx, &view, "name", Operator::IsNotNull, "");

    let filters = view.read_with(cx, |view, _| view.filters_for_test());
    filters.read_with(cx, |filters, cx| {
        assert!(
            !filters.shows_value_for_test(0, cx),
            "IS NOT NULL has nothing to compare against"
        );
    });
}

#[gpui_kit::test]
fn setting_a_filter_replaces_whatever_was_there(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();

    add_filter(cx, &view, "id", Operator::Greater, "0");
    add_filter(cx, &view, "name", Operator::NotEquals, "alpha");

    // A programmatic jump — a foreign key follow, say — replaces the filters
    // rather than AND-ing onto whatever was there, the way `add_filter` does.
    let filters = view.read_with(cx, |view, _| view.filters_for_test());
    filters
        .downgrade()
        .update_in(cx, |filters, window, cx| {
            filters.set_filter("name", Operator::Equals, "alpha", window, cx);
        })
        .unwrap();
    cx.run_until_parked();

    filters.read_with(cx, |filters, _cx| {
        assert_eq!(
            filters.filter_count_for_test(),
            1,
            "set_filter should replace every existing row, not add to them"
        );
    });
    view.update(cx, |view, cx| {
        assert_eq!(
            view.query(cx),
            "select * from items where name = ? limit 500 offset 0"
        );
        assert_eq!(view.loaded_rows_for_test(), 1);
    });
}

#[gpui_kit::test]
fn filtering_asks_before_it_discards_staged_edits(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");
    add_filter(cx, &view, "name", Operator::Equals, "alpha");

    view.update(cx, |view, cx| {
        assert!(view.pending_for_test(), "filtering should be held back");
        assert_eq!(view.grid_for_test().read(cx).staged(cx).len(), 1);
    });

    click_in_session(cx, handle, "discard-and-continue");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.loaded_rows_for_test(),
            1,
            "the filter should be in force"
        );
        assert!(view.grid_for_test().read(cx).staged(cx).is_empty());
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "a discarded edit must not reach the server"
    );
}
