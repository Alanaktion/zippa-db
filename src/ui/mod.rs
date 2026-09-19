//! UI components.
//!
//! Each submodule is a GPUI view (`Entity<T>` where `T: Render`) that owns its
//! own state and notifies independently of the rest of the window.

pub mod data_grid;
pub mod filter_bar;
pub mod import_dialog;
pub mod plan_view;
pub mod query_editor;
pub mod quick_switcher;
pub mod schema_view;
pub mod session;
pub mod settings_window;
pub mod shortcuts_dialog;
pub mod sql_file;
pub mod table_view;
mod text_filter;
pub mod value_dialog;
pub mod welcome;

#[cfg(test)]
mod tests;

use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, Root, Theme, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, Hsla, SharedString, Window, div, rgb};

use crate::db::{Engine, TagColor};

/// Surface `message` as a toast, alongside whatever inline text already names
/// the error — a failure that lands while attention is elsewhere still gets
/// seen.
///
/// A bare test window has no `Root` mounted to show a toast in, and
/// `WindowExt::push_notification` panics on that rather than no-op, so this
/// checks first.
pub fn notify_error(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    if window.root::<Root>().flatten().is_some() {
        window.push_notification(Notification::error(message.into()), cx);
    }
}

/// The accent an engine badge is tinted with, from `assets/colors.md`.
///
/// Purely decorative — every place this is used also carries the engine's
/// name in text, so colour is never the only way to tell one connection from
/// another.
pub fn engine_color(engine: Engine, cx: &App) -> Hsla {
    let dark = cx.theme().mode.is_dark();
    match (engine, dark) {
        (Engine::MySql, true) => rgb(0xE0AF68).into(),
        (Engine::MySql, false) => rgb(0xD97706).into(),
        (Engine::Postgres, true) => rgb(0x7AA2F7).into(),
        (Engine::Postgres, false) => rgb(0x2563EB).into(),
        (Engine::Sqlite, true) => rgb(0x7DCFFF).into(),
        (Engine::Sqlite, false) => rgb(0x0284C7).into(),
    }
}

/// A small pill carrying a connection's tag: its text always on its colour.
///
/// The tag text is the cue; the colour only reinforces it, so an untagged or
/// colourless tag still reads through the text or a neutral fill.
pub fn tag_chip(tag: &str, color: Option<TagColor>, cx: &App) -> impl IntoElement {
    let bg = color
        .map(|color| color.hsla(cx))
        .unwrap_or_else(|| cx.theme().muted);
    let fg = color
        .map(|color| color.on_color(cx))
        .unwrap_or_else(|| cx.theme().muted_foreground);

    div()
        .flex_none()
        .rounded_full()
        .px_2()
        .py_0p5()
        .text_xs()
        .bg(bg)
        .text_color(fg)
        .child(tag.to_string())
}

/// Resolve a [`TagColor`] to the colour it is drawn with.
///
/// Semantic colours (`danger`, `warning`, `success`, `info`) follow the active
/// theme; the rest of the palette uses fixed hues adjusted for light and dark.
/// The hue is user-chosen *data* rather than decoration, which is the one case
/// the theme system deliberately leaves to a fixed value.
impl TagColor {
    /// The fill colour for a chip, bar or strip carrying this tag.
    pub fn hsla(&self, cx: &App) -> Hsla {
        self.hsla_on(cx.theme())
    }

    /// A text colour that contrasts with [`TagColor::hsla`] in every theme.
    pub fn on_color(&self, cx: &App) -> Hsla {
        on_color_for(self.hsla_on(cx.theme()))
    }

    pub(crate) fn hsla_on(&self, theme: &Theme) -> Hsla {
        let dark = theme.mode.is_dark();
        match self {
            TagColor::Red => theme.danger,
            TagColor::Orange => theme.warning,
            TagColor::Green => theme.success,
            TagColor::Blue => theme.info,
            TagColor::Yellow => if dark { rgb(0xFACC15) } else { rgb(0xCA8A04) }.into(),
            TagColor::Teal => if dark { rgb(0x14B8A6) } else { rgb(0x0D9488) }.into(),
            TagColor::Purple => if dark { rgb(0xA78BFA) } else { rgb(0x7C3AED) }.into(),
            TagColor::Pink => if dark { rgb(0xF472B6) } else { rgb(0xBE185D) }.into(),
            TagColor::Gray => if dark { rgb(0x9CA3AF) } else { rgb(0x4B5563) }.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn on_color_for(&self, theme: &Theme) -> Hsla {
        on_color_for(self.hsla_on(theme))
    }
}

/// The black or white that reads best on `background`, whichever is closer to
/// a 4.5:1 ratio. A colour near the crossover point (a mid grey) fails against
/// both; the palette avoids that band and [`contrast`] verifies it.
fn on_color_for(background: Hsla) -> Hsla {
    let white = Hsla::white();
    let black = Hsla::black();
    if contrast(white, background) >= contrast(black, background) {
        white
    } else {
        black
    }
}

/// Relative luminance of an opaque colour, per WCAG 2.x.
pub(crate) fn relative_luminance(color: Hsla) -> f64 {
    let rgba = color.to_rgb();
    let channel = |component: f32| {
        let component = component as f64;
        if component <= 0.04045 {
            component / 12.92
        } else {
            ((component + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(rgba.r) + 0.7152 * channel(rgba.g) + 0.0722 * channel(rgba.b)
}

/// WCAG contrast ratio between two colours, in the range 1.0..=21.0.
pub(crate) fn contrast(a: Hsla, b: Hsla) -> f64 {
    let (lighter, darker) = if relative_luminance(a) >= relative_luminance(b) {
        (a, b)
    } else {
        (b, a)
    };
    (relative_luminance(lighter) + 0.05) / (relative_luminance(darker) + 0.05)
}
