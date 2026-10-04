//! Completion in the query editor: what the catalog offers, and the menu.

use super::*;

/// The labels the active editor offers with `sql` in it and the caret at
/// `cursor`.
fn offered(
    cx: &mut TestAppContext,
    handle: WindowHandle<Session>,
    sql: &str,
    cursor: usize,
) -> Vec<String> {
    prepare_editor(cx, handle, sql, cursor);
    handle
        .update(cx, |session, _, cx| {
            session
                .active_editor_for_test(cx)
                .expect("a query tab has an editor")
                .read(cx)
                .completions_for_test(cx)
        })
        .unwrap()
}

#[gpui_kit::test]
fn the_catalog_reaches_an_editor_opened_before_it_loaded(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    // The query tab is already open; the catalog lands after it.
    handle
        .update(cx, |session, _, cx| session.refresh(cx))
        .unwrap();
    cx.run_until_parked();

    let sql = "select sc from items";
    assert_eq!(offered(cx, handle, sql, "select sc".len()), ["score"]);
    assert_eq!(
        offered(cx, handle, "select * from it", "select * from it".len()),
        ["items"]
    );
}

#[gpui_kit::test]
fn typing_opens_the_menu_and_enter_takes_the_suggestion(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    handle
        .update(cx, |session, _, cx| session.refresh(cx))
        .unwrap();
    cx.run_until_parked();

    prepare_editor(cx, handle, "select  from items", "select ".len());
    for letter in ["s", "c"] {
        cx.update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            window.input(letter, cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
    press(cx, handle, "enter");
    cx.run_until_parked();

    let sql = handle
        .update(cx, |session, _, cx| {
            session
                .active_editor_for_test(cx)
                .expect("a query tab has an editor")
                .read(cx)
                .sql(cx)
        })
        .unwrap();
    assert_eq!(sql, "select score from items");
}
