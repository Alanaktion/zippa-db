//! Loading indicators shared by every view that waits on the database.
//!
//! A spinner turns a bare "Loading…" line into something that visibly moves,
//! and the words stay beside it: motion is never the only cue that work is in
//! flight, the same rule colour follows elsewhere.

use gpui_kit::component::skeleton::Skeleton;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme, Sizable, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{App, Hsla, SharedString, div};

/// A small spinner in the muted foreground colour.
pub fn spinner(cx: &App) -> Spinner {
    Spinner::new().xsmall().color(cx.theme().muted_foreground)
}

/// A spinner followed by `text`, both in `color`.
pub fn busy_label(text: impl Into<SharedString>, color: Hsla) -> impl IntoElement {
    h_flex()
        .gap_1p5()
        .items_center()
        .text_color(color)
        .child(Spinner::new().xsmall().color(color))
        .child(div().child(text.into()))
}

/// `rows` grey bars of varied width standing in for a list that has not
/// arrived yet.
pub fn skeleton_rows(rows: usize) -> impl IntoElement {
    // Widths are fixed rather than random so a reload does not reshuffle them.
    const WIDTHS: [f32; 5] = [0.8, 0.6, 0.7, 0.5, 0.65];
    v_flex().gap_2().px_1().children((0..rows).map(|ix| {
        Skeleton::new()
            .h_3()
            .rounded_sm()
            .w(gpui_kit::relative(WIDTHS[ix % WIDTHS.len()]))
    }))
}

/// A scrim over the whole window with `message` and a spinner in a card at the
/// middle. It occludes the view beneath, so nothing there can be clicked
/// while the work it stands for is still running.
pub fn overlay(message: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("busy-overlay")
        .absolute()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.background.opacity(0.6))
        .occlude()
        .child(
            h_flex()
                .gap_2()
                .items_center()
                .px_5()
                .py_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .text_sm()
                .child(Spinner::new().small().color(theme.muted_foreground))
                .child(div().child(message.into())),
        )
}
