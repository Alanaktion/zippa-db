//! Charting a query result: the view switch, its defaults, and its return
//! to the grid.

use super::*;

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
        Some((
            "Line".to_string(),
            Some("name".to_string()),
            "id".to_string(),
            2,
            0
        )),
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
        Some((
            "Line".to_string(),
            Some("name".to_string()),
            "score".to_string(),
            1,
            1
        )),
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
