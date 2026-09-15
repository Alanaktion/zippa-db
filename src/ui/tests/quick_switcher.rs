//! Quick switcher (`Cmd+K`) fuzzy command palette tests.

use super::*;
use gpui_kit::component::WindowExt;

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
    session.update(cx, |session, _| {
        assert_eq!(session.tabs().len(), 2);
        assert_eq!(session.tabs()[0].title, "Query 1");
        assert_eq!(session.tabs()[1].title, "Custom Query");
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
fn quick_switcher_selecting_object_opens_table_tab(cx: &mut TestAppContext) {
    let handle = workspace(cx);
    let _database = connect(cx, &handle);

    let session = handle
        .update(cx, |workspace, _, _| workspace.active_session_for_test())
        .unwrap()
        .expect("active session");

    session.update(cx, |session, _| {
        assert_eq!(session.tabs().len(), 1);
    });

    let obj = DatabaseObject {
        name: "items".into(),
        schema: None,
        kind: ObjectKind::Table,
    };

    // Simulate selecting object from quick switcher
    handle
        .update(cx, |_, window, cx| {
            session.update(cx, |session, cx| {
                session.open_object(&obj, window, cx);
            });
        })
        .unwrap();

    session.update(cx, |session, _| {
        assert_eq!(session.tabs().len(), 2);
        assert_eq!(session.tabs()[1].title, "items");
        assert_eq!(session.active_tab_index(), 1);
    });
}

#[gpui_kit::test]
fn quick_switcher_selecting_tab_switches_active_tab(cx: &mut TestAppContext) {
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
                session.activate_tab(0, cx);
                assert_eq!(session.active_tab_index(), 0);
            });
        })
        .unwrap();
}
