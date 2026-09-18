//! The side panel that lays the focused row out one field per column.

use super::*;

fn panel(
    cx: &mut TestAppContext,
    view: &gpui_kit::Entity<crate::ui::table_view::TableView>,
) -> gpui_kit::Entity<crate::ui::table_view::RowPanel> {
    view.read_with(cx, |view, _| view.row_panel_for_test())
}

#[gpui_kit::test]
fn the_panel_follows_the_focused_row(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();
    let panel = panel(cx, &view);

    select_cell(cx, &view, 1, 1);
    let (focused, name) = panel.read_with(cx, |panel, cx| {
        (
            panel.focused_row_for_test(),
            panel.field_value_for_test(1, cx),
        )
    });
    let expected = view.read_with(cx, |view, cx| {
        view.grid_for_test().read(cx).cell_for_test(1, 1, cx)
    });
    assert_eq!(focused, Some(1));
    assert_eq!(Some(name), expected.or(Some(String::new())));

    view.read_with(cx, |view, _| view.grid_for_test())
        .update(cx, |grid, cx| grid.clear(cx));
    cx.run_until_parked();
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.focused_row_for_test()),
        None
    );
}

#[gpui_kit::test]
fn typing_in_the_panel_stages_like_a_grid_edit(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();
    let panel = panel(cx, &view);
    select_cell(cx, &view, 0, 1);

    cx.update_window(handle.into(), |_, window, cx| {
        panel.update(cx, |panel, cx| {
            panel.set_field_for_test(1, "from the panel", window, cx);
            panel.commit_field_for_test(1, window, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();

    let staged = view.read_with(cx, |view, cx| view.grid_for_test().read(cx).staged(cx));
    assert_eq!(staged.len(), 1);
    assert_eq!(
        staged[0].cells,
        vec![(1, Some("from the panel".to_string()))]
    );
}

#[gpui_kit::test]
fn the_filter_narrows_columns_and_survives_a_bad_pattern(cx: &mut TestAppContext) {
    let (_database, handle, view) = table_view(cx);
    cx.run_until_parked();
    let panel = panel(cx, &view);

    cx.update_window(handle.into(), |_, window, cx| {
        panel.update(cx, |panel, cx| panel.set_filter_for_test("NAM", window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.shown_columns_for_test()),
        vec!["name".to_string()]
    );

    cx.update_window(handle.into(), |_, window, cx| {
        panel.update(cx, |panel, cx| panel.set_filter_for_test("na(", window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    assert!(panel.read_with(cx, |panel, _| panel.shown_columns_for_test().is_empty()));
}

#[gpui_kit::test]
fn the_panel_can_be_hidden_and_brought_back(cx: &mut TestAppContext) {
    let (_database, _handle, view) = table_view(cx);
    cx.run_until_parked();
    assert!(view.read_with(cx, |view, _| view.row_panel_visible_for_test()));

    view.update(cx, |view, cx| view.toggle_row_panel_for_test(cx));
    assert!(!view.read_with(cx, |view, _| view.row_panel_visible_for_test()));

    view.update(cx, |view, cx| view.toggle_row_panel_for_test(cx));
    assert!(view.read_with(cx, |view, _| view.row_panel_visible_for_test()));
}
