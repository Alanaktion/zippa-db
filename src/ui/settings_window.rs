//! The settings window.
//!
//! A window of its own rather than a screen inside the main one: settings
//! apply to the whole app, and reaching them should not cost you the query you
//! were in the middle of. `OpenSettings` is a global action, so `Cmd`/`Ctrl` +
//! `,` works from the connection manager and from an open session alike.
//!
//! Every field reads and writes `settings::Settings` directly through the app,
//! so there is nothing to apply or confirm: the change is saved and in effect
//! as soon as it is made.

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings as SettingsElement,
};
use gpui_kit::component::{ActiveTheme, IconName, Root, Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, FocusHandle, Global, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, actions, div, px, size,
};

use crate::settings::{self, Appearance, Settings};

actions!(zippa_db, [OpenSettings, CloseSettings]);

const WINDOW_SIZE: (f32, f32) = (860., 620.);

/// The dropdown value standing for "whatever the theme uses".
const THEME_FONT: &str = "";

/// The open settings window, so a second `Cmd+,` raises it instead of opening
/// a second copy.
struct OpenWindow(WindowHandle<Root>);

impl Global for OpenWindow {}

/// Make `OpenSettings` work wherever the keystroke is pressed.
pub fn init(cx: &mut App) {
    cx.on_action(|_: &OpenSettings, cx| open(cx));
}

/// Show the settings window, raising the one already open.
pub fn open(cx: &mut App) {
    if let Some(OpenWindow(handle)) = cx.try_global::<OpenWindow>() {
        let handle = *handle;
        // Updating a closed window fails, which is how a stale handle is found.
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }

    let bounds = Bounds::centered(None, size(px(WINDOW_SIZE.0), px(WINDOW_SIZE.1)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some("Settings".into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let opened = cx.open_window(options, |window, cx| {
        let view = cx.new(|cx| SettingsView {
            focus: cx.focus_handle(),
        });
        // Something inside the window has to hold the focus for its keys to
        // reach `SettingsWindow`.
        let focus = view.read(cx).focus.clone();
        focus.focus(window, cx);
        cx.new(|cx| Root::new(view, window, cx))
    });

    match opened {
        Ok(handle) => cx.set_global(OpenWindow(handle)),
        Err(error) => eprintln!("could not open the settings window: {error:#}"),
    }
}

pub struct SettingsView {
    focus: FocusHandle,
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("settings")
            .test_support()
            .track_focus(&self.focus)
            .key_context("SettingsWindow")
            .on_action(|_: &CloseSettings, window, _cx| window.remove_window())
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(SettingsElement::new("settings-pages").pages([appearance_page(cx), data_page()]))
    }
}

fn appearance_page(cx: &App) -> SettingPage {
    SettingPage::new("Appearance")
        .icon(IconName::Palette)
        .description("How Zippa looks.")
        .group(
            SettingGroup::new()
                .title("Theme")
                .item(
                    SettingItem::new(
                        "Appearance",
                        SettingField::dropdown(
                            Appearance::ALL
                                .map(|appearance| {
                                    (appearance.key().into(), appearance.label().into())
                                })
                                .to_vec(),
                            |cx| Settings::global(cx).appearance.key().into(),
                            |value, cx| {
                                settings::update(cx, |settings| {
                                    settings.appearance = Appearance::from_key(&value)
                                })
                            },
                        )
                        .default_value(Appearance::default().key()),
                    )
                    .description("Matching the system follows it as it switches."),
                )
                .item(theme_item(ThemeMode::Light, cx))
                .item(theme_item(ThemeMode::Dark, cx)),
        )
        .group(
            SettingGroup::new()
                .title("Fonts")
                .description("Only monospaced families are a good fit for either.")
                .item(font_item(
                    "SQL editor",
                    |settings| settings.editor_font.clone(),
                    |settings, family| settings.editor_font = family,
                    cx,
                ))
                .item(font_item(
                    "Result grid",
                    |settings| settings.grid_font.clone(),
                    |settings, family| settings.grid_font = family,
                    cx,
                )),
        )
        .group(
            SettingGroup::new()
                .title("Result grid")
                .item(
                    SettingItem::new(
                        "Striped rows",
                        SettingField::switch(
                            |cx| Settings::global(cx).stripe_rows,
                            |value, cx| {
                                settings::update(cx, |settings| settings.stripe_rows = value)
                            },
                        )
                        .default_value(true),
                    )
                    .description("Tints every other row so a long row is easier to follow."),
                )
                .item(
                    SettingItem::new(
                        "Always show scrollbars",
                        SettingField::switch(
                            |cx| Settings::global(cx).always_show_scrollbars,
                            |value, cx| {
                                settings::update(cx, |settings| {
                                    settings.always_show_scrollbars = value
                                })
                            },
                        )
                        .default_value(true),
                    )
                    .description(
                        "Keeps every scrollbar on screen. A wide result is scrolled sideways \
                         by dragging one, which a bar that fades out cannot be. Turn this off \
                         to follow the system instead.",
                    ),
                ),
        )
}

fn data_page() -> SettingPage {
    SettingPage::new("Data").icon(IconName::LayoutDashboard).group(
        SettingGroup::new().title("Tables").item(
            SettingItem::new(
                "Rows per page",
                SettingField::number_input(
                    NumberFieldOptions {
                        min: 1.,
                        max: settings::MAX_PAGE_SIZE as f64,
                        step: 100.,
                    },
                    |cx| Settings::global(cx).page_size as f64,
                    |value, cx| {
                        settings::update(cx, |settings| {
                            settings.page_size =
                                (value as usize).clamp(1, settings::MAX_PAGE_SIZE)
                        })
                    },
                )
                .default_value(settings::DEFAULT_PAGE_SIZE as f64),
            )
            .description("Rows a table opened from the sidebar reads at a time. Tables already open keep the limit they were opened with."),
        )
        .item(
            SettingItem::new(
                "Typing NULL means SQL NULL",
                SettingField::switch(
                    |cx| Settings::global(cx).coerce_null_literal,
                    |value, cx| {
                        settings::update(cx, |settings| settings.coerce_null_literal = value)
                    },
                )
                .default_value(false),
            )
            .description("With this off, NULL typed into a cell is stored as the text; the Set Null command stores SQL NULL either way."),
        ),
    )
}

/// The theme picker for one mode, listing the themes that suit it.
fn theme_item(mode: ThemeMode, cx: &App) -> SettingItem {
    let options = settings::themes_for(mode, cx)
        .into_iter()
        .map(|name| (name.clone(), name))
        .collect();

    let title = match mode {
        ThemeMode::Light => "Light theme",
        ThemeMode::Dark => "Dark theme",
    };

    SettingItem::new(
        title,
        SettingField::scrollable_dropdown(
            options,
            // The theme in force names itself, so the built-in default shows
            // up as the chosen one until the user picks another.
            move |cx| match mode {
                ThemeMode::Light => Theme::global(cx).light_theme.name.clone(),
                ThemeMode::Dark => Theme::global(cx).dark_theme.name.clone(),
            },
            move |value, cx| {
                settings::update(cx, |settings| match mode {
                    ThemeMode::Light => settings.light_theme = Some(value),
                    ThemeMode::Dark => settings.dark_theme = Some(value),
                })
            },
        ),
    )
    .description("Theme files in the app's config directory are listed here too.")
}

/// A font picker over the families this machine has.
fn font_item(
    title: &'static str,
    chosen: fn(&Settings) -> Option<SharedString>,
    set: fn(&mut Settings, Option<SharedString>),
    cx: &App,
) -> SettingItem {
    let mut options: Vec<(SharedString, SharedString)> =
        vec![(THEME_FONT.into(), "Theme default".into())];
    options.extend(
        settings::installed_families(cx)
            .iter()
            .map(|family| (family.clone().into(), family.clone().into())),
    );

    SettingItem::new(
        title,
        SettingField::scrollable_dropdown(
            options,
            move |cx| chosen(Settings::global(cx)).unwrap_or(THEME_FONT.into()),
            move |value, cx| {
                let family = (!value.is_empty()).then_some(value);
                settings::update(cx, |settings| set(settings, family))
            },
        )
        .default_value(THEME_FONT),
    )
}
