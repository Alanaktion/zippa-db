//! Transactions in a query tab: its own connection, the status bar's word
//! for an open one, Commit and Roll Back, and asking before one is thrown
//! away.

use super::*;

#[gpui_kit::test]
fn a_transaction_lasts_between_runs_and_commit_ends_it(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session(cx);

    run_in_tab(cx, &handle, &session, "BEGIN");
    assert_eq!(transaction(cx, &session), TxnState::Open);
    run_in_tab(
        cx,
        &handle,
        &session,
        "insert into items values (3, 'gamma', 3.0, NULL)",
    );
    assert_eq!(
        transaction(cx, &session),
        TxnState::Open,
        "the next run is on the same connection, inside the same transaction"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "nothing is visible elsewhere before the commit"
    );

    click_workspace(cx, &handle, "transaction-commit");
    assert_eq!(transaction(cx, &session), TxnState::Idle);
    assert_eq!(runtime::block_on(other_count(&database)), 3);
}

#[gpui_kit::test]
fn roll_back_in_the_status_bar_undoes_the_transaction(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session(cx);

    run_in_tab(cx, &handle, &session, "BEGIN");
    run_in_tab(cx, &handle, &session, "delete from items");
    click_workspace(cx, &handle, "transaction-rollback");

    assert_eq!(transaction(cx, &session), TxnState::Idle);
    assert_eq!(runtime::block_on(other_count(&database)), 2);
}

#[gpui_kit::test]
fn closing_a_tab_with_an_open_transaction_asks_and_rolls_it_back(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session(cx);

    run_in_tab(cx, &handle, &session, "BEGIN");
    run_in_tab(
        cx,
        &handle,
        &session,
        "insert into items values (3, 'gamma', 3.0, NULL)",
    );
    assert!(!session.read_with(cx, |session, cx| session.tab_is_dirty_for_test(0, cx)));

    close_workspace_tab(cx, &handle, &session, 0);
    assert!(
        workspace_dialog_open(cx, &handle),
        "a tab with an open transaction should be asked about before it closes"
    );

    // Keeping it open keeps the transaction.
    click_workspace(cx, &handle, "cancel");
    assert_eq!(transaction(cx, &session), TxnState::Open);

    close_workspace_tab(cx, &handle, &session, 0);
    click_workspace(cx, &handle, "ok");
    cx.run_until_parked();
    assert_eq!(
        session.read_with(cx, |session, cx| session.tab_titles(cx)),
        ["Query 2"],
        "the tab closed on the answer"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "closing the tab rolled its transaction back"
    );
}

#[gpui_kit::test]
fn a_tab_without_a_transaction_closes_without_asking(cx: &mut TestAppContext) {
    let (_database, handle, session) = workspace_session(cx);

    run_in_tab(cx, &handle, &session, "select 1");
    assert_eq!(transaction(cx, &session), TxnState::Idle);

    close_workspace_tab(cx, &handle, &session, 0);
    assert!(!workspace_dialog_open(cx, &handle));
}

#[gpui_kit::test]
fn each_query_tab_has_its_own_transaction(cx: &mut TestAppContext) {
    let (database, handle, session) = workspace_session(cx);

    run_in_tab(cx, &handle, &session, "BEGIN");
    run_in_tab(
        cx,
        &handle,
        &session,
        "insert into items values (3, 'gamma', 3.0, NULL)",
    );

    // A second tab is on a connection of its own, outside the transaction.
    cx.update_window(handle.window.into(), |_, window, cx| {
        session.update(cx, |session, cx| session.open_tab_for_test(window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(transaction(cx, &session), TxnState::Idle);
    run_in_tab(cx, &handle, &session, "select count(*) from items");
    let status = session.read_with(cx, |session, cx| session.active_status_for_test(cx));
    assert!(status.contains("1 row"), "{status}");
    assert_eq!(runtime::block_on(other_count(&database)), 2);
}

#[gpui_kit::test]
fn a_second_buffer_sees_the_first_buffers_session_when_sharing_is_on(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Settings {
            share_connection_across_buffers: true,
            ..Settings::default()
        })
    });
    let (_database, handle, session) = workspace_session(cx);

    // A temporary table only exists on the connection that created it.
    run_in_tab(
        cx,
        &handle,
        &session,
        "create temp table shared_seen (a integer)",
    );
    open_workspace_tab(cx, &handle, &session);
    run_in_tab(cx, &handle, &session, "insert into shared_seen values (1)");

    let status = session.read_with(cx, |session, cx| session.active_status_for_test(cx));
    assert!(
        !status.starts_with("Error:"),
        "the second buffer should see the first buffer's temporary table: {status}"
    );
}

#[gpui_kit::test]
fn buffers_keep_their_own_connections_by_default(cx: &mut TestAppContext) {
    let (_database, handle, session) = workspace_session(cx);

    run_in_tab(
        cx,
        &handle,
        &session,
        "create temp table private_seen (a integer)",
    );
    open_workspace_tab(cx, &handle, &session);
    run_in_tab(cx, &handle, &session, "insert into private_seen values (1)");

    let status = session.read_with(cx, |session, cx| session.active_status_for_test(cx));
    assert!(
        status.starts_with("Error:"),
        "the second buffer should not see the first buffer's temporary table: {status}"
    );
}

#[gpui_kit::test]
fn an_open_transaction_is_shared_across_buffers_when_the_setting_is_on(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Settings {
            share_connection_across_buffers: true,
            ..Settings::default()
        })
    });
    let (_database, handle, session) = workspace_session(cx);

    run_in_tab(cx, &handle, &session, "BEGIN");
    open_workspace_tab(cx, &handle, &session);
    // The second tab's first run puts it on the shared connection, inside the
    // first buffer's transaction.
    run_in_tab(cx, &handle, &session, "select 1");

    assert_eq!(
        transaction(cx, &session),
        TxnState::Open,
        "the second buffer runs on the same connection, inside the first buffer's transaction"
    );
}

#[gpui_kit::test]
fn a_tab_with_an_open_transaction_keeps_its_connection_when_sharing_is_turned_on(
    cx: &mut TestAppContext,
) {
    let (_database, handle, session) = workspace_session(cx);

    run_in_tab(cx, &handle, &session, "BEGIN");
    run_in_tab(
        cx,
        &handle,
        &session,
        "create temp table bound_seen (a integer)",
    );
    // Sharing is turned on with the transaction still open: the tab stays on
    // its own connection rather than rolling the transaction back.
    cx.update(|cx| {
        cx.set_global(Settings {
            share_connection_across_buffers: true,
            ..Settings::default()
        })
    });
    open_workspace_tab(cx, &handle, &session);
    run_in_tab(cx, &handle, &session, "insert into bound_seen values (1)");

    let status = session.read_with(cx, |session, cx| session.active_status_for_test(cx));
    assert!(
        status.starts_with("Error:"),
        "the second tab is on the shared connection, not the first tab's still-open one: {status}"
    );

    activate_workspace_tab(cx, &handle, &session, 0);
    assert_eq!(
        transaction(cx, &session),
        TxnState::Open,
        "turning the setting on should not roll back the first tab's transaction"
    );
}

#[gpui_kit::test]
fn a_tab_with_an_open_transaction_keeps_the_shared_connection_when_sharing_is_turned_off(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        cx.set_global(Settings {
            share_connection_across_buffers: true,
            ..Settings::default()
        })
    });
    let (_database, handle, session) = workspace_session(cx);

    run_in_tab(cx, &handle, &session, "BEGIN");
    run_in_tab(
        cx,
        &handle,
        &session,
        "create temp table still_shared (a integer)",
    );
    // Sharing is turned off with the transaction still open: the tab stays on
    // the shared connection rather than rolling the transaction back.
    cx.update(|cx| {
        cx.set_global(Settings {
            share_connection_across_buffers: false,
            ..Settings::default()
        })
    });
    open_workspace_tab(cx, &handle, &session);
    run_in_tab(cx, &handle, &session, "insert into still_shared values (1)");

    let status = session.read_with(cx, |session, cx| session.active_status_for_test(cx));
    assert!(
        status.starts_with("Error:"),
        "the second tab should be on a connection of its own: {status}"
    );

    activate_workspace_tab(cx, &handle, &session, 0);
    assert_eq!(
        transaction(cx, &session),
        TxnState::Open,
        "turning the setting off should not roll back the first tab's transaction"
    );
    run_in_tab(cx, &handle, &session, "insert into still_shared values (2)");
    let status = session.read_with(cx, |session, cx| session.active_status_for_test(cx));
    assert!(
        !status.starts_with("Error:"),
        "the first tab should still see its temporary table: {status}"
    );
}
