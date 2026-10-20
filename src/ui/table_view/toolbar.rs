//! The bar above the grid: the filters on the leading edge, and what else the
//! view can show of the table — its structure, a file of its rows, the row
//! panel — on the trailing edge. The footer below keeps paging and the
//! staged-change commits, so neither line has to squeeze the other's buttons
//! in a narrow window.

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, IconName, Selectable, Sizable, h_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, div};

use crate::db::export::Format;

use super::{TableView, ToggleRowPanel};

impl TableView {
    pub(super) fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();

        h_flex()
            .flex_none()
            .items_start()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_1().min_w_0().child(self.filters.clone()))
            .child(
                h_flex()
                    .flex_none()
                    .px_2()
                    .py_1()
                    .gap_1()
                    .child(
                        Button::new("view-structure")
                            .ghost()
                            .xsmall()
                            .label("Structure")
                            .tooltip("See the table's columns, indexes, and keys")
                            .on_click(cx.listener(|this, _, _window, cx| this.view_structure(cx))),
                    )
                    .child(
                        Button::new("export-table")
                            .ghost()
                            .xsmall()
                            .label("Export")
                            .dropdown_caret(true)
                            .tooltip("Write every row the view shows to a file")
                            .disabled(self.loading || self.committing)
                            .dropdown_menu(move |mut menu, _window, _cx| {
                                for format in Format::FILE {
                                    let view = view.clone();
                                    menu =
                                        menu.item(
                                            PopupMenuItem::new(format!(
                                                "Export as {}…",
                                                format.label()
                                            ))
                                            .on_click(move |_, _window, cx| {
                                                if let Some(view) = view.upgrade() {
                                                    view.update(cx, |view, cx| {
                                                        view.export(format, cx)
                                                    });
                                                }
                                            }),
                                        );
                                }
                                menu
                            }),
                    )
                    // A panel toggle is a familiar symbol; its pressed state
                    // says whether the panel is open.
                    .child(
                        Button::new("toggle-row-panel")
                            .ghost()
                            .xsmall()
                            .icon(IconName::PanelRightOpen)
                            .selected(self.row_panel_visible)
                            .accessibility_label("Row panel")
                            .tooltip_with_action(
                                "Show the focused row as fields",
                                &ToggleRowPanel,
                                Some("TableView"),
                            )
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.on_toggle_row_panel(&ToggleRowPanel, window, cx)
                            })),
                    ),
            )
    }
}
