//! Paging, refreshing and sorting a table view, and the edits they would lose.

use super::*;

#[gpui_kit::test]
fn the_table_view_pages_through_rows(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);

    view.update(cx, |view, cx| {
        assert_eq!(view.page_for_test(), 0);
        assert_eq!(view.limit_for_test(), 500);
        assert_eq!(view.query(cx), "select * from items limit 500 offset 0");

        // A short first page means there is nothing after it.
        view.set_loaded_rows_for_test(2);
        assert_eq!(view.can_page_for_test(), (false, false));

        // A full page suggests there is.
        view.set_loaded_rows_for_test(500);
        assert_eq!(view.can_page_for_test(), (false, true));

        view.go_for_test(1, cx);
        assert_eq!(view.page_for_test(), 1);
        assert_eq!(view.query(cx), "select * from items limit 500 offset 500");
        assert!(view.can_page_for_test().0, "page 2 can go back");
    });
}

#[gpui_kit::test]
fn paging_asks_before_it_discards_staged_edits(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");

    // A new page is a new set of rows, so the old ones' edits would go with
    // them; the footer asks instead of turning the page.
    view.update(cx, |view, cx| view.go_for_test(1, cx));
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(view.page_for_test(), 0, "the page should be held back");
        assert_eq!(
            view.grid_for_test().read(cx).staged(cx).len(),
            1,
            "the edit should still be there while the question stands"
        );
    });

    // Saying no leaves everything where it was.
    click_in_session(cx, handle, "keep-edits");
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        assert_eq!(view.page_for_test(), 0);
        assert_eq!(view.grid_for_test().read(cx).staged(cx).len(), 1);
    });

    // Saying yes throws the edit away and turns the page.
    view.update(cx, |view, cx| view.go_for_test(1, cx));
    cx.run_until_parked();
    click_in_session(cx, handle, "discard-and-continue");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(view.page_for_test(), 1, "the page should turn now");
        assert!(view.grid_for_test().read(cx).staged(cx).is_empty());
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "paging away from an edit must not write it"
    );
}

#[gpui_kit::test]
fn refresh_asks_before_it_discards_staged_edits(cx: &mut TestAppContext) {
    let (database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");

    handle
        .update(cx, |session, _, cx| session.refresh(cx))
        .unwrap();
    cx.run_until_parked();

    assert_eq!(
        view.read_with(cx, |view, cx| view
            .grid_for_test()
            .read(cx)
            .staged(cx)
            .len()),
        1,
        "refresh should ask rather than reread the rows under the edit"
    );

    click_in_session(cx, handle, "discard-and-continue");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert!(view.grid_for_test().read(cx).staged(cx).is_empty());
        assert_eq!(
            view.grid_for_test().read(cx).cell_for_test(0, 1, cx),
            Some("alpha".to_string()),
            "the page should be back to what the server has"
        );
    });
    assert_eq!(
        runtime::block_on(name_of(&database, 1)),
        Some("alpha".to_string()),
        "a refused edit must not reach the server"
    );
}

#[gpui_kit::test]
fn sorting_asks_and_puts_the_header_back_when_it_is_refused(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();

    stage_cell(cx, &view, 0, 1, "never written");

    view.update(cx, |view, cx| {
        view.sort_for_test("name", ColumnSort::Descending, cx)
    });
    cx.run_until_parked();
    assert!(
        view.read_with(cx, |view, _| view.pending_for_test()),
        "sorting should be held back while edits are waiting"
    );

    click_in_session(cx, handle, "keep-edits");
    cx.run_until_parked();

    view.update(cx, |view, cx| {
        assert_eq!(
            view.grid_for_test().read(cx).sorted_for_test(cx),
            None,
            "a refused sort should leave the header on the order the rows are in"
        );
        assert_eq!(view.grid_for_test().read(cx).staged(cx).len(), 1);
    });
}
