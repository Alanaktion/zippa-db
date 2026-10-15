//! Named queries the user saved for later, kept in
//! `<config_dir>/saved_queries.json` as a plain JSON array.
//!
//! The list is one GPUI global, [`SavedQueries`], read from disk once and
//! shared by every window — the same arrangement as the saved connections
//! (`ui::welcome::SavedConnections`). The sidebar's Saved tab draws from it
//! rather than reading the file per frame, and every change writes the whole
//! list through [`store::write_atomic`] on the background executor, so the
//! last change made is the last one on disk.

use std::path::PathBuf;

use anyhow::Result;
use gpui_kit::{App, AppContext as _, Global};
use serde::{Deserialize, Serialize};

use crate::db::store;

const FILE_NAME: &str = "saved_queries.json";

/// A query the user named and kept. Names are unique: saving under an
/// existing name replaces that query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub name: String,
    pub sql: String,
}

/// The saved queries, one copy for the whole app.
pub struct SavedQueries {
    list: Vec<SavedQuery>,
    /// Why the file could not be read, or the last change could not be
    /// written; the Saved tab shows it until a later write succeeds.
    error: Option<String>,
}

impl Global for SavedQueries {}

impl SavedQueries {
    /// The list, read from disk the first time anything asks.
    pub fn get(cx: &mut App) -> &[SavedQuery] {
        Self::ensure(cx);
        &cx.global::<Self>().list
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

/// Save `sql` under `name`, replacing any query already saved with that
/// name.
pub fn upsert(name: &str, sql: &str, cx: &mut App) {
    change(cx, |queries| {
        match queries.iter_mut().find(|query| query.name == name) {
            Some(existing) => existing.sql = sql.to_string(),
            None => queries.push(SavedQuery {
                name: name.to_string(),
                sql: sql.to_string(),
            }),
        }
    });
}

/// Forget the query saved as `name`, if any.
pub fn remove(name: &str, cx: &mut App) {
    change(cx, |queries| queries.retain(|query| query.name != name));
}

/// Change the one list, then write it in the background. Observers of the
/// global — every session's sidebar — redraw.
fn change(cx: &mut App, change: impl FnOnce(&mut Vec<SavedQuery>)) {
    SavedQueries::ensure(cx);
    let saved = cx.global_mut::<SavedQueries>();
    change(&mut saved.list);
    let queries = saved.list.clone();
    // Drawn here, in the order the changes were made, so a slow write
    // cannot land after a later one.
    let ticket = store::ticket();

    let written = cx.background_spawn(async move { save(&queries, ticket) });
    cx.spawn(async move |cx| {
        let error = written.await.err().map(|error| format!("{error:#}"));
        cx.update(|cx| cx.global_mut::<SavedQueries>().error = error);
    })
    .detach();
}

fn file_path() -> Result<PathBuf> {
    Ok(store::config_dir()?.join(FILE_NAME))
}

/// Read the saved queries: none when there is no file yet. A file that
/// cannot be parsed is moved aside (see [`store::load_json`]) rather than
/// replaced by the next save.
pub(crate) fn load() -> Result<Vec<SavedQuery>> {
    store::load_json(&file_path()?)
}

/// Replace the saved queries on disk, whole or not at all.
fn save(queries: &[SavedQuery], ticket: store::Ticket) -> Result<()> {
    let contents = serde_json::to_string_pretty(queries)?;
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

    fn query(name: &str, sql: &str) -> SavedQuery {
        SavedQuery {
            name: name.to_string(),
            sql: sql.to_string(),
        }
    }

    #[test]
    fn round_trip() {
        let _lock = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let queries = vec![
            query("Users", "SELECT * FROM users;"),
            query("Counts", "SELECT count(*) FROM orders;"),
        ];
        save(&queries, store::ticket()).expect("save should succeed");
        assert_eq!(load().expect("load should succeed"), queries);
        reset_for_test();
    }

    #[gpui_kit::test]
    fn saving_under_an_existing_name_overwrites(cx: &mut gpui_kit::TestAppContext) {
        let _lock = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        cx.update(|cx| {
            upsert("Users", "SELECT 1;", cx);
            upsert("Users", "SELECT 2;", cx);
        });
        cx.run_until_parked();
        let expected = vec![query("Users", "SELECT 2;")];
        cx.update(|cx| assert_eq!(SavedQueries::get(cx), expected.as_slice()));
        assert_eq!(load().expect("load should succeed"), expected);
        reset_for_test();
    }

    #[gpui_kit::test]
    fn removing_a_query_forgets_it(cx: &mut gpui_kit::TestAppContext) {
        let _lock = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        cx.update(|cx| {
            upsert("Keep", "SELECT 1;", cx);
            upsert("Drop", "SELECT 2;", cx);
            remove("Drop", cx);
            remove("Missing", cx);
        });
        cx.run_until_parked();
        let expected = vec![query("Keep", "SELECT 1;")];
        cx.update(|cx| assert_eq!(SavedQueries::get(cx), expected.as_slice()));
        assert_eq!(load().expect("load should succeed"), expected);
        reset_for_test();
    }

    /// A file that does not parse is kept aside and reported, so the next
    /// save cannot replace every query the user had with one.
    #[gpui_kit::test]
    fn a_corrupt_file_is_kept_and_reported(cx: &mut gpui_kit::TestAppContext) {
        let _lock = FILE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset_for_test();
        let path = file_path().expect("config dir should resolve");
        std::fs::create_dir_all(path.parent().unwrap()).expect("scratch dir should create");
        std::fs::write(&path, "{ not json").expect("scratch file should write");

        let (list, error) =
            cx.update(|cx| (SavedQueries::get(cx).to_vec(), SavedQueries::error(cx)));
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
