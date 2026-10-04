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
    let (_database, handle) = session_accepting_with(cx, settings::CompletionKey::Enter);

    prepare_editor(cx, handle, "select  from items", "select ".len());
    type_letters(cx, handle, &["s", "c"]);
    press(cx, handle, "enter");
    cx.run_until_parked();

    assert_eq!(active_sql(cx, handle), "select score from items");
}

/// Type each of `letters` into the active editor, letting the menu answer.
fn type_letters(cx: &mut TestAppContext, handle: WindowHandle<Session>, letters: &[&str]) {
    for letter in letters {
        cx.update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            window.input(letter, cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
}

fn active_sql(cx: &mut TestAppContext, handle: WindowHandle<Session>) -> String {
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

/// A session with its catalog loaded and suggestions taken with `key`.
fn session_accepting_with(
    cx: &mut TestAppContext,
    key: settings::CompletionKey,
) -> (TempDatabase, WindowHandle<Session>) {
    let (database, handle) = session_with_objects(cx);
    cx.update(|cx| settings::update(cx, |settings| settings.accept_completion = key));
    handle
        .update(cx, |session, _, cx| session.refresh(cx))
        .unwrap();
    cx.run_until_parked();
    (database, handle)
}

#[gpui_kit::test]
fn tab_takes_the_suggestion_when_the_setting_says_so(cx: &mut TestAppContext) {
    let (_database, handle) = session_accepting_with(cx, settings::CompletionKey::Tab);

    prepare_editor(cx, handle, "select  from items", "select ".len());
    type_letters(cx, handle, &["s", "c"]);
    press(cx, handle, "tab");
    cx.run_until_parked();

    assert_eq!(active_sql(cx, handle), "select score from items");
}

#[gpui_kit::test]
fn enter_starts_a_line_when_tab_takes_the_suggestion(cx: &mut TestAppContext) {
    let (_database, handle) = session_accepting_with(cx, settings::CompletionKey::Tab);

    prepare_editor(cx, handle, "select  from items", "select ".len());
    type_letters(cx, handle, &["s", "c"]);
    press(cx, handle, "enter");
    cx.run_until_parked();

    let sql = active_sql(cx, handle);
    assert!(
        sql.starts_with("select sc\n"),
        "Enter took a suggestion: {sql:?}"
    );
}

#[gpui_kit::test]
fn tab_indents_when_enter_takes_the_suggestion(cx: &mut TestAppContext) {
    let (_database, handle) = session_accepting_with(cx, settings::CompletionKey::Enter);

    prepare_editor(cx, handle, "select  from items", "select ".len());
    type_letters(cx, handle, &["s", "c"]);
    press(cx, handle, "tab");
    cx.run_until_parked();

    let sql = active_sql(cx, handle);
    assert!(!sql.contains("score"), "Tab took a suggestion: {sql:?}");
}

/// Press each of `keys` in the active editor, letting the menu answer.
fn press_keys(cx: &mut TestAppContext, handle: WindowHandle<Session>, keys: &[&str]) {
    for key in keys {
        press(cx, handle, key);
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn completion_works_back_on_an_earlier_line(cx: &mut TestAppContext) {
    let (_database, handle) = session_accepting_with(cx, settings::CompletionKey::Enter);

    // Write the FROM first, completing in it, then go back to the SELECT
    // list: the menu must still open there.
    prepare_editor(cx, handle, "select \nfrom ", "select \nfrom ".len());
    press_keys(cx, handle, &["i", "t", "enter"]);
    assert_eq!(active_sql(cx, handle), "select \nfrom items");

    handle
        .update(cx, |session, _, cx| {
            if let Some(editor) = session.active_editor_for_test(cx) {
                editor.update(cx, |editor, cx| {
                    editor.set_cursor_for_test("select ".len(), cx)
                });
            }
        })
        .unwrap();
    press_keys(cx, handle, &["s", "c", "enter"]);

    assert_eq!(active_sql(cx, handle), "select score\nfrom items");
}
