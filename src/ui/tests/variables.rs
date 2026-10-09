//! Query variables: `:name` substitution when a buffer runs.

use super::*;
use crate::db::params::Variable;
use crate::ui::variables_dialog::VariablesView;

fn set_variables(cx: &mut TestAppContext, handle: WindowHandle<Session>, variables: Vec<Variable>) {
    handle
        .update(cx, |session, _, cx| {
            let editor = session
                .active_editor_for_test(cx)
                .expect("a query tab has an editor");
            editor.update(cx, |editor, cx| editor.set_variables(variables, cx));
        })
        .unwrap();
}

fn variable(name: &str, value: &str) -> Variable {
    Variable {
        name: name.to_string(),
        value: value.to_string(),
    }
}

#[gpui_kit::test]
fn a_variable_binds_into_the_statement(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "select * from items where name = :name", 0);
    set_variables(cx, handle, vec![variable("name", "alpha")]);
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    let grid = handle
        .update(cx, |session, _, cx| session.active_grid(cx))
        .unwrap()
        .expect("a query tab has a grid");
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.column_values_for_test(1, cx)),
        vec![Some("alpha".to_string())],
        "the :name placeholder should have bound 'alpha'"
    );
}

#[gpui_kit::test]
fn the_same_variable_is_used_for_every_statement_in_a_script(cx: &mut TestAppContext) {
    let (database, handle) = session_with_objects(cx);

    // Alan's example: one mapping for the whole buffer, across statements.
    let sql = "insert into items values (3, :name, 3.0, NULL);\n\
               select name from items where name = :name;";
    prepare_editor(cx, handle, sql, 0);
    set_variables(cx, handle, vec![variable("name", "gamma")]);
    press(cx, handle, "secondary-shift-enter");
    cx.run_until_parked();

    let (results, _) = handle
        .update(cx, |session, _, cx| session.results_for_test(cx))
        .unwrap();
    assert_eq!(results, 2, "both statements should have run");
    assert_eq!(
        runtime::block_on(name_of(&database, 3)),
        Some("gamma".to_string()),
        "the insert should have bound :name"
    );
}

#[gpui_kit::test]
fn a_missing_variable_refuses_the_run_before_anything_executes(cx: &mut TestAppContext) {
    let (database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "select * from items where name = :name", 0);
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    let status = handle
        .update(cx, |session, _, cx| session.active_status_for_test(cx))
        .unwrap();
    assert!(
        status.contains("no value for variable :name"),
        "the run should be refused naming the variable: {status}"
    );
    assert_eq!(
        runtime::block_on(other_count(&database)),
        2,
        "nothing should have run"
    );
}

#[gpui_kit::test]
fn the_dialog_collects_its_rows_into_variables(cx: &mut TestAppContext) {
    cx.update(|cx| {
        init_ui(cx);
    });
    let view = cx.open_window(size(px(WINDOW.0), px(WINDOW.1)), |window, cx| {
        VariablesView::new(vec![variable("name", "gamma")], window, cx)
    });
    cx.run_until_parked();

    // The tab's variables arrive as rows.
    let rows = view
        .read_with(cx, |view, cx| view.collected_for_test(cx))
        .unwrap();
    assert_eq!(rows, vec![variable("name", "gamma")]);

    // Editing a row changes what saving would hand over.
    view.update(cx, |view, window, cx| {
        view.set_row_for_test(0, "name", "delta", window, cx);
    })
    .unwrap();
    let rows = view
        .read_with(cx, |view, cx| view.collected_for_test(cx))
        .unwrap();
    assert_eq!(rows, vec![variable("name", "delta")]);
}

#[gpui_kit::test]
fn saving_stores_the_variables_on_the_tab(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    let panel = handle
        .update(cx, |session, _, _cx| {
            session
                .active_panel_for_test()
                .expect("a query tab is open")
        })
        .unwrap();
    handle
        .update(cx, |session, _, cx| {
            session.apply_variables(&panel, vec![variable("name", "delta")], cx);
        })
        .unwrap();

    let variables = handle
        .update(cx, |session, _, cx| {
            session
                .active_editor_for_test(cx)
                .expect("a query tab has an editor")
                .read(cx)
                .variables()
        })
        .unwrap();
    assert_eq!(variables, vec![variable("name", "delta")]);
}

#[gpui_kit::test]
fn variables_ride_along_with_the_session_snapshot(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);
    set_variables(cx, handle, vec![variable("name", "gamma")]);

    let state = handle
        .update(cx, |session, _, cx| session.snapshot(cx))
        .unwrap();
    let PanelState::Query { variables, .. } = &state.panels[0] else {
        panic!("the first tab should be a query tab");
    };
    assert_eq!(variables, &vec![variable("name", "gamma")]);
}
