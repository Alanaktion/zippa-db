//! The "Save query" dialog: a name box for the buffer being kept. Confirming
//! — the Save button, or `Enter`, which the dialog turns into its Confirm —
//! stores it in the app's one
//! saved-query list, which every sidebar observes, and tells the caller the
//! name it went under so the tab can keep updating it in place.

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::DialogFooter;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, Window, div, px};

use crate::saved_queries::{self, SavedQueries};

/// What the dialog's owner does once the query is saved, given its name.
pub(crate) type OnSaved = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// One text box for the query's name; Save/Cancel sit in the dialog's footer.
pub(crate) struct SaveQueryDialog {
    name: Entity<InputState>,
    sql: String,
    on_saved: OnSaved,
}

impl SaveQueryDialog {
    pub(crate) fn open(
        sql: String,
        default_name: String,
        on_saved: OnSaved,
        window: &mut Window,
        cx: &mut App,
    ) {
        let view = cx.new(|cx| {
            let name = cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder("Name");
                state.set_value(default_name, window, cx);
                state.select_all(window, cx);
                state
            });
            cx.observe(&name, |_, _, cx| cx.notify()).detach();
            Self {
                name,
                sql,
                on_saved,
            }
        });
        let body = view.clone();
        window.open_dialog(cx, move |dialog, _window, cx| {
            let can_save = body.read(cx).can_save(cx);
            let cancel = body.clone();
            let confirm = body.clone();
            let ok = body.clone();
            dialog
                .title("Save query")
                .w(px(360.))
                .overlay_closable(true)
                // `Enter` in the name box is the dialog's Confirm, which
                // lands here; an empty name keeps the dialog open.
                .on_ok(move |_, window, cx| ok.update(cx, |this, cx| this.save(window, cx)))
                .child(body.clone())
                .footer(
                    DialogFooter::new()
                        .child(
                            Button::new("save-query-cancel")
                                .ghost()
                                .small()
                                .label("Cancel")
                                .on_click(move |_, window, cx| {
                                    cancel.update(cx, |_, cx| window.close_dialog(cx));
                                }),
                        )
                        .child(
                            Button::new("save-query-confirm")
                                .primary()
                                .small()
                                .label("Save")
                                .disabled(!can_save)
                                .on_click(move |_, window, cx| {
                                    if confirm.update(cx, |this, cx| this.save(window, cx)) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
        });
        view.update(cx, |this, cx| {
            this.name.update(cx, |state, cx| {
                state.focus(window, cx);
            })
        });
    }

    fn typed_name(&self, cx: &App) -> String {
        self.name.read(cx).value().trim().to_string()
    }

    fn can_save(&self, cx: &App) -> bool {
        !self.typed_name(cx).is_empty()
    }

    /// Store the query under the typed name, and say whether there was one
    /// to store it under; the caller closes the dialog.
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let name = self.typed_name(cx);
        if name.is_empty() {
            return false;
        }
        saved_queries::upsert(&name, &self.sql, cx);
        (self.on_saved)(name, window, cx);
        true
    }
}

impl Render for SaveQueryDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.typed_name(cx);
        // Saving under a name already in the list replaces that query, which
        // is worth saying before it happens.
        let replaces =
            !name.is_empty() && SavedQueries::get(cx).iter().any(|query| query.name == name);

        v_flex()
            .gap_2()
            .child(
                Input::new(&self.name)
                    .id("save-query-name")
                    .aria_label("Query name")
                    .small(),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if replaces {
                        "Replaces the saved query with this name."
                    } else {
                        "Kept in the sidebar's Saved tab."
                    }),
            )
    }
}
