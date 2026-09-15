//! Reading and writing `.sql` files from a query tab.

use super::*;

#[gpui_kit::test]
fn opening_a_sql_file_puts_it_in_its_own_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let scratch = ScratchDir::new();
    let file = scratch.file("report.sql", "SELECT 1;\n");

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-filter", cx);
    })
    .unwrap();
    press(cx, handle, "secondary-o");

    cx.simulate_path_prompt_response(|options| {
        assert!(options.files, "the open dialog should offer files");
        assert!(
            !options.directories,
            "the open dialog should not offer directories"
        );
        Some(vec![file])
    });
    cx.run_until_parked();

    handle
        .update(cx, |session, _, cx| {
            assert_eq!(
                session.tab_titles(),
                ["Query 1", "report.sql"],
                "the file should open in a tab named after it"
            );
            assert_eq!(session.active_sql(cx), "SELECT 1;\n");
        })
        .unwrap();
}

#[gpui_kit::test]
fn cancelling_the_open_dialog_opens_nothing(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("object-filter", cx);
    })
    .unwrap();
    press(cx, handle, "secondary-o");

    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();

    handle
        .update(cx, |session, _, _| {
            assert_eq!(session.tab_titles(), ["Query 1"]);
        })
        .unwrap();
}

#[gpui_kit::test]
fn saving_a_tab_with_no_file_asks_where_to_put_it(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let scratch = ScratchDir::new();
    // No extension: saving one should add `.sql` for the user.
    let chosen = scratch.path.join("notes");

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("SELECT 2;", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-s");

    cx.simulate_new_path_selection({
        let chosen = chosen.clone();
        move |_directory| Some(chosen)
    });
    cx.run_until_parked();

    let saved = chosen.with_extension("sql");
    assert_eq!(
        std::fs::read_to_string(&saved).expect("the buffer was not written"),
        "SELECT 2;"
    );
    handle
        .update(cx, |session, _, _| {
            assert_eq!(
                session.tab_titles(),
                ["notes.sql"],
                "the tab should take the name of the file it was saved to"
            );
        })
        .unwrap();

    // Saving again goes straight to the same file.
    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("SELECT 3;", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-s");
    cx.run_until_parked();

    assert!(
        !cx.did_prompt_for_new_path(),
        "saving a tab that already has a file should not ask again"
    );
    assert_eq!(
        std::fs::read_to_string(&saved).expect("the buffer was not written"),
        "SELECT 3;"
    );
}

#[gpui_kit::test]
fn save_as_asks_again_and_follows_the_new_file(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let scratch = ScratchDir::new();
    let first = scratch.path.join("first.sql");
    let second = scratch.path.join("second.sql");

    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("SELECT 4;", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-s");
    cx.simulate_new_path_selection({
        let first = first.clone();
        move |_directory| Some(first)
    });
    cx.run_until_parked();

    press(cx, handle, "secondary-shift-s");
    assert!(
        cx.did_prompt_for_new_path(),
        "Save As should ask even when the tab already has a file"
    );
    cx.simulate_new_path_selection({
        let second = second.clone();
        move |_directory| Some(second)
    });
    cx.run_until_parked();

    assert_eq!(
        std::fs::read_to_string(&second).expect("the buffer was not written"),
        "SELECT 4;"
    );
    handle
        .update(cx, |session, _, _| {
            assert_eq!(session.tab_titles(), ["second.sql"]);
        })
        .unwrap();

    // The tab now follows the second file, so a plain save leaves the first.
    handle
        .update(cx, |session, window, cx| {
            session.prepare_active_editor_for_test("SELECT 5;", window, cx);
        })
        .unwrap();
    press(cx, handle, "secondary-s");
    cx.run_until_parked();

    assert_eq!(
        std::fs::read_to_string(&first).expect("the first file was not written"),
        "SELECT 4;"
    );
    assert_eq!(
        std::fs::read_to_string(&second).expect("the buffer was not written"),
        "SELECT 5;"
    );
}
