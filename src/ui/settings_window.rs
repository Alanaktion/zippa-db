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

use std::collections::HashMap;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::dialog::DialogButtonProps;
use gpui_kit::component::setting::{
    NumberFieldOptions, SettingField, SettingGroup, SettingItem, SettingPage,
    Settings as SettingsElement,
};
use gpui_kit::component::{
    ActiveTheme, Disableable, IconName, Root, Sizable, Theme, ThemeMode, WindowExt as _, h_flex,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Bounds, Context, FocusHandle, Global, SharedString, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, actions, div, px, size,
};

use crate::db::agent::presets::{self, CustomProvider, ProviderKey, Resolved};
use crate::db::history::{self, Keep};
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
                history_page(),
                agents_page(cx),
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

/// Whether each provider has a key in the keychain, by its stored key: read
/// once per provider, then kept up to date by the key box, so drawing the
/// page does not ask the keychain every frame.
static KEYS_SET: std::sync::LazyLock<std::sync::Mutex<HashMap<String, bool>>> =
    std::sync::LazyLock::new(Default::default);

fn key_is_set(provider: &Resolved) -> bool {
    let account = provider.key.as_string();
    let Ok(mut known) = KEYS_SET.lock() else {
        return false;
    };
    *known.entry(account.clone()).or_insert_with(|| {
        matches!(
            crate::db::store::api_key(&account, provider.legacy_key),
            Ok(Some(_))
        )
    })
}

fn note_key_set(account: String, set: bool) {
    if let Ok(mut known) = KEYS_SET.lock() {
        known.insert(account, set);
    }
}

/// One of the selected provider's text fields.
#[derive(Clone, Copy)]
enum ProviderField {
    Name,
    BaseUrl,
    Model,
}

/// The selected provider's value for `field`, as its box shows it: what the
/// user typed, or the preset's own value where they have typed nothing.
fn provider_field(field: ProviderField, settings: &Settings) -> SharedString {
    match ProviderKey::parse(&settings.agent_provider) {
        Some(ProviderKey::Preset(preset)) => {
            let choice = settings.agent_presets.get(preset.key);
            match field {
                ProviderField::Name => preset.name.into(),
                ProviderField::BaseUrl => match choice {
                    Some(choice) if preset.local => choice.base_url.clone().into(),
                    _ => preset.base_url.into(),
                },
                ProviderField::Model => choice
                    .map_or(preset.model, |choice| choice.model.as_str())
                    .to_string()
                    .into(),
            }
        }
        Some(ProviderKey::Custom(id)) => settings
            .agent_custom_providers
            .iter()
            .find(|custom| custom.id == id)
            .map(|custom| match field {
                ProviderField::Name => custom.name.clone(),
                ProviderField::BaseUrl => custom.base_url.clone(),
                ProviderField::Model => custom.model.clone(),
            })
            .unwrap_or_default()
            .into(),
        None => SharedString::default(),
    }
}

/// Write `field` of whichever provider is selected *now*.
///
/// A settings input keeps the setter it was first drawn with, and the same
/// boxes serve every provider, so the setter must not capture the provider
/// it was built for: after a switch it would write into the old one.
fn set_provider_field(field: ProviderField, value: &str, cx: &mut App) {
    settings::update(cx, |settings| {
        match ProviderKey::parse(&settings.agent_provider) {
            Some(ProviderKey::Preset(preset)) => {
                let value = value.trim();
                let existing = settings.agent_presets.get(preset.key);
                // A box showing the preset's own value is not a choice; storing
                // it would pin today's default.
                let default = match field {
                    ProviderField::Model => preset.model,
                    ProviderField::BaseUrl => preset.base_url,
                    ProviderField::Name => return,
                };
                if existing.is_none() && value == default {
                    return;
                }
                let choice = settings
                    .agent_presets
                    .entry(preset.key.to_string())
                    .or_default();
                match field {
                    ProviderField::Model => choice.model = value.to_string(),
                    ProviderField::BaseUrl if preset.local => choice.base_url = value.to_string(),
                    _ => {}
                }
            }
            Some(ProviderKey::Custom(id)) => {
                if let Some(custom) = settings
                    .agent_custom_providers
                    .iter_mut()
                    .find(|custom| custom.id == id)
                {
                    match field {
                        // Kept as typed: trimming here would eat the space
                        // just typed between two words.
                        ProviderField::Name => custom.name = value.to_string(),
                        ProviderField::BaseUrl => custom.base_url = value.trim().to_string(),
                        ProviderField::Model => custom.model = value.trim().to_string(),
                    }
                }
            }
            None => {}
        }
    })
}

fn provider_input(field: ProviderField) -> SettingField<SharedString> {
    SettingField::input(
        move |cx| provider_field(field, Settings::global(cx)),
        move |value, cx| set_provider_field(field, &value, cx),
    )
}

/// Save a key for whichever provider is selected now (see
/// [`set_provider_field`] for why it is not captured).
fn set_selected_key(value: &str, cx: &mut App) {
    if value == KEY_MASK {
        return;
    }
    let Some(provider) = Settings::global(cx).agent_choices().resolve() else {
        return;
    };
    let key = value.trim();
    let account = provider.key.as_string();
    if let Err(error) = crate::db::store::set_api_key(&account, provider.legacy_key, key) {
        eprintln!("could not save the API key: {error:#}");
        return;
    }
    note_key_set(account, !key.is_empty());
    // The key now lives under the provider's own account.
    if provider.legacy_key
        && let ProviderKey::Custom(id) = provider.key
    {
        settings::update(cx, |settings| {
            if let Some(custom) = settings
                .agent_custom_providers
                .iter_mut()
                .find(|custom| custom.id == id)
            {
                custom.legacy_key = false;
            }
        });
    }
}

/// Save a new custom provider and select it, so the boxes above fill in
/// for it.
fn add_custom_provider(cx: &mut App) {
    settings::update(cx, |settings| {
        let custom =
            CustomProvider::new(presets::next_custom_name(&settings.agent_custom_providers));
        settings.agent_provider = ProviderKey::Custom(custom.id).as_string();
        settings.agent_custom_providers.push(custom);
    });
}

/// Ask before deleting the selected custom provider and its key.
fn confirm_remove_custom_provider(window: &mut Window, cx: &mut App) {
    let Some(provider) = Settings::global(cx).agent_choices().resolve() else {
        return;
    };
    let ProviderKey::Custom(id) = provider.key else {
        return;
    };
    let title = format!("Remove {}?", provider.name);
    window.open_alert_dialog(cx, move |alert, _window, _cx| {
        let provider = provider.clone();
        alert
            .title(title.clone())
            .description("Its address, model, and saved API key are forgotten.")
            .button_props(
                DialogButtonProps::default()
                    .ok_text("Remove Provider")
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text("Cancel")
                    .show_cancel(true),
            )
            .on_ok(move |_, _window, cx| {
                let account = provider.key.as_string();
                if let Err(error) = crate::db::store::set_api_key(&account, provider.legacy_key, "")
                {
                    eprintln!("could not forget the API key: {error:#}");
                }
                note_key_set(account, false);
                settings::update(cx, |settings| {
                    settings
                        .agent_custom_providers
                        .retain(|custom| custom.id != id);
                    if settings.agent_provider == provider.key.as_string() {
                        // The provider that was picked before it is a
                        // better guess than none; the first preset will do.
                        settings.agent_provider = presets::PRESETS[0].key.to_string();
                    }
                });
                true
            })
    });
}

fn agents_page(cx: &App) -> SettingPage {
    let settings = Settings::global(cx);
    let choices = settings.agent_choices();
    let selected = choices.resolve();
    let preset = match selected.as_ref().map(|provider| provider.key) {
        Some(ProviderKey::Preset(preset)) => Some(preset),
        _ => None,
    };
    let custom = matches!(
        selected.as_ref().map(|provider| provider.key),
        Some(ProviderKey::Custom(_))
    );
    let options: Vec<(SharedString, SharedString)> = choices
        .options()
        .into_iter()
        .map(|(key, label)| (key.into(), label.into()))
        .collect();
    let key_set = selected.as_ref().is_some_and(key_is_set);

    let base_url_note = match preset {
        Some(preset) if preset.local => {
            "Where the server listens on this machine or your network.".to_string()
        }
        Some(preset) => format!("{} is always at this address.", preset.name),
        None => "Up to but not including /chat/completions.".to_string(),
    };
    let model_note = match preset {
        Some(preset) if !preset.model.is_empty() => {
            format!("Left empty, {} is used.", preset.model)
        }
        Some(_) => "The name of a model the server has.".to_string(),
        None => "Sent with each request as the model name.".to_string(),
    };
    let key_note = match preset {
        Some(preset) if preset.local => "Rarely needed locally. In the OS keychain.",
        Some(_) => "Kept in the OS keychain.",
        None => "Optional. Kept in the OS keychain.",
    };

    SettingPage::new("Agents")
        .icon(IconName::Bot)
        .group(
            // The items keep one order whatever is selected: an input's state
            // is keyed by its position, so moving a box between providers
            // would hand it another box's text.
            SettingGroup::new()
                .title("Model provider")
                .description(
                    "When you send a message in the Agents panel, schema names, query text, \
                     and result rows go to this provider. A local one (Ollama, LM Studio) \
                     keeps them on this machine; a hosted one is a data-sharing choice. \
                     Nothing is sent until you do.",
                )
                .item(
                    SettingItem::new(
                        "Provider",
                        SettingField::scrollable_dropdown(
                            options,
                            |cx| Settings::global(cx).agent_provider.clone().into(),
                            |value, cx| {
                                settings::update(cx, |settings| {
                                    settings.agent_provider = value.to_string()
                                })
                            },
                        ),
                    )
                    .description(
                        "Anthropic uses its own API; the rest use the OpenAI-compatible one.",
                    ),
                )
                .item(
                    SettingItem::new("Name", provider_input(ProviderField::Name))
                        .disabled(!custom)
                        .description(if custom {
                            "How this provider is listed."
                        } else {
                            "Built-in providers keep their names."
                        }),
                )
                .item(
                    SettingItem::new("Base URL", provider_input(ProviderField::BaseUrl))
                        .disabled(preset.is_some_and(|preset| !preset.local))
                        .description(base_url_note),
                )
                .item(
                    SettingItem::new("Model", provider_input(ProviderField::Model))
                        .description(model_note),
                )
                .item(
                    SettingItem::new(
                        "API key",
                        SettingField::input(
                            move |_| {
                                if key_set { KEY_MASK.into() } else { "".into() }
                            },
                            |value: SharedString, cx| set_selected_key(&value, cx),
                        ),
                    )
                    .description(key_note),
                )
                .item(
                    SettingItem::new(
                        "Custom providers",
                        SettingField::render(move |_options, _window, _cx| {
                            h_flex()
                                .gap_2()
                                .child(
                                    Button::new("add-custom-provider")
                                        .small()
                                        .outline()
                                        .label("Add Provider")
                                        .on_click(|_, _window, cx| add_custom_provider(cx)),
                                )
                                .child(
                                    Button::new("remove-custom-provider")
                                        .small()
                                        .danger()
                                        .label("Remove…")
                                        .disabled(!custom)
                                        .on_click(|_, window, cx| {
                                            confirm_remove_custom_provider(window, cx)
                                        }),
                                )
                        }),
                    )
                    .description(
                        "Save another OpenAI-compatible endpoint, or remove the selected one.",
                    ),
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

/// The age limits offered for query history, in days; `0` is no limit.
const HISTORY_DAYS: [(u32, &str); 5] = [
    (0, "Forever"),
    (7, "7 days"),
    (30, "30 days"),
    (90, "90 days"),
    (365, "1 year"),
];

fn history_page() -> SettingPage {
    SettingPage::new("History")
        .icon(gpui_kit::assets::IconName::Clock)
        .group(
            SettingGroup::new()
                .title("Query history")
                .description(
                    "What you run from query tabs, kept per connection and shown in its \
                     History tab (Database › Query History). The app's own reads and the \
                     Agents panel's statements are not kept. SQL text can hold anything \
                     you typed, so the files are readable only by you; passwords from \
                     connections are never written.",
                )
                .item(
                    SettingItem::new(
                        "Keep query history",
                        SettingField::dropdown(
                            Keep::ALL
                                .map(|keep| (keep.key().into(), keep.label().into()))
                                .to_vec(),
                            |cx| Settings::global(cx).query_history.key().into(),
                            |value, cx| {
                                settings::update(cx, |settings| {
                                    settings.query_history = Keep::from_key(&value)
                                })
                            },
                        )
                        .default_value(Keep::default().key()),
                    )
                    .description(
                        "\"Until the app quits\" keeps it in memory only. Changing this keeps \
                         what is already saved; clear it below.",
                    ),
                )
                .item(
                    SettingItem::new(
                        "Statements per connection",
                        SettingField::number_input(
                            NumberFieldOptions {
                                min: 0.,
                                max: history::MAX_ENTRIES_LIMIT as f64,
                                step: 100.,
                            },
                            |cx| Settings::global(cx).query_history_limit as f64,
                            |value, cx| {
                                settings::update(cx, |settings| {
                                    settings.query_history_limit =
                                        (value.max(0.) as usize).min(history::MAX_ENTRIES_LIMIT)
                                })
                            },
                        )
                        .default_value(history::DEFAULT_MAX_ENTRIES as f64),
                    )
                    .description("The oldest go first. 0 keeps every statement."),
                )
                .item(
                    SettingItem::new(
                        "Keep statements for",
                        SettingField::dropdown(
                            HISTORY_DAYS
                                .map(|(days, label)| (days.to_string().into(), label.into()))
                                .to_vec(),
                            |cx| Settings::global(cx).query_history_days.to_string().into(),
                            |value, cx| {
                                settings::update(cx, |settings| {
                                    settings.query_history_days = value.parse().unwrap_or(0)
                                })
                            },
                        )
                        .default_value("0"),
                    )
                    .description("Older statements are dropped."),
                )
                .item(
                    SettingItem::new(
                        "Clear all query history",
                        SettingField::render(|_options, _window, _cx| {
                            Button::new("clear-all-history")
                                .small()
                                .danger()
                                .label("Clear…")
                                .on_click(|_, window, cx| confirm_clear_history(window, cx))
                        }),
                    )
                    .description(
                        "Every connection's. A single connection's is cleared from its \
                         History tab; deleting a connection deletes its history.",
                    ),
                ),
        )
}

/// Ask before forgetting every connection's history: it cannot be brought
/// back.
fn confirm_clear_history(window: &mut Window, cx: &mut App) {
    window.open_alert_dialog(cx, |alert, _window, _cx| {
        alert
            .title("Clear all query history?")
            .description("Every statement recorded for every connection is forgotten.")
            .button_props(
                DialogButtonProps::default()
                    .ok_text("Clear History")
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text("Cancel")
                    .show_cancel(true),
            )
            .on_ok(|_, _window, cx| {
                crate::ui::history::clear_all(cx);
                true
            })
    });
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
