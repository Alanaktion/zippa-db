//! UI components.
//!
//! Each submodule is a GPUI view (`Entity<T>` where `T: Render`) that owns its
//! own state and notifies independently of the rest of the window.

pub mod data_grid;
pub mod filter_bar;
pub mod query_editor;
pub mod session;
pub mod settings_window;
pub mod sql_file;
pub mod table_view;
pub mod value_dialog;
pub mod welcome;

#[cfg(test)]
mod tests;
