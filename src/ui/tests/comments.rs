//! Toggling SQL comments on the caret's line, or on a selection.

use super::*;

/// The active query tab's buffer.
fn buffer(cx: &mut TestAppContext, handle: WindowHandle<Session>) -> String {
    handle
        .update(cx, |session, _, cx| {
            session
                .active_editor_for_test(cx)
                .expect("a query tab has an editor")
                .read(cx)
                .sql(cx)
        })
        .unwrap()
}

#[gpui_kit::test]
fn the_comment_shortcut_comments_the_caret_line(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "select * from items", 6);
    press(cx, handle, "secondary-/");
    cx.run_until_parked();

    assert_eq!(buffer(cx, handle), "-- select * from items");
}

#[gpui_kit::test]
fn the_shortcut_takes_the_comment_back_off(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "-- select * from items", 6);
    press(cx, handle, "secondary-/");
    cx.run_until_parked();

    assert_eq!(buffer(cx, handle), "select * from items");
}

#[gpui_kit::test]
fn a_selection_comments_every_line_it_touches(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let sql = "select 1;\nselect 2;";
    prepare_editor(cx, handle, sql, 0);
    handle
        .update(cx, |session, _, cx| {
            session
                .active_editor_for_test(cx)
                .expect("a query tab has an editor")
                .update(cx, |editor, cx| editor.select_for_test(0..sql.len(), cx));
        })
        .unwrap();

    press(cx, handle, "secondary-/");
    cx.run_until_parked();

    assert_eq!(buffer(cx, handle), "-- select 1;\n-- select 2;");
}
