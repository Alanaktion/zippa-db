//! Sorting, on the server for a table and in place for a result.

use super::*;

#[gpui_kit::test]
fn sorting_the_table_view_reorders_on_the_server(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);

    view.update(cx, |view, cx| {
        view.go_for_test(3, cx);
        assert_eq!(view.page_for_test(), 3);

        view.sort_for_test("name", ColumnSort::Descending, cx);
        assert_eq!(
            view.query(cx),
            "select * from items order by name desc limit 500 offset 0",
            "sorting should ask the server for ordered rows"
        );
        assert_eq!(
            view.page_for_test(),
            0,
            "sorting reorders the whole table, so it starts from the first page"
        );

        view.sort_for_test("name", ColumnSort::Default, cx);
        assert_eq!(view.query(cx), "select * from items limit 500 offset 0");
    });
}

#[gpui_kit::test]
fn sorting_a_query_result_reorders_the_rows_in_place(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(
                QueryResult {
                    columns: vec!["score".into(), "name".into()],
                    rows: vec![
                        vec![Some("10".into()), Some("ada".into())],
                        vec![Some("9".into()), Some("grace".into())],
                        vec![None, Some("unknown".into())],
                        vec![Some("100".into()), Some("alan".into())],
                    ],
                    ..QueryResult::default()
                },
                cx,
            );
        })
        .unwrap();

    let grid = handle
        .update(cx, |session, _, _| session.active_grid())
        .unwrap()
        .expect("a query tab should have a grid");

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);

        grid.update(cx, |grid, cx| {
            grid.sort_for_test(0, ColumnSort::Ascending, window, cx);
            assert_eq!(
                grid.column_values_for_test(0, cx),
                [
                    Some("9".to_string()),
                    Some("10".to_string()),
                    Some("100".to_string()),
                    None,
                ],
                "numbers should sort numerically, with NULLs last"
            );

            grid.sort_for_test(0, ColumnSort::Descending, window, cx);
            assert_eq!(
                grid.column_values_for_test(0, cx),
                [
                    None,
                    Some("100".to_string()),
                    Some("10".to_string()),
                    Some("9".to_string()),
                ]
            );

            grid.sort_for_test(1, ColumnSort::Ascending, window, cx);
            assert_eq!(
                grid.column_values_for_test(1, cx),
                [
                    Some("ada".to_string()),
                    Some("alan".to_string()),
                    Some("grace".to_string()),
                    Some("unknown".to_string()),
                ],
                "text should sort alphabetically"
            );
        });
    })
    .unwrap();
}

#[gpui_kit::test]
fn clicking_a_column_header_cycles_the_sort_and_comes_back(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_result_for_test(scores(), cx);
        })
        .unwrap();

    let grid = handle
        .update(cx, |session, _, _| session.active_grid())
        .unwrap()
        .expect("a query tab should have a grid");

    // The sort control sits at the right-hand end of the header cell. Column
    // 0 in the table is the row-pick column, so the first data column's header
    // is the one after it.
    let click_sort = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            let size = window.find(("col-header", 1usize)).bounds().size;
            window.click_at(
                ("col-header", 1usize),
                point(size.width - px(8.), size.height / 2.),
                cx,
            );
        })
        .unwrap();
    };
    let scores_shown =
        |cx: &mut TestAppContext| grid.update(cx, |grid, cx| grid.column_values_for_test(0, cx));

    click_sort(cx);
    assert_eq!(
        scores_shown(cx),
        [
            None,
            Some("100".to_string()),
            Some("10".to_string()),
            Some("9".to_string()),
        ],
        "the first click should sort descending"
    );

    click_sort(cx);
    assert_eq!(
        scores_shown(cx),
        [
            Some("9".to_string()),
            Some("10".to_string()),
            Some("100".to_string()),
            None,
        ],
        "the second click should sort ascending"
    );

    click_sort(cx);
    assert_eq!(
        scores_shown(cx),
        [
            Some("10".to_string()),
            Some("9".to_string()),
            None,
            Some("100".to_string()),
        ],
        "the third click should put the rows back in the order they arrived"
    );
}

#[gpui_kit::test]
fn the_table_view_keeps_its_sort_when_the_page_reloads(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);

    view.update(cx, |view, cx| {
        view.sort_for_test("name", ColumnSort::Descending, cx)
    });
    cx.run_until_parked();

    let sorted = view.update(cx, |view, cx| {
        let grid = view.grid_for_test();
        let grid = grid.read(cx);
        grid.sorted_for_test(cx)
    });

    assert_eq!(
        sorted,
        Some(("name".to_string(), ColumnSort::Descending)),
        "the rows come back already sorted, so the header has to say so: \
         forgetting it restarts the cycle and every click sorts descending"
    );
}
