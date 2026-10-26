//! Named dashboards the user saved for later, kept in
//! `<config_dir>/dashboards.json` as a plain JSON array.
//!
//! The list is one GPUI global, [`Dashboards`], read from disk once and
//! shared by every window — the same arrangement as the saved queries
//! (`saved_queries::SavedQueries`). The sidebar's Saved tab draws from it
//! rather than reading the file per frame, and every change writes the whole
//! list through [`store::write_atomic`] on the background executor, so the
//! last change made is the last one on disk.
//!
//! A dashboard is self-contained: each widget carries its own SQL, so
//! renaming or deleting a saved query can never break one. No result data
//! is stored in a dashboard — opening one runs its widgets' queries fresh.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;
use gpui_kit::{App, AppContext as _, Global};
use serde::{Deserialize, Serialize};

use crate::db::store;
use crate::ui::session::chart::ChartKind;

const FILE_NAME: &str = "dashboards.json";

/// A dashboard the user named and kept. Names are unique: saving under an
/// existing name replaces that dashboard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dashboard {
    pub name: String,
    pub widgets: Vec<Widget>,
    /// Dashboard-global variable values: one value per name, shared by
    /// every widget whose SQL references it.
    pub variables: BTreeMap<String, String>,
}

/// One widget: its own SQL, and how its result is shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Widget {
    /// The title the user gave it; none shows a number and the SQL's first
    /// line instead. Files from before widgets could be named read back
    /// unnamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// How much of a row it takes; none leaves it to the display (a data
    /// grid is as wide as its columns, a chart is not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<WidgetWidth>,
    pub sql: String,
    pub display: WidgetDisplay,
}

/// How much of a dashboard row a widget takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetWidth {
    Half,
    Full,
}

impl Widget {
    /// The width it is drawn at: the one chosen, else full for a data grid
    /// and half for a chart.
    pub fn effective_width(&self) -> WidgetWidth {
        self.width.unwrap_or(match self.display {
            WidgetDisplay::Grid => WidgetWidth::Full,
            WidgetDisplay::Chart { .. } => WidgetWidth::Half,
        })
    }
}

/// How a widget shows its result. A chart's column picks are names, not
/// indices: indices break silently when a query's output columns change,
/// while a missing name is the widget's own "column not found" state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WidgetDisplay {
    Chart {
        kind: ChartKind,
        x: Option<String>,
        y: Vec<String>,
    },
    Grid,
}

/// The saved dashboards, one copy for the whole app.
pub struct Dashboards {
    list: Vec<Dashboard>,
    /// Why the file could not be read, or the last change could not be
    /// written; the Saved tab shows it until a later write succeeds.
    error: Option<String>,
}

impl Global for Dashboards {}

impl Dashboards {
    /// The list, read from disk the first time anything asks.
    pub fn get(cx: &mut App) -> &[Dashboard] {
        Self::ensure(cx);
        &cx.global::<Self>().list
    }

    /// The dashboard `name`, if one is saved under it.
    pub fn find(cx: &mut App, name: &str) -> Option<Dashboard> {
        Self::get(cx)
            .iter()
            .find(|dashboard| dashboard.name == name)
            .cloned()
    }

    /// What went wrong reading or writing the file, if anything.
    pub fn error(cx: &mut App) -> Option<String> {
        Self::ensure(cx);
        cx.global::<Self>().error.clone()
    }

    fn ensure(cx: &mut App) {
        if !cx.has_global::<Self>() {
            let (list, error) = match load() {
                Ok(list) => (list, None),
                Err(error) => (Vec::new(), Some(format!("{error:#}"))),
            };
            cx.set_global(Self { list, error });
        }
    }
}

/// Save `dashboard`, replacing any dashboard already saved under its name.
pub fn upsert(dashboard: &Dashboard, cx: &mut App) {
    change(cx, |dashboards| {
        match dashboards
            .iter_mut()
            .find(|saved| saved.name == dashboard.name)
        {
            Some(existing) => *existing = dashboard.clone(),
            None => dashboards.push(dashboard.clone()),
        }
    });
}

/// Forget the dashboard saved as `name`, if any.
pub fn remove(name: &str, cx: &mut App) {
    change(cx, |dashboards| {
        dashboards.retain(|dashboard| dashboard.name != name)
    });
}

/// Change the one list, then write it in the background. Observers of the
/// global — every session's sidebar — redraw.
fn change(cx: &mut App, change: impl FnOnce(&mut Vec<Dashboard>)) {
    Dashboards::ensure(cx);
    let saved = cx.global_mut::<Dashboards>();
    change(&mut saved.list);
    let dashboards = saved.list.clone();
    // Drawn here, in the order the changes were made, so a slow write
    // cannot land after a later one.
    let ticket = store::ticket();

    let written = cx.background_spawn(async move { save(&dashboards, ticket) });
    cx.spawn(async move |cx| {
        let error = written.await.err().map(|error| format!("{error:#}"));
        cx.update(|cx| cx.global_mut::<Dashboards>().error = error);
    })
    .detach();
}

fn file_path() -> Result<PathBuf> {
    Ok(store::config_dir()?.join(FILE_NAME))
}

/// Read the saved dashboards: none when there is no file yet. A file that
/// cannot be parsed is moved aside (see [`store::load_json`]) rather than
/// replaced by the next save.
pub(crate) fn load() -> Result<Vec<Dashboard>> {
    store::load_json(&file_path()?)
}

/// Replace the saved dashboards on disk, whole or not at all.
fn save(dashboards: &[Dashboard], ticket: store::Ticket) -> Result<()> {
    let contents = serde_json::to_string_pretty(dashboards)?;
    store::write_atomic(&file_path()?, &contents, ticket)
}

#[cfg(test)]
/// The storage tests (unit and UI alike) share one per-process scratch
/// config dir, so they take this lock for the whole test rather than
/// tripping over each other's files.
pub(crate) static FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn reset_for_test() {
    if let Ok(path) = file_path() {
        let _ = std::fs::remove_file(&path);
        let mut aside = path.into_os_string();
        aside.push(".unreadable");
        let _ = std::fs::remove_file(aside);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::session::chart::ChartKind;

    fn dashboard(name: &str) -> Dashboard {
        Dashboard {
            name: name.to_string(),
            widgets: vec![
                Widget {
                    name: Some("Scores".to_string()),
                    width: Some(WidgetWidth::Full),
                    sql: "SELECT name, score FROM items;".to_string(),
                    display: WidgetDisplay::Chart {
                        kind: ChartKind::Line,
                        x: Some("name".to_string()),
                        y: vec!["score".to_string()],
                    },
                },
                Widget {
                    name: None,
                    width: None,
                    sql: "SELECT id, name FROM items;".to_string(),
                    display: WidgetDisplay::Grid,
                },
            ],
            variables: BTreeMap::from([("who".to_string(), "alpha".to_string())]),
        }
    }

    #[test]
    fn round_trip() {
        let _lock = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let dashboards = vec![dashboard("Sales"), dashboard("Ops")];
        save(&dashboards, store::ticket()).expect("save should succeed");
        assert_eq!(load().expect("load should succeed"), dashboards);
        reset_for_test();
    }

    /// A file written before widgets had names still loads, and an unnamed
    /// widget does not write a `name` key back.
    #[test]
    fn a_widget_without_a_name_reads_back_unnamed() {
        let widget: Widget =
            serde_json::from_str(r#"{"sql":"SELECT 1","display":"Grid"}"#).expect("should parse");
        assert_eq!(widget.name, None);
        let written = serde_json::to_string(&widget).expect("should write");
        assert!(!written.contains("name"), "{written}");
    }

    #[gpui_kit::test]
    fn saving_under_an_existing_name_overwrites(cx: &mut gpui_kit::TestAppContext) {
        let _lock = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        cx.update(|cx| {
            upsert(&dashboard("Sales"), cx);
            let mut changed = dashboard("Sales");
            changed.widgets.clear();
            upsert(&changed, cx);
        });
        cx.run_until_parked();
        let expected = vec![{
            let mut changed = dashboard("Sales");
            changed.widgets.clear();
            changed
        }];
        cx.update(|cx| assert_eq!(Dashboards::get(cx), expected.as_slice()));
        assert_eq!(load().expect("load should succeed"), expected);
        reset_for_test();
    }

    #[gpui_kit::test]
    fn removing_a_dashboard_forgets_it(cx: &mut gpui_kit::TestAppContext) {
        let _lock = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        cx.update(|cx| {
            upsert(&dashboard("Keep"), cx);
            upsert(&dashboard("Drop"), cx);
            remove("Drop", cx);
            remove("Missing", cx);
        });
        cx.run_until_parked();
        let expected = vec![dashboard("Keep")];
        cx.update(|cx| assert_eq!(Dashboards::get(cx), expected.as_slice()));
        assert_eq!(load().expect("load should succeed"), expected);
        reset_for_test();
    }

    /// A file that does not parse is kept aside and reported, so the next
    /// save cannot replace every dashboard the user had with one.
    #[gpui_kit::test]
    fn a_corrupt_file_is_kept_and_reported(cx: &mut gpui_kit::TestAppContext) {
        let _lock = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let path = file_path().expect("config dir should resolve");
        std::fs::create_dir_all(path.parent().unwrap()).expect("scratch dir should create");
        std::fs::write(&path, "{ not json").expect("scratch file should write");

        let (list, error) = cx.update(|cx| (Dashboards::get(cx).to_vec(), Dashboards::error(cx)));
        assert!(list.is_empty());
        assert!(
            error.is_some_and(|error| error.contains("unreadable")),
            "the error should say where the file went"
        );
        let mut aside = path.clone().into_os_string();
        aside.push(".unreadable");
        assert_eq!(
            std::fs::read_to_string(aside).expect("the file should be kept aside"),
            "{ not json"
        );
        reset_for_test();
    }
}
