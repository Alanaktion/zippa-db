//! The Agents panel: placement, the approval prompt, and the gates around it.

use super::*;
use crate::db::agent::gate::Kind;
use crate::db::agent::provider::Message;
use crate::db::agent::tools::{UiAction, WriteRequest};
use crate::db::agent::turn::{TurnEvent, TurnOutcome};
use crate::db::{AgentAccess, WriteApproval};

fn delete_request() -> WriteRequest {
    WriteRequest {
        sql: "delete from items where id = 1".into(),
        kind: Kind::Dml,
        access: AgentAccess::Crud,
        approval: WriteApproval::Ask,
    }
}

#[gpui_kit::test]
fn the_panel_toggles_and_keeps_its_thread(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();

    handle
        .update(cx, |session, window, cx| {
            session.toggle_agents_for_test(window, cx)
        })
        .unwrap();
    assert!(
        handle
            .update(cx, |s, _, _| s.agents_open_for_test())
            .unwrap()
    );

    // A message typed with no endpoint configured says so and sends nothing.
    agents.update(cx, |agents, cx| {
        agents.start_turn("what tables are here?".into(), Vec::new(), None, cx)
    });
    let lines = agents.read_with(cx, |agents, _| agents.lines_for_test());
    assert_eq!(lines[0], "what tables are here?");
    assert!(lines[1].contains("Settings"), "{lines:?}");
    assert!(!agents.read_with(cx, |agents, _| agents.is_running()));

    handle
        .update(cx, |session, window, cx| {
            session.toggle_agents_for_test(window, cx)
        })
        .unwrap();
    assert!(
        !handle
            .update(cx, |s, _, _| s.agents_open_for_test())
            .unwrap()
    );
    assert_eq!(
        agents.read_with(cx, |agents, _| agents.lines_for_test().len()),
        2,
        "hiding the panel must not end the thread"
    );
}

#[gpui_kit::test]
fn a_connection_starts_at_schema_access_asking_for_writes(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();
    agents.read_with(cx, |agents, _| {
        assert_eq!(agents.access(), AgentAccess::Schema);
        assert_eq!(agents.approval(), WriteApproval::Ask);
    });
}

#[gpui_kit::test]
fn a_write_waits_for_an_answer_and_runs_only_on_approve(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();

    let mut answer = agents.update(cx, |agents, cx| {
        agents.offer_write_for_test(delete_request(), cx)
    });
    assert_eq!(
        agents
            .read_with(cx, |agents, _| agents.pending_sql_for_test())
            .as_deref(),
        Some("delete from items where id = 1")
    );
    assert_eq!(
        answer.try_recv().unwrap(),
        None,
        "nothing is decided until the user answers"
    );

    agents.update(cx, |agents, cx| agents.answer_for_test(true, cx));
    assert_eq!(answer.try_recv().unwrap(), Some(true));
    assert_eq!(
        agents.read_with(cx, |agents, _| agents.pending_sql_for_test()),
        None
    );

    // Approval was for that one statement: the next is asked about afresh.
    let mut second = agents.update(cx, |agents, cx| {
        agents.offer_write_for_test(delete_request(), cx)
    });
    agents.update(cx, |agents, cx| agents.answer_for_test(false, cx));
    assert_eq!(second.try_recv().unwrap(), Some(false));
}

#[gpui_kit::test]
fn stopping_rejects_a_waiting_write(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();
    let mut answer = agents.update(cx, |agents, cx| {
        agents.offer_write_for_test(delete_request(), cx)
    });
    // Stop only acts on a running turn; one that is not running leaves the
    // question up, so run state is set the way a turn would.
    agents.update(cx, |agents, cx| agents.set_running_for_test(cx));
    agents.update(cx, |agents, cx| agents.stop(cx));
    assert!(
        answer.try_recv().is_err(),
        "the dropped answer reads as a rejection"
    );
}

#[gpui_kit::test]
fn the_panel_says_when_it_is_off(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();
    agents.update(cx, |agents, cx| {
        agents.set_access_for_test(AgentAccess::None, cx)
    });
    agents.update(cx, |agents, cx| {
        agents.start_turn("hi".into(), Vec::new(), None, cx)
    });
    // A turn is still refused when it is somehow asked for: no model call.
    assert!(!agents.read_with(cx, |agents, _| agents.is_running()));
}

#[gpui_kit::test]
fn auto_approval_is_taken_at_once_on_an_ordinary_connection(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();
    agents.update(cx, |agents, cx| {
        agents.set_access_for_test(AgentAccess::Crud, cx)
    });
    handle
        .update(cx, |_, window, cx| {
            agents.update(cx, |agents, cx| {
                agents.choose_approval_for_test(WriteApproval::Auto, window, cx)
            })
        })
        .unwrap();
    assert_eq!(
        agents.read_with(cx, |agents, _| agents.approval()),
        WriteApproval::Auto
    );
}

#[gpui_kit::test]
fn replies_draw_as_markdown(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    handle
        .update(cx, |session, window, cx| {
            session.toggle_agents_for_test(window, cx)
        })
        .unwrap();
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();
    agents.update(cx, |agents, cx| {
        agents.say_for_test("**bold** and `code`", cx)
    });
    // Drawing must not panic on a rich-text message.
    cx.update_window(handle.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_message_copies_as_its_raw_markdown(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    handle
        .update(cx, |session, window, cx| {
            session.toggle_agents_for_test(window, cx)
        })
        .unwrap();
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();
    agents.update(cx, |agents, cx| {
        agents.say_for_test("**bold** and `code`", cx)
    });
    click_in_session(cx, handle, "agent-copy-0");
    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some("**bold** and `code`")
    );
}

#[gpui_kit::test]
fn tool_placeholders_fill_in_in_place_and_text_streams(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();

    // Text streams into one assistant item, then a second round's text
    // starts a new one after the tool item between them.
    agents.update(cx, |agents, cx| {
        agents.apply_turn_event_for_test(TurnEvent::AssistantDelta("Hel".into()), cx);
        agents.apply_turn_event_for_test(TurnEvent::AssistantDelta("lo".into()), cx);
        agents.apply_turn_event_for_test(
            TurnEvent::ToolPending {
                index: 0,
                name: "list_tables".into(),
                arguments: "{}".into(),
            },
            cx,
        );
    });
    let tools = agents.read_with(cx, |agents, _| agents.tools_for_test());
    assert_eq!(tools.len(), 1, "a pending tool is drawn at once");
    assert_eq!(tools[0].2, None, "no result yet");
    assert_eq!(tools[0].3, "waiting");

    agents.update(cx, |agents, cx| {
        agents.apply_turn_event_for_test(TurnEvent::ToolRunning { index: 0 }, cx);
        agents.apply_turn_event_for_test(
            TurnEvent::ToolFinished {
                index: 0,
                result: "table items".into(),
                is_error: false,
            },
            cx,
        );
        agents.apply_turn_event_for_test(TurnEvent::AssistantDelta("Done".into()), cx);
    });
    let tools = agents.read_with(cx, |agents, _| agents.tools_for_test());
    assert_eq!(
        tools.len(),
        1,
        "the result fills the placeholder, not a second item"
    );
    assert_eq!(tools[0].2.as_deref(), Some("table items"));
    assert_eq!(tools[0].3, "done");
    // Full order: streamed text, tool, final text.
    let lines = agents.read_with(cx, |agents, _| agents.lines_for_test());
    assert_eq!(lines, vec!["Hello", "tool list_tables", "Done"]);
}

#[gpui_kit::test]
fn new_output_follows_the_bottom_only_while_the_user_is_there(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    handle
        .update(cx, |session, window, cx| {
            session.toggle_agents_for_test(window, cx)
        })
        .unwrap();
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();

    let draw = |cx: &mut TestAppContext| {
        cx.update_window(handle.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
        })
        .unwrap();
    };

    // Enough thread to overflow the panel, so there is a bottom to follow.
    agents.update(cx, |agents, cx| {
        agents.begin_turn_for_test("question", cx);
        for line in 0..100 {
            agents.say_for_test(&format!("line {line}"), cx);
        }
    });
    draw(cx);
    let max = agents.read_with(cx, |agents, _| agents.scroll_max_for_test());
    assert!(max > px(0.), "the thread must overflow the panel");

    // Scrolled to the top, new output must not yank the view back down.
    agents.update(cx, |agents, cx| {
        agents.set_scroll_offset_for_test(px(0.));
        agents.apply_turn_event_for_test(TurnEvent::AssistantDelta("more".into()), cx);
    });
    draw(cx);
    assert_eq!(
        agents.read_with(cx, |agents, _| agents.scroll_offset_for_test()),
        px(0.),
        "reading earlier output must not be interrupted"
    );

    // Back at the bottom, new output follows it as it grows.
    agents.update(cx, |agents, cx| {
        let max = agents.scroll_max_for_test();
        agents.set_scroll_offset_for_test(-max);
        agents.apply_turn_event_for_test(TurnEvent::AssistantDelta("word ".repeat(100)), cx);
    });
    draw(cx);
    let (offset, max) = agents.read_with(cx, |agents, _| {
        (
            agents.scroll_offset_for_test(),
            agents.scroll_max_for_test(),
        )
    });
    assert_eq!(
        offset, -max,
        "output at the bottom must stay at the bottom as it grows"
    );
}

#[gpui_kit::test]
fn a_stopped_tool_does_not_stay_running(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();

    agents.update(cx, |agents, cx| {
        agents.apply_turn_event_for_test(
            TurnEvent::ToolPending {
                index: 0,
                name: "list_tables".into(),
                arguments: "{}".into(),
            },
            cx,
        );
        agents.apply_turn_event_for_test(TurnEvent::ToolRunning { index: 0 }, cx);
        agents.set_running_for_test(cx);
        agents.stop(cx);
    });
    let tools = agents.read_with(cx, |agents, _| agents.tools_for_test());
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].3, "stopped");
    assert_eq!(tools[0].2.as_deref(), Some("stopped"));
}

#[gpui_kit::test]
fn enter_sends_and_shift_enter_adds_a_line(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    handle
        .update(cx, |session, window, cx| {
            session.toggle_agents_for_test(window, cx)
        })
        .unwrap();
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();

    handle
        .update(cx, |_, window, cx| {
            agents.update(cx, |agents, cx| agents.type_for_test("first", window, cx))
        })
        .unwrap();
    press(cx, handle, "shift-enter");
    assert!(
        agents
            .read_with(cx, |agents, cx| agents.composer_text_for_test(cx))
            .contains('\n'),
        "Shift+Enter should start a new line"
    );
    assert!(
        agents
            .read_with(cx, |agents, _| agents.lines_for_test())
            .is_empty()
    );

    press(cx, handle, "enter");
    cx.run_until_parked();
    let lines = agents.read_with(cx, |agents, _| agents.lines_for_test());
    assert!(lines[0].starts_with("first"), "{lines:?}");
    assert_eq!(
        agents.read_with(cx, |agents, cx| agents.composer_text_for_test(cx)),
        ""
    );
}

#[gpui_kit::test]
fn an_empty_answer_with_no_tool_calls_leaves_a_notice(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();
    agents.update(cx, |agents, cx| {
        agents.finish_for_test(
            TurnOutcome {
                messages: vec![Message::User("what tables are here?".into())],
                answer: None,
                ..TurnOutcome::default()
            },
            cx,
        );
    });
    let lines = agents.read_with(cx, |agents, _| agents.lines_for_test());
    assert!(
        lines.iter().any(|line| line.contains("returned no answer")),
        "an empty turn should say so, not vanish: {lines:?}"
    );
}

#[gpui_kit::test]
fn a_turn_that_showed_something_gets_no_empty_notice(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();

    // Assistant words on screen: no notice, even with no closing answer.
    agents.update(cx, |agents, cx| agents.say_for_test("here they are", cx));
    agents.update(cx, |agents, cx| {
        agents.finish_for_test(
            TurnOutcome {
                answer: None,
                ..TurnOutcome::default()
            },
            cx,
        );
    });
    let lines = agents.read_with(cx, |agents, _| agents.lines_for_test());
    assert!(
        !lines.iter().any(|line| line.contains("returned no answer")),
        "{lines:?}"
    );

    // A tool item on screen likewise: the turn did not vanish.
    agents.update(cx, |agents, cx| {
        agents.apply_turn_event_for_test(
            TurnEvent::ToolPending {
                index: 0,
                name: "list_tables".into(),
                arguments: "{}".into(),
            },
            cx,
        );
        agents.finish_for_test(
            TurnOutcome {
                answer: None,
                ..TurnOutcome::default()
            },
            cx,
        );
    });
    let lines = agents.read_with(cx, |agents, _| agents.lines_for_test());
    assert!(
        !lines.iter().any(|line| line.contains("returned no answer")),
        "{lines:?}"
    );
}

#[gpui_kit::test]
fn a_running_turn_keeps_showing_after_access_is_lowered(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();

    assert!(agents.read_with(cx, |agents, _| agents.shows_thread_for_test()));
    agents.update(cx, |agents, cx| {
        agents.set_access_for_test(AgentAccess::None, cx)
    });
    assert!(
        !agents.read_with(cx, |agents, _| agents.shows_thread_for_test()),
        "with nothing running, the turned-off message is right"
    );
    agents.update(cx, |agents, cx| agents.set_running_for_test(cx));
    assert!(
        agents.read_with(cx, |agents, _| agents.shows_thread_for_test()),
        "lowering access mid-turn must not hide the running turn's output"
    );
}

#[gpui_kit::test]
fn a_catalog_refresh_action_reloads_the_session_catalog(cx: &mut TestAppContext) {
    let (database, handle) = session_with_objects(cx);
    let agents = handle
        .update(cx, |session, _, _| session.agents_for_test())
        .unwrap();

    // A table the session has never seen, made behind its back.
    runtime::block_on(async {
        let connection = Connection::open(database.config(), None)
            .await
            .expect("could not reopen the test database");
        connection
            .execute("create table fresh (id integer)", Vec::new())
            .await
            .expect("could not create the table");
        connection.close().await;
    });

    agents.update(cx, |agents, cx| {
        agents.finish_for_test(
            TurnOutcome {
                ui: vec![UiAction::RefreshCatalog],
                ..TurnOutcome::default()
            },
            cx,
        );
    });
    cx.run_until_parked();

    let catalog = handle
        .update(cx, |session, _, _| session.catalog())
        .unwrap();
    assert!(
        catalog.entries.iter().any(|entry| entry.name == "fresh"),
        "the re-read catalog should hold the new table"
    );
}
