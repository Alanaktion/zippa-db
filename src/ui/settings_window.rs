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
use gpui_kit::component::{ActiveTheme, IconName, Root, Sizable, Theme, ThemeMode};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, FocusHandle, Global, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, actions, div, px, size,
};

use crate::settings::{self, Appearance, CompletionKey, Settings};

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
            // Small, like the rest of the app's chrome: the settings are a
            // short list to scan, not a form to fill in.
            .child(SettingsElement::new("settings-pages").small().pages([
                appearance_page(cx),
                editor_page(),
                data_page(),
                agents_page(),
            ]))
    }
}

fn appearance_page(cx: &App) -> SettingPage {
    SettingPage::new("Appearance")
        .icon(IconName::Palette)
        .group(
            SettingGroup::new()
                .title("Theme")
                .description("Theme files in the app's config directory are listed too.")
                .item(SettingItem::new(
                    "Appearance",
                    SettingField::dropdown(
                        Appearance::ALL
                            .map(|appearance| (appearance.key().into(), appearance.label().into()))
                            .to_vec(),
                        |cx| Settings::global(cx).appearance.key().into(),
                        |value, cx| {
                            settings::update(cx, |settings| {
                                settings.appearance = Appearance::from_key(&value)
                            })
                        },
                    )
                    .default_value(Appearance::default().key()),
                ))
                .item(theme_item(ThemeMode::Light, cx))
                .item(theme_item(ThemeMode::Dark, cx)),
        )
        .group(
            SettingGroup::new()
                .title("Fonts")
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
                .item(SettingItem::new(
                    "Striped rows",
                    SettingField::switch(
                        |cx| Settings::global(cx).stripe_rows,
                        |value, cx| settings::update(cx, |settings| settings.stripe_rows = value),
                    )
                    .default_value(true),
                ))
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
                    .description("Off follows the system setting."),
                ),
        )
        .group(
            SettingGroup::new()
                .title("Motion")
                .item(
                    SettingItem::new(
                        "Reduce motion",
                        SettingField::switch(
                            |cx| Settings::global(cx).reduce_motion,
                            |value, cx| {
                                settings::update(cx, |settings| settings.reduce_motion = value)
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "Dialog transitions and other animations settle immediately instead of animating.",
                    ),
                ),
        )
}

fn editor_page() -> SettingPage {
    SettingPage::new("Editor")
        .icon(IconName::SquareTerminal)
        .group(
            SettingGroup::new().title("Completion").item(
                SettingItem::new(
                    "Accept a suggestion with",
                    SettingField::dropdown(
                        CompletionKey::ALL
                            .map(|choice| (choice.key().into(), choice.label().into()))
                            .to_vec(),
                        |cx| Settings::global(cx).accept_completion.key().into(),
                        |value, cx| {
                            settings::update(cx, |settings| {
                                settings.accept_completion = CompletionKey::from_key(&value)
                            })
                        },
                    )
                    .default_value(CompletionKey::default().key()),
                )
                .description("The other key keeps its usual job."),
            ),
        )
        .group(
            SettingGroup::new()
                .title("Running")
                .item(
                    SettingItem::new(
                        "Run all queries in one transaction",
                        SettingField::switch(
                            |cx| Settings::global(cx).wrap_scripts_in_transaction,
                            |value, cx| {
                                settings::update(cx, |settings| {
                                    settings.wrap_scripts_in_transaction = value
                                })
                            },
                        )
                        .default_value(true),
                    )
                    .description("Off commits each statement as it runs."),
                )
                .item(
                    SettingItem::new(
                        "Limit SELECT results",
                        SettingField::switch(
                            |cx| Settings::global(cx).implicit_limit,
                            |value, cx| {
                                settings::update(cx, |settings| settings.implicit_limit = value)
                            },
                        )
                        .default_value(true),
                    )
                    .description("Adds a LIMIT to a SELECT that has no explicit limit."),
                )
                .item(
                    SettingItem::new(
                        "Run dashboard widgets in parallel",
                        SettingField::switch(
                            |cx| Settings::global(cx).parallel_dashboard_queries,
                            |value, cx| {
                                settings::update(cx, |settings| {
                                    settings.parallel_dashboard_queries = value
                                })
                            },
                        )
                        .default_value(false),
                    )
                    .description(
                        "On, each widget runs at once on its own connection. Off, they run one at a time.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Rows to limit to",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 1.,
                                max: settings::MAX_PAGE_SIZE as f64,
                                step: 100.,
                            },
                            |cx| Settings::global(cx).implicit_limit_rows as f64,
                            |value, cx| {
                                settings::update(cx, |settings| {
                                    settings.implicit_limit_rows =
                                        (value as usize).clamp(1, settings::MAX_PAGE_SIZE)
                                })
                            },
                        )
                        .default_value(settings::DEFAULT_PAGE_SIZE as f64),
                    )
                    .description("Can be overridden per query tab."),
                ),
        )
        .group(
            SettingGroup::new().title("Connections").item(
                SettingItem::new(
                    "Share one session across query tabs",
                    SettingField::switch(
                        |cx| Settings::global(cx).share_connection_across_buffers,
                        |value, cx| {
                            settings::update(cx, |settings| {
                                settings.share_connection_across_buffers = value
                            })
                        },
                    )
                    .default_value(false),
                )
                .description("Only one query runs at a time."),
            ),
        )
}

/// What the key box shows in place of a stored key. The key itself is never
/// read back into the window.
const KEY_MASK: &str = "••••••••";

static KEY_SET: std::sync::OnceLock<std::sync::atomic::AtomicBool> = std::sync::OnceLock::new();

fn key_set() -> &'static std::sync::atomic::AtomicBool {
    KEY_SET.get_or_init(|| {
        std::sync::atomic::AtomicBool::new(matches!(crate::db::store::api_key(), Ok(Some(_))))
    })
}

fn agents_page() -> SettingPage {
    SettingPage::new("Agents")
        .icon(IconName::Bot)
        .group(
            SettingGroup::new()
                .title("Model endpoint")
                .description(
                    "Any OpenAI-compatible chat endpoint. When you send a message in the \
                     Agents panel, schema names, query text, and result rows go to this \
                     endpoint. A local one (Ollama, LM Studio) keeps them on this machine; \
                     a hosted one is a data-sharing choice. Nothing is sent until you do.",
                )
                .item(SettingItem::new(
                    "Base URL",
                    SettingField::input(
                        |cx| Settings::global(cx).agent_base_url.clone().into(),
                        |value, cx| {
                            settings::update(cx, |settings| {
                                settings.agent_base_url = value.trim().to_string()
                            })
                        },
                    ),
                ))
                .item(SettingItem::new(
                    "Model",
                    SettingField::input(
                        |cx| Settings::global(cx).agent_model.clone().into(),
                        |value, cx| {
                            settings::update(cx, |settings| {
                                settings.agent_model = value.trim().to_string()
                            })
                        },
                    ),
                ))
                .item(
                    SettingItem::new(
                        "API key",
                        SettingField::input(
                            |_| {
                                if key_set().load(std::sync::atomic::Ordering::Relaxed) {
                                    KEY_MASK.into()
                                } else {
                                    "".into()
                                }
                            },
                            |value, _| {
                                if value.as_ref() == KEY_MASK {
                                    return;
                                }
                                let key = value.trim();
                                if crate::db::store::set_api_key(key).is_ok() {
                                    key_set().store(
                                        !key.is_empty(),
                                        std::sync::atomic::Ordering::Relaxed,
                                    );
                                }
                            },
                        ),
                    )
                    .description("Kept in the OS keychain, never in a settings file."),
                ),
        )
        .group(
            SettingGroup::new().title("Thread history").item(
                SettingItem::new(
                    "Save threads on this machine",
                    SettingField::switch(
                        |cx| Settings::global(cx).agent_save_threads,
                        |value, cx| {
                            settings::update(cx, |settings| settings.agent_save_threads = value)
                        },
                    )
                    .default_value(false),
                )
                .description(
                    "One file per thread, under this machine's config directory, \
                     listed in the Agents panel's Threads menu by recency. A thread \
                     can hold schema, row data, and pasted secrets, so it is only \
                     written with this on — and restoring one brings its transcript \
                     back, never a running turn or a waiting approval. Turning this off \
                     keeps the threads already saved; deleting a connection deletes its \
                     threads.",
                ),
            ),
        )
}

fn data_page() -> SettingPage {
    SettingPage::new("Data")
        .icon(IconName::LayoutDashboard)
        .group(
            SettingGroup::new()
                .title("Tables")
                .item(
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
                    .description("Tables already open keep their limit."),
                )
                .item(
                    SettingItem::new(
                        "Typing NULL stores SQL NULL",
                        SettingField::switch(
                            |cx| Settings::global(cx).coerce_null_literal,
                            |value, cx| {
                                settings::update(cx, |settings| {
                                    settings.coerce_null_literal = value
                                })
                            },
                        )
                        .default_value(true),
                    )
                    .description("Off stores the text NULL."),
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
