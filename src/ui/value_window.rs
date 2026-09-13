//! A window for one cell's value.
//!
//! TODO.md section 2 ("Specialized Cell Renderers"). A value too big for a
//! grid cell — JSON, several lines, a long string — is opened here instead,
//! in a text box that fills the window and grows with it. The box is where
//! editing a value in full will live, so the window takes what to do on save
//! from whoever opened it.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Root, Sizable, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, Entity, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowOptions, actions, div, px, size,
};

use crate::settings;

actions!(zippa_db, [CloseValue]);

const WINDOW_SIZE: (f32, f32) = (560., 420.);

/// What the window does with the text when the user saves it.
pub type Save = Rc<dyn Fn(String, &mut App)>;

/// The window that is open, so the next value reuses it rather than piling
/// another window on top.
struct OpenWindow(gpui_kit::WindowHandle<Root>, Entity<ValueView>);

impl gpui_kit::Global for OpenWindow {}

/// Show `value` in a window of its own, raising the one already open.
///
/// `save` is `None` for a value that cannot be written back — a query result,
/// a read-only connection, a value the driver only described — and the text
/// box is then read-only.
pub fn open(column: &str, value: &str, note: Option<&str>, save: Option<Save>, cx: &mut App) {
    let column = SharedString::from(column.to_string());
    let value = value.to_string();
    let note = note.map(|note| SharedString::from(note.to_string()));

    if let Some(OpenWindow(handle, view)) = cx.try_global::<OpenWindow>() {
        let (handle, view) = (*handle, view.clone());
        // Updating a closed window fails, which is how a stale handle is found.
        let raised = handle.update(cx, |_, window, cx| {
            view.update(cx, |view, cx| {
                view.show(&column, &value, note.clone(), save.clone(), window, cx)
            });
            window.activate_window();
        });

        if raised.is_ok() {
            return;
        }
    }

    let bounds = Bounds::centered(None, size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Value".into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| ValueView::new(&column, &value, note, save, window, cx));
        let root = cx.new(|cx| Root::new(view.clone(), window, cx));
        cx.set_global(OpenWindow(
            window
                .window_handle()
                .downcast::<Root>()
                .expect("just made"),
            view,
        ));
        root
    });

    if let Err(error) = opened {
        eprintln!("could not open the value window: {error:#}");
    }
}

pub struct ValueView {
    column: SharedString,
    /// Why the value cannot be edited, when it cannot.
    note: Option<SharedString>,
    input: Entity<TextareaState>,
    save: Option<Save>,
}

impl ValueView {
    fn new(
        column: &SharedString,
        value: &str,
        note: Option<SharedString>,
        save: Option<Save>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| TextareaState::new(window, cx).default_value(value));
        Self {
            column: column.clone(),
            note,
            input,
            save,
        }
    }

    /// Put another cell's value in the window that is already open.
    fn show(
        &mut self,
        column: &SharedString,
        value: &str,
        note: Option<SharedString>,
        save: Option<Save>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.column = column.clone();
        self.note = note;
        self.save = save;
        self.input.update(cx, |input, cx| {
            input.set_value(value.to_string(), window, cx)
        });
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(save) = self.save.clone() else {
            return;
        };

        let value = self.input.read(cx).value().to_string();
        save(value, cx);
        self.close(window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.remove_global::<OpenWindow>();
        window.remove_window();
    }

    fn on_close(&mut self, _: &CloseValue, window: &mut Window, cx: &mut Context<Self>) {
        self.close(window, cx);
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

    /// Press Save, the way the button does.
    #[cfg(test)]
    pub(crate) fn save_for_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save(window, cx);
    }
}

impl Render for ValueView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.save.is_some();

        v_flex()
            .size_full()
            .key_context("ValueWindow")
            .on_action(cx.listener(Self::on_close))
            .bg(cx.theme().background)
            .child(TitleBar::new().child(div().text_sm().child(self.column.clone())))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p_3()
                    .gap_2()
                    .when_some(self.note.clone(), |this, note| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(note),
                        )
                    })
                    .child(
                        // The box fills the window, so resizing the window is
                        // what resizes the box.
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
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.close(window, cx)),
                                    ),
                            )
                            .when(editable, |this| {
                                this.child(
                                    Button::new("save-value")
                                        .primary()
                                        .small()
                                        .label("Save")
                                        .tooltip("Stage this value in the grid")
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.save(window, cx)
                                            }),
                                        ),
                                )
                            }),
                    ),
            )
    }
}

/// The window that is open, for a test to read.
#[cfg(test)]
pub(crate) fn open_view(cx: &App) -> Option<Entity<ValueView>> {
    cx.try_global::<OpenWindow>().map(|open| open.1.clone())
}
