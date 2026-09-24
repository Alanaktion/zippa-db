//! The SQLite maintenance tab: its buttons run `Connection::run_maintenance`
//! (covered in `db::tests`) and this shows what came back in the grid.

use super::*;
use crate::db::Maintenance;

#[gpui_kit::test]
fn the_maintenance_tab_opens_once_and_is_reused(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    for _ in 0..2 {
        handle
            .update(cx, |session, window, cx| {
                session.open_maintenance_for_test(window, cx)
            })
            .unwrap();
    }

    let titles = handle
        .update(cx, |session, _, cx| session.tab_titles(cx))
        .unwrap();
    assert_eq!(
        titles
            .iter()
            .filter(|title| *title == "Maintenance")
            .count(),
        1
    );
    assert!(
        handle
            .update(cx, |session, _, cx| session.active_maintenance_view(cx))
            .unwrap()
            .is_some()
    );
}

#[gpui_kit::test]
fn an_integrity_check_reports_ok_in_the_grid(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    handle
        .update(cx, |session, window, cx| {
            session.open_maintenance_for_test(window, cx)
        })
        .unwrap();
    let view = handle
        .update(cx, |session, _, cx| session.active_maintenance_view(cx))
        .unwrap()
        .expect("the active tab should be the maintenance tab");

    view.update(cx, |view, cx| {
        view.run_for_test(Maintenance::IntegrityCheck, cx)
    });
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    let values = grid.read_with(cx, |grid, cx| grid.column_values_for_test(0, cx));
    assert_eq!(values, vec![Some("ok".to_string())]);
    view.read_with(cx, |view, _| {
        let (notice, error) = view.report_for_test();
        assert!(notice.is_some_and(|notice| notice.starts_with("Integrity Check")));
        assert!(error.is_none());
    });
}

#[gpui_kit::test]
fn vacuum_reports_that_it_finished_and_a_read_only_connection_refuses_it(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    handle
        .update(cx, |session, window, cx| {
            session.open_maintenance_for_test(window, cx)
        })
        .unwrap();
    let view = handle
        .update(cx, |session, _, cx| session.active_maintenance_view(cx))
        .unwrap()
        .expect("the active tab should be the maintenance tab");

    view.update(cx, |view, cx| view.run_for_test(Maintenance::Vacuum, cx));
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        let (notice, error) = view.report_for_test();
        assert!(notice.is_some_and(|notice| notice.starts_with("Vacuum finished")));
        assert!(error.is_none());
    });

    let (_database, handle) = session_with_safety(cx, SafetyMode::ReadOnly);
    handle
        .update(cx, |session, window, cx| {
            session.open_maintenance_for_test(window, cx)
        })
        .unwrap();
    let view = handle
        .update(cx, |session, _, cx| session.active_maintenance_view(cx))
        .unwrap()
        .expect("the active tab should be the maintenance tab");
    view.update(cx, |view, cx| view.run_for_test(Maintenance::Vacuum, cx));
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        let (_, error) = view.report_for_test();
        assert!(error.is_some_and(|error| error.contains("read-only")));
    });
}
