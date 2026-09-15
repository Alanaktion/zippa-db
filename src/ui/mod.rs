//! UI components.
//!
//! Each submodule is a GPUI view (`Entity<T>` where `T: Render`) that owns its
//! own state and notifies independently of the rest of the window.

pub mod data_grid;
pub mod filter_bar;
pub mod query_editor;
pub mod quick_switcher;
pub mod session;
pub mod settings_window;
pub mod sql_file;
pub mod table_view;
pub mod value_dialog;
pub mod welcome;

#[cfg(test)]
mod tests;

use gpui_kit::component::ActiveTheme;
use gpui_kit::{App, Hsla, rgb};

use crate::db::Engine;

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
