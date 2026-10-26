//! Custom dashboards: opening one runs its widgets, each widget keeps its
//! own state, and the sidebar's Saved tab lists them beside saved queries.

use super::*;
use crate::dashboards::{self, Dashboard, Dashboards, Widget, WidgetDisplay};
use crate::ui::session::chart::ChartKind;
use crate::ui::session::dashboard::WidgetSummary;

fn grid_widget(sql: &str) -> Widget {
    Widget {
        sql: sql.to_string(),
        display: WidgetDisplay::Grid,
    }
}

fn chart_widget(sql: &str, kind: ChartKind, x: Option<&str>, y: &[&str]) -> Widget {
    Widget {
        sql: sql.to_string(),
        display: WidgetDisplay::Chart {
            kind,
            x: x.map(str::to_string),
            y: y.iter().map(|name| name.to_string()).collect(),
        },
    }
}

fn dashboard(name: &str, widgets: Vec<Widget>, variables: &[(&str, &str)]) -> Dashboard {
    Dashboard {
        name: name.to_string(),
        widgets,
        variables: variables
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect(),
    }
}

/// A session with `dashboards` already saved, the way the Saved tab would
/// list them.
fn dashboard_session(
    cx: &mut TestAppContext,
    dashboards: Vec<Dashboard>,
) -> (TempDatabase, WindowHandle<Session>) {
    dashboards::reset_for_test();
    cx.update(|cx| {
        for dashboard in &dashboards {
            dashboards::upsert(dashboard, cx);
        }
    });
    cx.run_until_parked();
    session_with_objects(cx)
}

fn open_dashboard(cx: &mut TestAppContext, handle: WindowHandle<Session>, name: &str) {
    handle
        .update(cx, |session, window, cx| {
            session.open_dashboard(name.to_string(), window, cx)
        })
        .unwrap();
    cx.run_until_parked();
}

fn summaries(cx: &mut TestAppContext, handle: WindowHandle<Session>) -> Vec<WidgetSummary> {
    handle
        .read_with(cx, |session, cx| session.dashboard_for_test(cx))
        .unwrap()
        .expect("the dashboard tab should be active")
}

#[gpui_kit::test]
fn opening_a_dashboard_runs_each_widget(cx: &mut TestAppContext) {
    let _lock = dashboards::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = dashboard_session(
        cx,
        vec![dashboard(
            "Ops",
            vec![
                grid_widget("select id, name from items;"),
                chart_widget(
                    "select name, score from items;",
                    ChartKind::Line,
                    Some("name"),
                    &["score"],
                ),
            ],
            &[],
        )],
    );
    open_dashboard(cx, handle, "Ops");
    let summaries = summaries(cx, handle);
    assert_eq!(summaries.len(), 2);
    // A lone SELECT goes out with the implicit limit.
    assert_eq!(summaries[0].status, "done");
    assert_eq!(summaries[0].rows, Some(2));
    assert!(summaries[0].applied_limit.is_some());
    // The NULL score reads as a skipped row, never as a zero point.
    assert_eq!(summaries[1].status, "done");
    let chart = summaries[1]
        .chart
        .as_ref()
        .expect("the chart widget charts");
    assert_eq!(chart.points, 1);
    assert_eq!(chart.skipped, 1);
    // Drawing the tab paints both a grid cell and a chart cell.
    draw(cx, handle);
    dashboards::reset_for_test();
}

#[gpui_kit::test]
fn a_query_with_its_own_limit_gets_no_implicit_limit(cx: &mut TestAppContext) {
    let _lock = dashboards::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = dashboard_session(
        cx,
        vec![dashboard(
            "Ops",
            vec![grid_widget("select id from items limit 10;")],
            &[],
        )],
    );
    open_dashboard(cx, handle, "Ops");
    let summaries = summaries(cx, handle);
    assert_eq!(summaries[0].status, "done");
    assert_eq!(summaries[0].applied_limit, None);
    dashboards::reset_for_test();
}

#[gpui_kit::test]
fn a_row_missing_any_picked_series_is_skipped_for_all_of_them(cx: &mut TestAppContext) {
    let _lock = dashboards::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = dashboard_session(
        cx,
        vec![dashboard(
            "Ops",
            vec![chart_widget(
                "select name, score, id from items;",
                ChartKind::Line,
                Some("name"),
                &["score", "id"],
            )],
            &[],
        )],
    );
    open_dashboard(cx, handle, "Ops");
    let summaries = summaries(cx, handle);
    let chart = summaries[0]
        .chart
        .as_ref()
        .expect("the chart widget charts");
    // The second row has an id but a NULL score: it is skipped for both
    // series, not plotted for the one it has.
    assert_eq!(chart.y, vec!["score".to_string(), "id".to_string()]);
    assert_eq!(chart.points, 1);
    assert_eq!(chart.skipped, 1);
    dashboards::reset_for_test();
}

#[gpui_kit::test]
fn a_variable_set_once_fans_out_to_every_widget(cx: &mut TestAppContext) {
    let _lock = dashboards::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = dashboard_session(
        cx,
        vec![dashboard(
            "Filtered",
            vec![
                grid_widget("select id, name from items where name = :who;"),
                grid_widget("select score from items where name = :who;"),
            ],
            &[("who", "alpha")],
        )],
    );
    open_dashboard(cx, handle, "Filtered");
    let summaries = summaries(cx, handle);
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].status, "done");
    assert_eq!(summaries[0].rows, Some(1));
    assert_eq!(summaries[1].status, "done");
    assert_eq!(summaries[1].rows, Some(1));
    dashboards::reset_for_test();
}

#[gpui_kit::test]
fn one_failing_widget_does_not_take_the_rest_down(cx: &mut TestAppContext) {
    let _lock = dashboards::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = dashboard_session(
        cx,
        vec![dashboard(
            "Ops",
            vec![
                grid_widget("select * from nope;"),
                grid_widget("select id from items;"),
            ],
            &[],
        )],
    );
    open_dashboard(cx, handle, "Ops");
    let summaries = summaries(cx, handle);
    assert!(
        summaries[0].status.starts_with("error:"),
        "status: {}",
        summaries[0].status
    );
    assert_eq!(summaries[1].status, "done");
    assert_eq!(summaries[1].rows, Some(2));
    draw(cx, handle);
    dashboards::reset_for_test();
}

#[gpui_kit::test]
fn a_chart_column_that_is_not_there_is_the_widgets_own_error(cx: &mut TestAppContext) {
    let _lock = dashboards::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = dashboard_session(
        cx,
        vec![dashboard(
            "Ops",
            vec![chart_widget(
                "select name, score from items;",
                ChartKind::Line,
                Some("name"),
                &["gone"],
            )],
            &[],
        )],
    );
    open_dashboard(cx, handle, "Ops");
    let summaries = summaries(cx, handle);
    // The query ran; only the chart pick is wrong, so the widget is done
    // with no chart and the missing name named, not a failed widget.
    assert_eq!(summaries[0].status, "done");
    assert!(summaries[0].chart.is_none());
    assert_eq!(summaries[0].chart_missing, vec!["gone".to_string()]);
    draw(cx, handle);
    dashboards::reset_for_test();
}

#[gpui_kit::test]
fn clicking_a_dashboard_in_the_saved_tab_opens_and_runs_it(cx: &mut TestAppContext) {
    let _lock = dashboards::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_database, handle) = dashboard_session(
        cx,
        vec![dashboard(
            "Ops",
            vec![grid_widget("select id from items;")],
            &[],
        )],
    );
    handle
        .update(cx, |session, _, cx| session.show_saved_tab_for_test(cx))
        .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click(("dashboard", 0usize), cx);
    })
    .unwrap();
    cx.run_until_parked();

    let titles = handle
        .read_with(cx, |session, cx| session.tab_titles(cx))
        .unwrap();
    assert!(
        titles.iter().any(|title| title == "Ops"),
        "titles: {titles:?}"
    );
    let summaries = summaries(cx, handle);
    assert_eq!(summaries[0].status, "done");
    dashboards::reset_for_test();
}

/// Saving variables from the dialog re-runs the dashboard with them.
///
/// The session is wrapped in gpui-kit's `Root`, the way the app opens its
/// windows: the dialog goes through `window.open_dialog`, which panics on
/// a bare `cx.open_window` window with no component root. The dialog is
/// opened through `cx.update_window` rather than `handle.update`: opening
/// it updates the `Root` itself, which the latter would still be borrowing.
#[gpui_kit::test]
fn saving_variables_from_the_dialog_re_runs_the_dashboard(cx: &mut TestAppContext) {
    let _lock = dashboards::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    dashboards::reset_for_test();
    cx.update(|cx| {
        dashboards::upsert(
            &dashboard(
                "Filtered",
                vec![grid_widget("select id, name from items where name = :who;")],
                &[],
            ),
            cx,
        );
    });
    cx.run_until_parked();

    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");
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
    let session = handle
        .read_with(cx, |root, _| root.view().clone().downcast::<Session>())
        .unwrap()
        .expect("the test window's root wraps a Session");

    // No variable set yet, so the widget errors naming it.
    cx.update_window(handle.into(), |_, window, cx| {
        session.update(cx, |session, cx| {
            session.open_dashboard("Filtered".to_string(), window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    let summaries = cx
        .update(|cx| session.read(cx).dashboard_for_test(cx))
        .expect("the dashboard tab should be active");
    assert_eq!(
        summaries[0].status, "error: no value for variable 'who'",
        "the missing variable should be named"
    );

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("dashboard-variables", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let dialog = cx
        .update(|cx| session.read(cx).dashboard_variables_dialog_for_test(cx))
        .expect("the variables dialog should be open");
    cx.update_window(handle.into(), |_, window, cx| {
        dialog.update(cx, |view, cx| {
            view.set_row_for_test(0, "who", "alpha", window, cx)
        });
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("variables-save", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let summaries = cx
        .update(|cx| session.read(cx).dashboard_for_test(cx))
        .expect("the dashboard tab should be active");
    assert_eq!(summaries[0].status, "done");
    assert_eq!(summaries[0].rows, Some(1));
    // The dialog's save persisted the value with the dashboard.
    let stored = cx.update(|cx| Dashboards::find(cx, "Filtered"));
    assert_eq!(
        stored
            .expect("the dashboard should still be saved")
            .variables
            .get("who")
            .map(String::as_str),
        Some("alpha")
    );
    dashboards::reset_for_test();
}

/// Saving variables while a refresh is still running re-runs the
/// dashboard with the new values: without the cancel first, `refresh`
/// returns early while `refreshing` and the widgets keep the results
/// of the snapshot the in-flight run took.
#[gpui_kit::test]
fn saving_variables_during_a_refresh_re_runs_with_the_new_values(cx: &mut TestAppContext) {
    let _lock = dashboards::FILE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    dashboards::reset_for_test();
    cx.update(|cx| {
        dashboards::upsert(
            &dashboard(
                "Filtered",
                vec![grid_widget("select id, name from items where name = :who;")],
                &[],
            ),
            cx,
        );
    });
    cx.run_until_parked();

    let database = runtime::block_on(TempDatabase::new());
    let connection = runtime::block_on(Connection::open(database.config(), None))
        .expect("could not open the test database");
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
    let session = handle
        .read_with(cx, |root, _| root.view().clone().downcast::<Session>())
        .unwrap()
        .expect("the test window's root wraps a Session");

    // No variable set yet, so the widget errors naming it.
    cx.update_window(handle.into(), |_, window, cx| {
        session.update(cx, |session, cx| {
            session.open_dashboard("Filtered".to_string(), window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    let summaries = cx
        .update(|cx| session.read(cx).dashboard_for_test(cx))
        .expect("the dashboard tab should be active");
    assert_eq!(
        summaries[0].status, "error: no value for variable 'who'",
        "the missing variable should be named"
    );

    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("dashboard-variables", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let dialog = cx
        .update(|cx| session.read(cx).dashboard_variables_dialog_for_test(cx))
        .expect("the variables dialog should be open");
    cx.update_window(handle.into(), |_, window, cx| {
        dialog.update(cx, |view, cx| {
            view.set_row_for_test(0, "who", "alpha", window, cx)
        });
    })
    .unwrap();
    // A refresh is still running when the save lands: its snapshot has
    // no value for `who`, so only a cancel-then-refresh re-runs with it.
    // Through `update`, not `read`: marking needs `cx` mutably, which a
    // read guard would still be borrowing.
    let marked = cx.update(|cx| {
        session.update(cx, |session, cx| {
            session.mark_dashboard_refreshing_for_test(cx)
        })
    });
    assert!(marked, "the dashboard tab should be active");
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
        window.click("variables-save", cx);
    })
    .unwrap();
    cx.run_until_parked();

    let summaries = cx
        .update(|cx| session.read(cx).dashboard_for_test(cx))
        .expect("the dashboard tab should be active");
    assert_eq!(summaries[0].status, "done");
    assert_eq!(summaries[0].rows, Some(1));
    dashboards::reset_for_test();
}
