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

/// Bars standing in for a chart that has not arrived yet: columns of varied
/// height rising from a shared baseline, filling the space the chart will.
pub fn skeleton_chart() -> impl IntoElement {
    // Fixed heights, as in `skeleton_rows`, so a reload does not reshuffle.
    const HEIGHTS: [f32; 10] = [0.45, 0.7, 0.55, 0.85, 0.6, 0.4, 0.75, 0.5, 0.65, 0.35];
    h_flex()
        .size_full()
        .gap_2()
        .px_2()
        .items_end()
        .children(HEIGHTS.iter().map(|height| {
            Skeleton::new()
                .flex_1()
                .h(gpui_kit::relative(*height))
                .rounded_sm()
        }))
}

/// A header row and `rows` rows of `columns` cells standing in for a data
/// grid that has not arrived yet, each cell a bar of varied width.
pub fn skeleton_grid(rows: usize, columns: usize) -> impl IntoElement {
    const WIDTHS: [f32; 5] = [0.8, 0.55, 0.7, 0.45, 0.65];
    v_flex()
        .size_full()
        .gap_3()
        .p_2()
        .children((0..=rows).map(move |row| {
            h_flex().gap_3().children((0..columns).map(move |column| {
                // The header row is a little taller and darker than the body.
                let bar = Skeleton::new()
                    .h(if row == 0 {
                        gpui_kit::px(14.)
                    } else {
                        gpui_kit::px(10.)
                    })
                    .rounded_sm()
                    .w(gpui_kit::relative(WIDTHS[(row + column) % WIDTHS.len()]));
                div()
                    .flex_1()
                    .min_w_0()
                    .child(if row == 0 { bar } else { bar.secondary() })
            }))
        }))
}
