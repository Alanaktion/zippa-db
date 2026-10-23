//! Charting a query result: the view switch, its defaults, and its return
//! to the grid.

use super::*;
use crate::ui::session::chart::{ChartKind, ChartSummary};

#[gpui_kit::test]
fn a_numeric_result_charts_with_sensible_defaults(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    run_sql(cx, handle, "select id, name, score from items;");
    assert!(
        !handle
            .update(cx, |session, _, cx| session.chart_showing_for_test(cx))
            .unwrap(),
        "the grid shows first, not the chart"
    );

    handle
        .update(cx, |session, _, cx| session.pick_chart_for_test(true, cx))
        .unwrap();
    assert!(
        handle
            .update(cx, |session, _, cx| session.chart_showing_for_test(cx))
            .unwrap(),
        "the chart shows once picked"
    );
    // The first numeric column is y, the first other column is x.
    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.chart_for_test(cx))
            .unwrap(),
        Some(ChartSummary {
            kind: "Line".to_string(),
            x: Some("name".to_string()),
            y: vec!["id".to_string()],
            points: 2,
            skipped: 0,
        }),
        "the chart should default to name against id"
    );
    draw(cx, handle);
}

#[gpui_kit::test]
fn rows_that_do_not_read_are_skipped_not_plotted(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // The second row's score is NULL.
    run_sql(cx, handle, "select name, score from items;");
    handle
        .update(cx, |session, _, cx| session.pick_chart_for_test(true, cx))
        .unwrap();
    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.chart_for_test(cx))
            .unwrap(),
        Some(ChartSummary {
            kind: "Line".to_string(),
            x: Some("name".to_string()),
            y: vec!["score".to_string()],
            points: 1,
            skipped: 1,
        }),
        "one point plotted, one row skipped"
    );
    draw(cx, handle);
}

#[gpui_kit::test]
fn a_result_without_numbers_has_no_chart(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    run_sql(cx, handle, "select name from items;");
    handle
        .update(cx, |session, _, cx| session.pick_chart_for_test(true, cx))
        .unwrap();
    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.chart_for_test(cx))
            .unwrap(),
        None,
        "no numeric column means no chart, whatever the switch says"
    );
    draw(cx, handle);
}

#[gpui_kit::test]
fn several_series_share_one_x_domain(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // Both columns are complete, so both series plot every row.
    run_sql(cx, handle, "select id, id * 2 as doubled from items;");
    handle
        .update(cx, |session, _, cx| session.pick_chart_for_test(true, cx))
        .unwrap();
    handle
        .update(cx, |session, _, cx| session.toggle_chart_y_for_test(1, cx))
        .unwrap();
    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.chart_for_test(cx))
            .unwrap(),
        Some(ChartSummary {
            kind: "Line".to_string(),
            x: Some("doubled".to_string()),
            y: vec!["id".to_string(), "doubled".to_string()],
            points: 2,
            skipped: 0,
        }),
        "two series, two points each, nothing skipped"
    );
    draw(cx, handle);
}

#[gpui_kit::test]
fn a_row_missing_one_series_is_skipped_for_all(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    // The second row's score is NULL, so it cannot join the shared x
    // domain: it is skipped for id too, not just for score.
    run_sql(cx, handle, "select id, name, score from items;");
    handle
        .update(cx, |session, _, cx| session.pick_chart_for_test(true, cx))
        .unwrap();
    handle
        .update(cx, |session, _, cx| session.toggle_chart_y_for_test(2, cx))
        .unwrap();
    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.chart_for_test(cx))
            .unwrap(),
        Some(ChartSummary {
            kind: "Line".to_string(),
            x: Some("name".to_string()),
            y: vec!["id".to_string(), "score".to_string()],
            points: 1,
            skipped: 1,
        }),
        "the incomplete row is skipped for both series, counted once"
    );
    draw(cx, handle);
}

#[gpui_kit::test]
fn bars_give_way_to_lines_with_a_second_series(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    run_sql(cx, handle, "select id, name, score from items;");
    handle
        .update(cx, |session, _, cx| session.pick_chart_for_test(true, cx))
        .unwrap();
    handle
        .update(cx, |session, _, cx| {
            session.set_chart_kind_for_test(ChartKind::Bar, cx)
        })
        .unwrap();
    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.chart_for_test(cx))
            .unwrap()
            .map(|summary| summary.kind),
        Some("Bar".to_string()),
        "one series bars fine"
    );
    draw(cx, handle);
    // Bars chart one column: a second series turns the chart into lines.
    handle
        .update(cx, |session, _, cx| session.toggle_chart_y_for_test(2, cx))
        .unwrap();
    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.chart_for_test(cx))
            .unwrap(),
        Some(ChartSummary {
            kind: "Line".to_string(),
            x: Some("name".to_string()),
            y: vec!["id".to_string(), "score".to_string()],
            points: 1,
            skipped: 1,
        }),
        "the second series turns bars into lines"
    );
    draw(cx, handle);
}

#[gpui_kit::test]
fn an_area_chart_keeps_its_kind_with_several_series(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    run_sql(cx, handle, "select id, name, score from items;");
    handle
        .update(cx, |session, _, cx| session.pick_chart_for_test(true, cx))
        .unwrap();
    handle
        .update(cx, |session, _, cx| {
            session.set_chart_kind_for_test(ChartKind::Area, cx)
        })
        .unwrap();
    handle
        .update(cx, |session, _, cx| session.toggle_chart_y_for_test(2, cx))
        .unwrap();
    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.chart_for_test(cx))
            .unwrap(),
        Some(ChartSummary {
            kind: "Area".to_string(),
            x: Some("name".to_string()),
            y: vec!["id".to_string(), "score".to_string()],
            points: 1,
            skipped: 1,
        }),
        "areas chart several series, so the kind stands"
    );
    draw(cx, handle);
}

#[gpui_kit::test]
fn a_new_run_returns_to_the_grid(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    run_sql(cx, handle, "select id, name, score from items;");
    handle
        .update(cx, |session, _, cx| session.pick_chart_for_test(true, cx))
        .unwrap();
    assert!(
        handle
            .update(cx, |session, _, cx| session.chart_showing_for_test(cx))
            .unwrap()
    );

    run_sql(cx, handle, "select id from items;");
    assert!(
        !handle
            .update(cx, |session, _, cx| session.chart_showing_for_test(cx))
            .unwrap(),
        "a new run is what the user asked to see, so the grid comes back"
    );
}
