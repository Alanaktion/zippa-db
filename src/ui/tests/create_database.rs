//! The Create Database dialog: validation and the picker entry point.

use super::*;
use crate::ui::create_database_dialog::CreateDatabaseView;

/// An empty name never reaches the server: the dialog says so inline and
/// stays open.
#[gpui_kit::test]
fn an_empty_name_is_refused_with_an_inline_error(cx: &mut TestAppContext) {
    let database = runtime::block_on(TempDatabase::new());
    let connection = Arc::new(
        runtime::block_on(Connection::open(database.config(), None))
            .expect("could not open the test database"),
    );

    cx.update(|cx| {
        init_ui(cx);
        crate::keymap::bind(cx);
    });

    let view = cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), {
        let connection = connection.clone();
        move |window, cx| CreateDatabaseView::new(connection, window, cx)
    });
    cx.run_until_parked();

    // The name box starts empty; submitting reports the problem inline
    // rather than running anything.
    view.update(cx, |view, _, cx| view.submit_for_test(cx))
        .unwrap();
    cx.run_until_parked();

    let error = view
        .read_with(cx, |view, _| view.error_for_test())
        .expect("reading the dialog should work");
    assert_eq!(error.as_deref(), Some("Enter a name for the new database."));
}

/// The whole flow against a real Postgres: the dialog's `CREATE DATABASE`
/// runs, the dialog closes itself, and the new database is listed.
///
/// Needs a live Postgres server; see `db/tests.rs` for how to start one.
/// `cargo test -- --ignored live_`.
///
/// The session is wrapped in gpui-kit's `Root`, the way the app opens its
/// windows: the dialog goes through `window.open_dialog`, which panics on a
/// bare `cx.open_window` window with no component root. The dialog is opened
/// through `cx.update_window` rather than `handle.update`: opening it updates
/// the `Root` itself, which the latter would still be borrowing.
#[gpui_kit::test]
#[ignore = "needs a live Postgres server; see the doc comment"]
async fn live_postgres_create_database(cx: &mut TestAppContext) {
    let connection = runtime::block_on(crate::db::tests::live_postgres(
        "ZIPPA_TEST_POSTGRES_URL",
        "app",
    ));

    cx.update(|cx| {
        init_ui(cx);
        crate::keymap::bind(cx);
    });
    let handle = {
        let connection = Arc::new(connection);
        cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
            let session = cx.new(|cx| Session::new(connection, window, cx));
            Root::new(session, window, cx)
        })
    };
    cx.run_until_parked();

    let session = handle
        .read_with(cx, |root, _| root.view().clone().downcast::<Session>())
        .unwrap()
        .expect("the test window's root wraps a Session");

    let name = format!("zippa_create_db_{}", Uuid::new_v4().simple());

    // Open the dialog the way the picker menu does. `update_window` hands
    // over the window without holding the root view: opening the dialog
    // updates the `Root` itself, which a `handle.update` would still be
    // borrowing ("cannot read Root while it is already being updated").
    cx.update_window(handle.into(), |_, window, cx| {
        session.update(cx, |session, cx| {
            session.open_create_database_dialog(window, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();

    // Name the database through the dialog itself.
    let dialog = session.update(cx, |session, _| {
        session
            .create_database_dialog_for_test()
            .expect("the dialog should be open")
    });
    cx.update_window(handle.into(), |_, window, cx| {
        dialog.update(cx, |view, cx| {
            view.set_name_for_test(&name, window, cx);
        });
    })
    .unwrap();
    dialog.update(cx, |view, cx| view.submit_for_test(cx));
    cx.run_until_parked();

    // Success closes the dialog; the failure cases keep it open with the
    // error inline.
    let dialog_open = session.update(cx, |session, _| {
        session.create_database_dialog_for_test().is_some()
    });
    assert!(!dialog_open, "the dialog closes after a create");

    // The new database is really there, and goes away again.
    let check = runtime::block_on(crate::db::tests::live_postgres(
        "ZIPPA_TEST_POSTGRES_URL",
        "app",
    ));
    let databases = runtime::block_on(check.databases()).expect("re-reading databases failed");
    assert!(
        databases.iter().any(|database| database == &name),
        "the new database should be listed"
    );
    runtime::block_on(check.execute(&format!("DROP DATABASE {name}"), vec![]))
        .expect("dropping the test database failed");
    runtime::block_on(check.close());
}
