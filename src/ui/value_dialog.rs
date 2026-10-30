//! A dialog for one cell's value.
//!
//! A value too big for a
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
use std::sync::Arc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, Entity, EventEmitter, Global, Image, ImageFormat, SharedString, Window, actions,
    div, img,
};

use crate::db::binary::{self, ImageKind};
use crate::settings;
use crate::ui::sql_file;

actions!(zippa_db, [CloseValue, SaveValue]);

/// What the dialog does with the text when the user saves it.
pub type Save = Rc<dyn Fn(String, &mut App)>;

/// What the dialog does with a file's bytes when the user saves them: stage
/// them on the binary cell the dialog opened on, the way [`Save`] stages
/// typed text. Only present on a binary dialog whose owner can write the
/// cell back — a query result or a read-only table gets none. Returns
/// whether the bytes were staged, so the dialog can stay open on a refusal
/// instead of dismissing with the file silently lost.
pub type SaveBinary = Rc<dyn Fn(Vec<u8>, &mut App) -> bool>;

/// What replaces the text box when there is something better to draw than a
/// string. The only case today is a binary value that decoded as an image —
/// see [`open_binary`].
#[derive(Clone)]
pub enum Preview {
    Image(Arc<Image>),
}

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
    /// Stages a loaded file's bytes on the cell, when the dialog offers
    /// that: only a binary dialog on a writable table carries one.
    pub save_binary: Option<SaveBinary>,
    /// Shown instead of the text box, when there is one.
    pub preview: Option<Preview>,
}

#[derive(Default)]
struct Pending(Option<ValueRequest>);

impl Global for Pending {}

/// Ask for `request` to be shown.
pub fn open(request: ValueRequest, cx: &mut App) {
    cx.set_global(Pending(Some(request)));
}

/// Show the real bytes behind a binary cell the grid could only describe: an
/// image, when the bytes decode as one of the common formats, a hex dump
/// otherwise. Replaces a dialog already open on the `<N bytes>` stand-in the
/// same way any other `open` does.
///
/// With `save_binary`, the dialog also offers to load a file: the loaded
/// bytes replace the preview, and saving stages them on the cell like a
/// typed edit. Without one — a query result, a read-only table, a type no
/// file can replace — the dialog stays read-only.
pub fn open_binary(
    column: SharedString,
    bytes: Vec<u8>,
    save_binary: Option<SaveBinary>,
    cx: &mut App,
) {
    let preview = binary::sniff_image(&bytes).map(|kind| {
        Preview::Image(Arc::new(Image::from_bytes(
            image_format(kind),
            bytes.clone(),
        )))
    });
    let text = match preview {
        Some(_) => format!("{} bytes, shown above", bytes.len()),
        None => binary::hex_dump(&bytes),
    };
    let note = if save_binary.is_some() {
        "Load a file to replace this value."
    } else {
        "This value cannot be edited here."
    };

    open(
        ValueRequest {
            column,
            text,
            note: Some(note.into()),
            save: None,
            save_binary,
            preview,
        },
        cx,
    );
}

fn image_format(kind: ImageKind) -> ImageFormat {
    match kind {
        ImageKind::Png => ImageFormat::Png,
        ImageKind::Jpeg => ImageFormat::Jpeg,
        ImageKind::Gif => ImageFormat::Gif,
        ImageKind::Webp => ImageFormat::Webp,
        ImageKind::Bmp => ImageFormat::Bmp,
    }
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
    save_binary: Option<SaveBinary>,
    preview: Option<Preview>,
    /// A file's bytes, loaded but not yet saved: saving stages these, not
    /// whatever the text box shows.
    staged_bytes: Option<Vec<u8>>,
    /// Which file load is the latest: an earlier read finishing after a
    /// later one is stale, and finishing it would stage the wrong file.
    load_seq: u64,
    /// A file load is in flight, so the Load button is dimmed until it
    /// lands — clicking again would only start a second read to race it.
    load_pending: bool,
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
            save_binary: request.save_binary,
            preview: request.preview,
            staged_bytes: None,
            load_seq: 0,
            load_pending: false,
        }
    }

    /// Put the focus in the text box, where the user is about to type.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        // A loaded file wins over the text box: the box holds a hex dump of
        // the new bytes for reading, not a value to stage.
        if let Some(bytes) = self.staged_bytes.clone() {
            if let Some(save_binary) = self.save_binary.clone() {
                // Staging can refuse — the table was reloaded since the
                // dialog opened, so the row it knew is gone — and then the
                // dialog stays open with the bytes still in hand, instead
                // of dismissing and losing the file without a word.
                if save_binary(bytes, cx) {
                    cx.emit(Dismissed);
                } else {
                    self.note = Some(
                        "The table changed since this value was opened, so the file was not staged. Reopen the value to try again."
                            .into(),
                    );
                    cx.notify();
                }
            }
            return;
        }

        let Some(save) = self.save.clone() else {
            return;
        };

        let value = self.input.read(cx).value().to_string();
        save(value, cx);
        cx.emit(Dismissed);
    }

    /// Mark a file load started and hand back its sequence number, so a
    /// completion can tell whether it is still the latest one.
    fn begin_load(&mut self) -> u64 {
        self.load_seq += 1;
        self.load_pending = true;
        self.load_seq
    }

    /// Ask for a file and show its bytes in place of the current value.
    /// Saving after that stages the file, not the text box.
    fn load_file(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.save_binary.is_none() {
            return;
        }

        let seq = self.begin_load();
        cx.notify();
        let prompt = sql_file::prompt_for_import(cx);

        cx.spawn(async move |this, cx| {
            let path = match prompt.await {
                Ok(Some(path)) => path,
                Ok(None) => {
                    this.update_in(cx, |this, _, cx| {
                        // A cancelled picker is not a load: the button
                        // frees up again, with nothing staged.
                        if seq == this.load_seq {
                            this.load_pending = false;
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
                Err(_) => {
                    this.update_in(cx, |this, _, cx| {
                        if seq == this.load_seq {
                            this.load_pending = false;
                            this.note = Some("Could not open the file picker.".into());
                            cx.notify();
                        }
                    })
                    .ok();
                    return;
                }
            };

            let bytes = cx.background_spawn(sql_file::read_bytes(path)).await;
            this.update_in(cx, |this, window, cx| match bytes {
                Ok(bytes) => this.finish_load(seq, bytes, window, cx),
                Err(_) => {
                    if seq == this.load_seq {
                        this.load_pending = false;
                        this.note = Some("Could not read that file.".into());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Replace the preview with `bytes`, ready for saving to stage them.
    /// A completion from an older load is ignored: the dialog already
    /// shows a newer file, and overwriting it would stage the wrong one.
    fn finish_load(
        &mut self,
        seq: u64,
        bytes: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if seq != self.load_seq {
            return;
        }
        self.load_pending = false;
        let (preview, text) = preview_for(&bytes);
        self.input.update(cx, |input, cx| {
            input.set_value(text, window, cx);
        });
        self.preview = preview;
        self.staged_bytes = Some(bytes);
        self.note = Some("Save to stage the loaded file in the grid.".into());
        cx.notify();
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

    /// Load a file's bytes, the way the picker would hand them over — the
    /// picker itself is the platform's, so tests drive this step directly.
    #[cfg(test)]
    pub(crate) fn load_bytes_for_test(
        &mut self,
        bytes: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let seq = self.begin_load();
        self.finish_load(seq, bytes, window, cx);
    }

    /// Start a file load the way [`Self::load_file`] does, without the
    /// picker: the sequence number a completion would carry.
    #[cfg(test)]
    pub(crate) fn begin_load_for_test(&mut self) -> u64 {
        self.begin_load()
    }

    /// Finish a file load started by [`Self::begin_load_for_test`], the way
    /// the read task's completion would.
    #[cfg(test)]
    pub(crate) fn finish_load_for_test(
        &mut self,
        seq: u64,
        bytes: Vec<u8>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.finish_load(seq, bytes, window, cx);
    }

    /// Whether a file load is in flight.
    #[cfg(test)]
    pub(crate) fn load_pending_for_test(&self) -> bool {
        self.load_pending
    }

    /// Whether the dialog offers to stage a loaded file.
    #[cfg(test)]
    pub(crate) fn has_binary_save_for_test(&self) -> bool {
        self.save_binary.is_some()
    }

    /// The loaded file's bytes, when one was loaded.
    #[cfg(test)]
    pub(crate) fn staged_bytes_for_test(&self) -> Option<Vec<u8>> {
        self.staged_bytes.clone()
    }
}

/// What a fresh set of bytes looks like in the dialog: an image preview, or
/// a hex dump in the text box.
fn preview_for(bytes: &[u8]) -> (Option<Preview>, String) {
    let preview = binary::sniff_image(bytes).map(|kind| {
        Preview::Image(Arc::new(Image::from_bytes(
            image_format(kind),
            bytes.to_vec(),
        )))
    });
    let text = match preview {
        Some(_) => format!("{} bytes, shown above", bytes.len()),
        None => binary::hex_dump(bytes),
    };
    (preview, text)
}

impl Render for ValueView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editable = self.save.is_some();
        // Saving stages either the typed text or, once a file is loaded,
        // the file's bytes; before either, there is nothing to save.
        let saveable = editable || self.staged_bytes.is_some();

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
            // The box fills the dialog, so it grows with the window the dialog
            // sits in; an image preview does the same in its place.
            .child(match &self.preview {
                Some(Preview::Image(image)) => {
                    img(image.clone()).flex_1().min_h_0().into_any_element()
                }
                None => Textarea::new(&self.input)
                    .flex_1()
                    .min_h_0()
                    .readonly(!editable)
                    .font_family(settings::grid_font(cx))
                    .into_any_element(),
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .when(self.save_binary.is_some(), |this| {
                        this.child(
                            Button::new("load-value-file")
                                .ghost()
                                .small()
                                .disabled(self.load_pending)
                                .label("Load from file…")
                                .tooltip("Replace this value with a file's bytes")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.load_file(window, cx)),
                                ),
                        )
                    })
                    .child(
                        Button::new("close-value")
                            .ghost()
                            .small()
                            .label("Close")
                            .tooltip_with_action(
                                "Leave the value as it was",
                                &CloseValue,
                                Some("ValueDialog"),
                            )
                            .on_click(cx.listener(|this, _, _window, cx| this.close(cx))),
                    )
                    .when(saveable, |this| {
                        this.child(
                            Button::new("save-value")
                                .primary()
                                .small()
                                .label("Save")
                                .tooltip_with_action(
                                    "Stage this value in the grid",
                                    &SaveValue,
                                    Some("ValueDialog"),
                                )
                                .on_click(cx.listener(|this, _, _window, cx| this.save(cx))),
                        )
                    }),
            )
    }
}
