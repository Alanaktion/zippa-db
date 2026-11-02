//! What a column header says about its column: key marks and a tooltip with
//! the type, nullability, and default.

use super::*;
use crate::ui::data_grid::header_text;

#[gpui_kit::test]
fn a_table_tabs_headers_know_keys_nullability_and_defaults(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view_on(
        cx,
        "entries",
        "create table races (id integer primary key, name text); \
         create table entries (id integer primary key, \
             race_id integer not null references races(id), \
             bib text default 'TBD'); \
         insert into races (id, name) values (1, 'city 10k'); \
         insert into entries (id, race_id) values (1, 1);",
    );
    cx.run_until_parked();

    let grid = view.read_with(cx, |view, _| view.grid_for_test());
    let info = |name: &str| {
        grid.read_with(cx, |grid, cx| grid.column_info(name, cx))
            .unwrap_or_else(|| panic!("{name} should have schema info"))
    };

    let id = info("id");
    assert!(id.primary_key, "id is the primary key");
    assert_eq!(id.references, None);

    let race = info("race_id");
    assert!(!race.primary_key);
    assert_eq!(race.references.as_deref(), Some("races.id"));
    assert_eq!(race.nullable, Some(false));

    let bib = info("bib");
    assert_eq!(bib.nullable, Some(true));
    assert_eq!(bib.default.as_deref(), Some("'TBD'"));

    let text = header_text("race_id", "INTEGER", Some(&race));
    assert_eq!(text.label, "race_id, foreign key to races.id");
    assert!(text.tooltip.contains("NOT NULL"), "{}", text.tooltip);
}

#[gpui_kit::test]
fn a_query_results_headers_have_no_schema_info(cx: &mut TestAppContext) {
    let (_database, handle) = session_with_objects(cx);

    prepare_editor(cx, handle, "select * from items", 0);
    press(cx, handle, "secondary-enter");
    cx.run_until_parked();

    let grid = handle
        .update(cx, |session, _, cx| session.active_grid(cx))
        .unwrap()
        .expect("the query tab should have a grid");
    assert_eq!(
        grid.read_with(cx, |grid, cx| grid.column_info("id", cx)),
        None,
        "a query result's header shows the driver type alone"
    );
}
