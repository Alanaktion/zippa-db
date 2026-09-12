//! Application settings: one global, one JSON file.
//!
//! The settings live in a GPUI global so any view can read them without being
//! handed a copy, and in `settings.json` beside `connections.json` so they
//! survive a restart. [`update`] is the only way to change them: it writes the
//! file, puts the new theme into effect, and redraws every window.

use std::fs;
use std::path::PathBuf;
use std::sync::{LazyLock, OnceLock};

use anyhow::{Context as _, Result};
use gpui_kit::component::{ActiveTheme as _, Theme, ThemeMode, ThemeRegistry};
use gpui_kit::{App, Global, SharedString, Window, WindowAppearance};
use serde::{Deserialize, Serialize};

use crate::db::store;

const FILE_NAME: &str = "settings.json";

/// Directory under the config dir that extra theme files are read from.
const THEMES_DIR: &str = "themes";

/// Rows a table view asks for per page until the user says otherwise.
pub const DEFAULT_PAGE_SIZE: usize = 500;

/// Keeps a typo in the settings box from asking for the whole table.
pub const MAX_PAGE_SIZE: usize = 100_000;

/// Which theme mode the app follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the operating system.
    #[default]
    Auto,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Light, Self::Dark];

    /// Stable name, used as the dropdown's value and in the JSON file.
    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Match the system",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// The appearance a dropdown value names.
    pub fn from_key(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|appearance| appearance.key() == key)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
// Every field defaulted, so a file written by an older build still loads.
#[serde(default)]
pub struct Settings {
    /// Rows a newly opened table view reads per page.
    pub page_size: usize,
    pub appearance: Appearance,
    /// Theme used in light mode; `None` is the built-in one.
    pub light_theme: Option<SharedString>,
    /// Theme used in dark mode; `None` is the built-in one.
    pub dark_theme: Option<SharedString>,
    /// Font family for the SQL editor; `None` is the theme's monospace family.
    pub editor_font: Option<SharedString>,
    /// Font family for the result grid; `None` is the theme's monospace family.
    pub grid_font: Option<SharedString>,
    /// Whether typing `NULL` into a cell stages SQL `NULL` rather than the
    /// four characters. Off by default, so an edit means what it says.
    pub coerce_null_literal: bool,
    /// Whether the result grid tints every other row.
    pub stripe_rows: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            page_size: DEFAULT_PAGE_SIZE,
            appearance: Appearance::default(),
            light_theme: None,
            dark_theme: None,
            editor_font: None,
            grid_font: None,
            coerce_null_literal: false,
            stripe_rows: true,
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// The settings in force.
    ///
    /// Falls back to the defaults rather than panicking when [`init`] has not
    /// run, which is the case in tests that build a view directly.
    pub fn global(cx: &App) -> &Self {
        static FALLBACK: LazyLock<Settings> = LazyLock::new(Settings::default);
        cx.try_global::<Self>().unwrap_or(&FALLBACK)
    }

    /// The theme chosen for `mode`, if the user picked one.
    fn theme_for(&self, mode: ThemeMode) -> Option<&SharedString> {
        match mode {
            ThemeMode::Light => self.light_theme.as_ref(),
            ThemeMode::Dark => self.dark_theme.as_ref(),
        }
    }
}

/// Read the settings file and put it in force. Call once at startup.
pub fn init(cx: &mut App) {
    let settings = load().unwrap_or_else(|error| {
        // A file we cannot read should not keep the app from starting.
        eprintln!("could not read the settings: {error:#}");
        Settings::default()
    });

    cx.set_global(settings);
    apply(None, cx);
}

/// Change the settings, then save and apply the result.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    let mut settings = Settings::global(cx).clone();
    change(&mut settings);
    if settings == *Settings::global(cx) {
        return;
    }

    if let Err(error) = save(&settings) {
        eprintln!("could not save the settings: {error:#}");
    }
    cx.set_global(settings);

    apply(None, cx);
    // Fonts and page sizes are read while rendering, so every window is stale.
    cx.refresh_windows();
}

/// Put the current settings into effect.
fn apply(window: Option<&mut Window>, cx: &mut App) {
    let settings = Settings::global(cx).clone();

    for mode in [ThemeMode::Light, ThemeMode::Dark] {
        let Some(config) = settings
            .theme_for(mode)
            .and_then(|name| ThemeRegistry::global(cx).themes().get(name).cloned())
        else {
            continue;
        };

        match mode {
            ThemeMode::Light => Theme::global_mut(cx).light_theme = config,
            ThemeMode::Dark => Theme::global_mut(cx).dark_theme = config,
        }
    }

    // A window's own appearance is the reliable one on Linux, where asking the
    // app can fail; fall back to the app when there is no window yet.
    let system = window
        .as_ref()
        .map(|window| window.appearance())
        .unwrap_or_else(|| cx.window_appearance());

    Theme::change(mode_for(settings.appearance, system), window, cx);
}

/// The theme mode an appearance setting asks for on a system that is `system`.
pub fn mode_for(appearance: Appearance, system: WindowAppearance) -> ThemeMode {
    match appearance {
        Appearance::Auto => ThemeMode::from(system),
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    }
}

/// Follow the system when it switches between light and dark.
///
/// Subscribed to the window's appearance; a fixed light or dark setting simply
/// re-applies itself.
pub fn follow_system_appearance(window: &mut Window, cx: &mut App) {
    apply(Some(window), cx);
}

/// The font family for the SQL editor.
pub fn editor_font(cx: &App) -> SharedString {
    installed_or_default(Settings::global(cx).editor_font.as_ref(), cx)
}

/// The font family for the result grid.
pub fn grid_font(cx: &App) -> SharedString {
    installed_or_default(Settings::global(cx).grid_font.as_ref(), cx)
}

/// A chosen family if the machine still has it, else the theme's monospace one.
///
/// GPUI panics the first time it lays out text in a family it cannot find, so a
/// font uninstalled since it was picked must not reach the text system.
fn installed_or_default(chosen: Option<&SharedString>, cx: &App) -> SharedString {
    match chosen {
        Some(family) if installed_families(cx).iter().any(|name| name == family) => family.clone(),
        _ => cx.theme().mono_font_family.clone(),
    }
}

/// The font families this machine has, in name order.
///
/// Enumerating them costs around a hundred milliseconds and the answer cannot
/// change while the process runs, so it is worth doing once.
pub fn installed_families(cx: &App) -> &'static [String] {
    static FAMILIES: OnceLock<Vec<String>> = OnceLock::new();

    FAMILIES.get_or_init(|| {
        let mut families = cx.text_system().all_font_names();
        families.sort();
        families.dedup();
        families
    })
}

/// Add the themes the user dropped in `<config>/themes` to the registry.
///
/// Each file holds a theme set — `{"themes": [ … ]}` — in the same shape the
/// component library's own themes use. Call before [`init`], so a theme named
/// in the settings is there to be found.
pub fn load_user_themes(cx: &mut App) {
    let Ok(dir) = store::config_dir().map(|dir| dir.join(THEMES_DIR)) else {
        return;
    };
    // Having no themes of your own is the normal case, not an error.
    let Ok(entries) = fs::read_dir(&dir) else {
        return;
    };

    for path in entries.flatten().map(|entry| entry.path()) {
        if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
            continue;
        }

        let loaded = fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|contents| ThemeRegistry::global_mut(cx).load_themes_from_str(&contents));

        if let Err(error) = loaded {
            eprintln!("could not load the theme {}: {error:#}", path.display());
        }
    }
}

/// The themes available for `mode`, in the order the registry sorts them.
pub fn themes_for(mode: ThemeMode, cx: &App) -> Vec<SharedString> {
    ThemeRegistry::global(cx)
        .sorted_themes()
        .into_iter()
        .filter(|theme| theme.mode == mode)
        .map(|theme| theme.name.clone())
        .collect()
}

fn settings_file() -> Result<PathBuf> {
    Ok(store::config_dir()?.join(FILE_NAME))
}

/// Read the settings file. A missing file means "nothing changed yet".
fn load() -> Result<Settings> {
    let path = settings_file()?;
    if !path.exists() {
        return Ok(Settings::default());
    }

    let contents =
        fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("could not parse {}", path.display()))
}

#[cfg(not(test))]
fn save(settings: &Settings) -> Result<()> {
    let path = settings_file()?;
    let dir = path.parent().context("no config directory")?;
    fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;

    let contents = serde_json::to_string_pretty(settings)?;
    fs::write(&path, contents).with_context(|| format!("could not write {}", path.display()))
}

/// Under test the file is left alone: the settings the tests change are the
/// developer's own, and a test run should not rewrite them.
#[cfg(test)]
fn save(_settings: &Settings) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_the_system_and_a_choice_does_not() {
        assert_eq!(
            mode_for(Appearance::Auto, WindowAppearance::Dark),
            ThemeMode::Dark
        );
        assert_eq!(
            mode_for(Appearance::Auto, WindowAppearance::VibrantLight),
            ThemeMode::Light
        );
        assert_eq!(
            mode_for(Appearance::Light, WindowAppearance::Dark),
            ThemeMode::Light
        );
        assert_eq!(
            mode_for(Appearance::Dark, WindowAppearance::Light),
            ThemeMode::Dark
        );
    }

    #[test]
    fn a_file_from_an_older_build_keeps_its_settings() {
        // Only the field that build knew about; the rest take their defaults.
        let settings: Settings =
            serde_json::from_str(r#"{"page_size": 50}"#).expect("the file should still parse");

        assert_eq!(settings.page_size, 50);
        assert_eq!(settings.appearance, Appearance::Auto);
        assert_eq!(settings.editor_font, None);
    }

    #[test]
    fn the_appearance_survives_a_round_trip_through_the_file() {
        for appearance in Appearance::ALL {
            let written = serde_json::to_string(&appearance).expect("appearance should serialize");
            assert_eq!(written, format!("\"{}\"", appearance.key()));
            assert_eq!(Appearance::from_key(appearance.key()), appearance);
        }
    }
}
