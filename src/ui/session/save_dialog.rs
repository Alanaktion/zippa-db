//! The "Save query" dialog: a name box for the buffer the toolbar button was
//! pressed on. Confirming stores it in the global saved-query list; the
//! panel hears about it through [`SaveQueryEvent`].

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, Window, div, px};

/// What the dialog did. The panel turns `Saved` into a
/// `SessionPanelEvent::SavedQueriesChanged` so the sidebar repaints.
pub(crate) enum SaveQueryEvent {
    Saved,
}

/// One text box for the query's name, with Save/Cancel beneath it.
pub(crate) struct SaveQueryDialog {
    name: Entity<InputState>,
    sql: String,
}

impl SaveQueryDialog {
    pub(crate) fn open(
        sql: String,
        default_name: String,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let view = cx.new(|cx| {
            let name = cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_value(default_name, window, cx);
                state.select_all(window, cx);
                state
            });
            Self { name, sql }
        });
        let body = view.clone();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .title("Save query")
                .w(px(420.))
                .overlay_closable(true)
                .child(body.clone())
        });
        view.update(cx, |this, cx| {
            this.name.update(cx, |state, cx| {
                state.focus(window, cx);
            })
        });
        view
    }
}

impl EventEmitter<SaveQueryEvent> for SaveQueryDialog {}

impl Render for SaveQueryDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let can_save = !self.name.read(cx).value().trim().is_empty();

        v_flex()
            .p_4()
            .gap_3()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Name this query to keep it in the sidebar's Saved tab."),
            )
            .child(Input::new(&self.name).id("save-query-name").small())
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("save-query-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.close_dialog(cx);
                            })),
                    )
                    .child(
                        Button::new("save-query-confirm")
                            .primary()
                            .small()
                            .label("Save query")
                            .disabled(!can_save)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let name = this.name.read(cx).value().trim().to_string();
                                if name.is_empty() {
                                    return;
                                }
                                if crate::saved_queries::upsert(&name, &this.sql).is_ok() {
                                    cx.emit(SaveQueryEvent::Saved);
                                    window.close_dialog(cx);
                                }
                            })),
                    ),
            )
    }
}
