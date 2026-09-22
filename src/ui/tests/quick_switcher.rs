//! Quick switcher (`Cmd+K`) fuzzy command palette tests.

use super::*;
use gpui_kit::component::WindowExt;

use crate::ui::quick_switcher::SwitcherTarget;

#[gpui_kit::test]
fn quick_switcher_opens_via_toolbar_and_lists_tabs_and_objects(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    // Initial state: 1 query tab "Query 1"
    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("active session");

    // Open another query tab
    handle
        .update(cx, |_, window, cx| {
            session.update(cx, |session, cx| {
                session.open_tab(
                    Some("Custom Query".into()),
                    "SELECT 1;".into(),
                    false,
                    window,
                    cx,
                );
            });
        })
        .unwrap();

    // Click the quick switcher button in the titlebar
    click(cx, &handle, "quick-switcher");
    cx.run_until_parked();

    // Verify dialog is open on the window
    cx.update_window(handle.window.into(), |_, window, cx| {
        assert!(
            window.has_active_dialog(cx),
            "quick switcher dialog should be open"
        );
    })
    .unwrap();

    // Verify session contents available to switcher
    session.update(cx, |session, cx| {
        assert_eq!(session.panels().len(), 2);
        assert_eq!(session.panels()[0].read(cx).title(), "Query 1");
        assert_eq!(session.panels()[1].read(cx).title(), "Custom Query");
        assert!(session.objects().iter().any(|o| o.name == "items"));
    });
}

#[gpui_kit::test]
fn quick_switcher_opens_and_closes_dialog(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    // Click the quick switcher button in the titlebar
    click(cx, &handle, "quick-switcher");
    cx.run_until_parked();

    cx.update_window(handle.window.into(), |_, window, cx| {
        assert!(
            window.has_active_dialog(cx),
            "dialog should open after clicking toolbar button"
        );
        // Close dialog
        window.close_dialog(cx);
        assert!(!window.has_active_dialog(cx), "dialog should be closed");
    })
    .unwrap();
}

#[gpui_kit::test]
fn selecting_object_opens_table_tab(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("active session");

    // Simulate selecting an object from the palette.
    let view = switcher_view(cx, &handle, &session);
    let object = DatabaseObject {
        name: "items".into(),
        schema: None,
        kind: ObjectKind::Table,
    };
    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.choose_for_test(SwitcherTarget::Object(object), window, cx)
        })
        .unwrap();

    session.update(cx, |session, cx| {
        assert_eq!(session.panels().len(), 2);
        assert_eq!(session.panels()[1].read(cx).title(), "items");
        assert_eq!(session.active_tab_index(), 1);
    });
}

#[gpui_kit::test]
fn selecting_tab_switches_active_tab(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("active session");

    handle
        .update(cx, |_, window, cx| {
            session.update(cx, |session, cx| {
                session.open_tab(Some("Second Tab".into()), String::new(), false, window, cx);
                assert_eq!(session.active_tab_index(), 1);
            });
        })
        .unwrap();

    let view = switcher_view(cx, &handle, &session);
    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.choose_for_test(SwitcherTarget::Tab(0), window, cx)
        })
        .unwrap();

    assert_eq!(
        session.update(cx, |session, _| session.active_tab_index()),
        0,
        "the palette should bring the chosen tab to the front"
    );
}

#[gpui_kit::test]
fn confirming_an_item_from_the_dialog_drives_the_session(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("active session");
    // A second tab, so there is something for the first item to switch back to.
    handle
        .update(cx, |_, window, cx| {
            session.update(cx, |session, cx| {
                session.open_tab(Some("Second".into()), String::new(), false, window, cx)
            });
        })
        .unwrap();
    assert_eq!(
        session.update(cx, |session, _| session.active_tab_index()),
        1
    );

    // The whole path a user takes: open the palette, then Enter on the item
    // that is already selected.
    click(cx, &handle, "quick-switcher");
    cx.run_until_parked();
    assert!(
        workspace_dialog_open(cx, &handle),
        "the palette should open"
    );

    cx.update_window(handle.window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    assert!(
        !workspace_dialog_open(cx, &handle),
        "the palette should be gone once an item is chosen"
    );
    assert_eq!(
        session.update(cx, |session, _| session.active_tab_index()),
        0,
        "confirming the first item should bring the first tab to the front"
    );
}

#[gpui_kit::test]
fn the_new_query_tab_item_opens_a_tab(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("active session");
    assert_eq!(session.update(cx, |session, _| session.panels().len()), 1);

    let view = switcher_view(cx, &handle, &session);
    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.choose_for_test(SwitcherTarget::NewTab, window, cx)
        })
        .unwrap();

    assert_eq!(
        session.update(cx, |session, _| session.panels().len()),
        2,
        "choosing New Query Tab should open one"
    );
}

#[gpui_kit::test]
fn the_explain_item_reads_the_active_statement(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("active session");
    handle
        .update(cx, |_, window, cx| {
            session.update(cx, |session, cx| {
                session.prepare_active_editor_for_test("select * from items", window, cx)
            });
        })
        .unwrap();

    let view = switcher_view(cx, &handle, &session);
    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.choose_for_test(SwitcherTarget::Explain, window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    assert!(
        session.update(cx, |session, cx| session.active_plan_showing_for_test(cx)),
        "choosing Explain should read the plan the editor holds"
    );
}

#[gpui_kit::test]
fn the_schema_search_item_opens_the_dialog(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("active session");

    let view = switcher_view(cx, &handle, &session);
    view.downgrade()
        .update_in(cx, |view, window, cx| {
            view.choose_for_test(SwitcherTarget::SearchSchema, window, cx)
        })
        .unwrap();
    cx.run_until_parked();

    assert!(
        workspace_dialog_open(cx, &handle),
        "the schema search is a dialog of its own and should be open"
    );
}
