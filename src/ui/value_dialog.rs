//! A dialog for one cell's value.
//!
//! TODO.md section 2 ("Specialized Cell Renderers"). A value too big for a
//! grid cell — JSON, several lines, a long string — is opened here instead,
//! in a text box that fills the dialog. The box is where editing a value in
//! full will live, so the dialog takes what to do on save from whoever opened
//! it.
//!
//! The dialog is part of the window rather than a window of its own: it is
//! asked for from deep in the view tree (a grid cell), where there is no
//! window to build a text box with, so the request is left in a global and
//! [`Workspace`](crate::app::Workspace) picks it up while it renders and
//! hands it to `window.open_dialog`.
//!
//! This view is only the *body* of the dialog. The surface around it — the
//! overlay, the focus trap, and the occlusion that keeps a click off the grid
//! behind — is `gpui_kit`'s [`Dialog`](gpui_kit::component::dialog::Dialog),
//! opened by the workspace. The dialog is opened with `keyboard(false)` so
//! its own `escape`/`enter` bindings stay out of the way of the text box;
//! `CloseValue`/`SaveValue` below are what answer those keys instead.

use std::rc::Rc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Context, Entity, EventEmitter, Global, SharedString, Window, actions, div};

use crate::settings;

actions!(zippa_db, [CloseValue, SaveValue]);

/// What the dialog does with the text when the user saves it.
pub type Save = Rc<dyn Fn(String, &mut App)>;

/// A value waiting for a dialog to be built for it.
#[derive(Clone)]
pub struct ValueRequest {
    pub column: SharedString,
    pub text: String,
    /// Why the value cannot be edited, when it cannot.
    pub note: Option<SharedString>,
    /// `None` for a value that cannot be written back — a query result, a
    /// read-only connection, a value the driver only described.
    pub save: Option<Save>,
}

#[derive(Default)]
struct Pending(Option<ValueRequest>);

impl Global for Pending {}

/// Ask for `request` to be shown.
pub fn open(request: ValueRequest, cx: &mut App) {
    cx.set_global(Pending(Some(request)));
}

/// Take the value waiting to be shown, if there is one.
pub fn take(cx: &mut App) -> Option<ValueRequest> {
    cx.try_global::<Pending>()?;
    cx.global_mut::<Pending>().0.take()
}

/// The value waiting to be shown, for a test to read.
#[cfg(test)]
pub(crate) fn pending(cx: &App) -> Option<ValueRequest> {
    cx.try_global::<Pending>()?.0.clone()
}

/// The dialog is done with; the owner takes it off screen.
pub struct Dismissed;

pub struct ValueView {
    column: SharedString,
    note: Option<SharedString>,
    input: Entity<TextareaState>,
    save: Option<Save>,
}

impl EventEmitter<Dismissed> for ValueView {}

impl ValueView {
    pub fn new(request: ValueRequest, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx).default_value(&request.text));
        Self {
            column: request.column,
            note: request.note,
            input,
            save: request.save,
        }
    }

    /// Put the focus in the text box, where the user is about to type.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(save) = self.save.clone() else {
            return;
        };

        let value = self.input.read(cx).value().to_string();
        save(value, cx);
        cx.emit(Dismissed);
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(Dismissed);
    }

    fn on_close(&mut self, _: &CloseValue, _window: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
    }

    fn on_save(&mut self, _: &SaveValue, _window: &mut Window, cx: &mut Context<Self>) {
        self.save(cx);
    }

    /// The text as it stands, for a test to read.
    #[cfg(test)]
    pub(crate) fn value_for_test(&self, cx: &App) -> String {
        self.input.read(cx).value().to_string()
    }

    #[cfg(test)]
    pub(crate) fn column_for_test(&self) -> String {
        self.column.to_string()
    }

    #[cfg(test)]
    pub(crate) fn is_editable_for_test(&self) -> bool {
        self.save.is_some()
    }

    /// Type into the box, the way the user does.
    #[cfg(test)]
    pub(crate) fn set_value_for_test(&mut self, value: &str, window: &mut Window, cx: &mut App) {
        self.input.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
    }
}

impl Render for ValueView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.save.is_some();

        // The surrounding dialog owns the overlay and the size; this is the
        // body that fills it.
        v_flex()
            .id("value-dialog-body")
            .test_support()
            .size_full()
            .gap_2()
            .key_context("ValueDialog")
            .on_action(cx.listener(Self::on_close))
            .on_action(cx.listener(Self::on_save))
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(div().text_sm().child(self.column.clone()))
                    .when_some(self.note.clone(), |this, note| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(note),
                        )
                    }),
            )
            .child(
                // The box fills the dialog, so it grows with the window the
                // dialog sits in.
                div().flex_1().min_h_0().child(
                    Textarea::new(&self.input)
                        .h_full()
                        .readonly(!editable)
                        .font_family(settings::grid_font(cx)),
                ),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("close-value")
                            .ghost()
                            .small()
                            .label("Close")
                            .tooltip("Leave the value as it was")
                            .on_click(cx.listener(|this, _, _window, cx| this.close(cx))),
                    )
                    .when(editable, |this| {
                        this.child(
                            Button::new("save-value")
                                .primary()
                                .small()
                                .label("Save")
                                .tooltip("Stage this value in the grid")
                                .on_click(cx.listener(|this, _, _window, cx| this.save(cx))),
                        )
                    }),
            )
    }
}
