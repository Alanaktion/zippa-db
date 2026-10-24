//! The Messages pane: when it appears, what it lists, and when the grid
//! comes back.

use super::*;
use crate::db::query::{QueryMessage, QueryResult};

fn result_with_messages(messages: Vec<QueryMessage>) -> QueryResult {
    QueryResult {
        columns: vec!["id".into()],
        column_types: vec!["INTEGER".into()],
        rows: vec![vec![Some("1".into())]],
        messages,
        ..QueryResult::default()
    }
}

fn warning(code: &str, text: &str) -> QueryMessage {
    QueryMessage {
        level: "Warning".into(),
        code: Some(code.into()),
        text: text.into(),
    }
}

#[gpui_kit::test]
fn messages_appear_only_for_a_result_that_has_them(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_query_results_for_test(
                vec![result_with_messages(vec![
                    warning("1265", "Data truncated for column 'v'"),
                    warning("1365", "Division by 0"),
                ])],
                cx,
            )
        })
        .unwrap();
    assert!(
        !handle
            .update(cx, |session, _, cx| session.messages_showing_for_test(cx))
            .unwrap(),
        "the grid shows first, not the messages"
    );
    assert_eq!(
        handle
            .update(cx, |session, _, cx| session.shown_messages_for_test(cx))
            .unwrap(),
        vec![
            "Warning 1265: Data truncated for column 'v'",
            "Warning 1365: Division by 0",
        ],
        "the pane lists the result's messages in server order"
    );
    assert!(
        handle
            .update(cx, |session, _, cx| session.active_status_for_test(cx))
            .unwrap()
            .contains("2 messages"),
        "the status bar names the messages, or they stay hidden behind the grid"
    );

    handle
        .update(cx, |session, _, cx| {
            session.pick_messages_for_test(true, cx)
        })
        .unwrap();
    assert!(
        handle
            .update(cx, |session, _, cx| session.messages_showing_for_test(cx))
            .unwrap(),
        "the messages show once picked"
    );
    draw(cx, handle);

    // A result with no messages has no messages pane to show.
    handle
        .update(cx, |session, _, cx| {
            session.show_query_results_for_test(vec![result_with_messages(vec![])], cx)
        })
        .unwrap();
    assert!(
        !handle
            .update(cx, |session, _, cx| session.messages_showing_for_test(cx))
            .unwrap(),
        "a new run returns to the grid"
    );
    assert!(
        handle
            .update(cx, |session, _, cx| session.shown_messages_for_test(cx))
            .unwrap()
            .is_empty(),
        "a result without messages lists none"
    );
    draw(cx, handle);
}

#[gpui_kit::test]
fn a_new_run_returns_to_the_grid_from_messages(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_query_results_for_test(
                vec![result_with_messages(vec![warning(
                    "1265",
                    "Data truncated for column 'v'",
                )])],
                cx,
            )
        })
        .unwrap();
    handle
        .update(cx, |session, _, cx| {
            session.pick_messages_for_test(true, cx)
        })
        .unwrap();
    assert!(
        handle
            .update(cx, |session, _, cx| session.messages_showing_for_test(cx))
            .unwrap()
    );

    run_sql(cx, handle, "select id from items;");
    assert!(
        !handle
            .update(cx, |session, _, cx| session.messages_showing_for_test(cx))
            .unwrap(),
        "a new run is what the user asked to see, so the grid comes back"
    );
}

#[gpui_kit::test]
fn switching_to_a_result_without_messages_returns_to_the_grid(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    handle
        .update(cx, |session, _, cx| {
            session.show_query_results_for_test(
                vec![
                    result_with_messages(vec![warning("1265", "Data truncated for column 'v'")]),
                    result_with_messages(vec![]),
                ],
                cx,
            )
        })
        .unwrap();
    handle
        .update(cx, |session, _, cx| {
            session.pick_messages_for_test(true, cx)
        })
        .unwrap();
    assert!(
        handle
            .update(cx, |session, _, cx| session.messages_showing_for_test(cx))
            .unwrap()
    );

    handle
        .update(cx, |session, _, cx| session.pick_result_for_test(1, cx))
        .unwrap();
    assert!(
        !handle
            .update(cx, |session, _, cx| session.messages_showing_for_test(cx))
            .unwrap(),
        "the second result has no messages, so there is no messages pane to stay on"
    );
    draw(cx, handle);
}
