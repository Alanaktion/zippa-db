//! UI components.
//!
//! Each submodule is a GPUI view (`Entity<T>` where `T: Render`) that owns its
//! own state and notifies independently of the rest of the window.

pub mod agents;
pub mod backup_dialog;
pub mod busy;
pub mod completion;
pub mod connection_info;
pub mod console;
pub mod create_database_dialog;
pub mod data_grid;
pub mod engine;
pub mod erd;
pub mod filter_bar;
mod fuzzy;
pub mod history;
pub mod import_dialog;
pub mod layout;
pub mod plan_view;
pub mod process_list;
pub mod query_digest;
pub mod query_editor;
pub mod quick_switcher;
pub mod schema_search;
pub mod schema_view;
pub mod server_variables;
pub mod session;
pub mod settings_window;
pub mod shortcuts_dialog;
pub mod sql_file;
pub mod sqlite_maintenance;
pub mod table_view;
mod text_filter;
pub mod user_management;
pub mod value_dialog;
pub mod variables_dialog;
pub mod welcome;

#[cfg(test)]
mod tests;

use gpui_kit::assets::IconName;
use gpui_kit::component::Sizable;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme, Root, Theme, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClipboardItem, ElementId, Hsla, SharedString, Window, div, rgb};

use crate::db::TagColor;

pub use engine::{EngineLogos, engine_color, engine_icon};

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

/// The same, for something that went right — a copy, for one, which has no
/// other way to say it happened.
pub fn notify_info(window: &mut Window, cx: &mut App, message: impl Into<SharedString>) {
    if window.root::<Root>().flatten().is_some() {
        window.push_notification(Notification::info(message.into()), cx);
    }
}

/// A small Copy button beside an error, putting `message` on the clipboard
/// as the server or the app said it: the whole text, never the shown
/// excerpt, and without the `Error: ` the display adds. Error text is drawn
/// as plain text that cannot be selected, so this is the way to get it into
/// a search, a ticket, or the Agents panel.
pub fn copy_error_button(id: impl Into<ElementId>, message: impl Into<SharedString>) -> Button {
    let message: SharedString = message.into();
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(IconName::Copy)
        .accessibility_label("Copy error")
        .tooltip("Copy the error message")
        .on_click(move |_, window, cx| {
            cx.write_to_clipboard(ClipboardItem::new_string(copied_error(&message)));
            notify_info(window, cx, "Copied the error message");
        })
}

/// What [`copy_error_button`] copies: the message less any `Error: ` a
/// caller already put in front of it.
pub(crate) fn copied_error(message: &str) -> String {
    message
        .strip_prefix("Error: ")
        .unwrap_or(message)
        .trim()
        .to_string()
}

/// A ghost button whose text is the theme's muted colour: it reads as chrome
/// until hovered, for controls that should not compete with content.
///
/// Pass it to `.custom(..)`; `ButtonVariant` is a closed enum, so a named
/// variant is not something this crate can add.
pub fn subtle_button(cx: &App) -> ButtonCustomVariant {
    let theme = cx.theme();
    ButtonCustomVariant::new(cx)
        .foreground(theme.muted_foreground)
        .hover(theme.accent)
        .active(theme.secondary_active)
}

/// A small round dot in a connection's colour, for where the connection is
/// named in a line of text — the colour's name goes with the name's own
/// accessibility label, since the dot carries no text.
pub fn color_dot(color: TagColor, cx: &App) -> impl IntoElement + use<> {
    div().flex_none().size_2().rounded_full().bg(color.hsla(cx))
}

/// Resolve a [`TagColor`] to the colour it is drawn with.
///
/// Semantic colours (`danger`, `warning`, `success`, `info`) follow the active
/// theme; the rest of the palette uses fixed hues adjusted for light and dark.
/// The hue is user-chosen *data* rather than decoration, which is the one case
/// the theme system deliberately leaves to a fixed value.
impl TagColor {
    /// The fill colour for a dot, bar or strip carrying this colour.
    pub fn hsla(&self, cx: &App) -> Hsla {
        self.hsla_on(cx.theme())
    }

    /// The colour at a tenth of its strength, for a background it only tints.
    pub fn tint(&self, cx: &App) -> Hsla {
        self.hsla(cx).opacity(0.1)
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

#[cfg(test)]
mod copy_error_tests {
    use super::copied_error;

    #[test]
    fn a_copied_error_leaves_the_display_prefix_behind() {
        assert_eq!(copied_error("Error: no such table: x"), "no such table: x");
        assert_eq!(copied_error("no such table: x\n"), "no such table: x");
        assert_eq!(copied_error("Errors: 3"), "Errors: 3");
    }
}
