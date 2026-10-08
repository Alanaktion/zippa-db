//! Named queries the user saved for later, kept in
//! `<config_dir>/saved_queries.json` as a plain JSON array.

use std::fs;

use anyhow::{Context, Result};
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

fn file_path() -> Result<std::path::PathBuf> {
    Ok(store::config_dir()?.join(FILE_NAME))
}

/// Read the saved queries. An absent, unreadable, or corrupt file means no
/// saved queries yet — never an error.
pub fn load() -> Vec<SavedQuery> {
    let Ok(path) = file_path() else {
        return Vec::new();
    };
    let Ok(contents) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

/// Replace the saved queries on disk. Written through a temporary file and
/// renamed, so a crash cannot leave half a file behind.
pub fn save(queries: &[SavedQuery]) -> Result<()> {
    let path = file_path()?;
    let contents = serde_json::to_string_pretty(queries)?;
    let dir = path.parent().context("no config directory")?;
    fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let temporary = dir.join(format!("{FILE_NAME}.tmp"));
    store::write_restricted(&temporary, &contents)
        .with_context(|| format!("could not write {}", temporary.display()))?;
    if let Err(error) = fs::rename(&temporary, &path) {
        // Windows will not rename over an existing file: fall back to
        // writing in place rather than leaving nothing saved.
        store::write_restricted(&path, &contents)
            .with_context(|| format!("could not write {}: {error:#}", path.display()))?;
        let _ = fs::remove_file(&temporary);
    }
    Ok(())
}

/// Save `sql` under `name`, replacing any query already saved with that
/// name.
pub fn upsert(name: &str, sql: &str) -> Result<()> {
    let mut queries = load();
    match queries.iter_mut().find(|query| query.name == name) {
        Some(existing) => existing.sql = sql.to_string(),
        None => queries.push(SavedQuery {
            name: name.to_string(),
            sql: sql.to_string(),
        }),
    }
    save(&queries)
}

/// Forget the query saved as `name`, if any.
pub fn remove(name: &str) -> Result<()> {
    let queries: Vec<SavedQuery> = load()
        .into_iter()
        .filter(|query| query.name != name)
        .collect();
    save(&queries)
}

#[cfg(test)]
/// The storage tests (unit and UI alike) share one per-process scratch
/// config dir, so they take this lock for the whole test rather than
/// tripping over each other's files.
pub(crate) static FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// Start each storage test from an empty file. The tests share the
    /// per-process scratch config dir (see `FILE_LOCK`), so they clean up
    /// after themselves.
    fn reset() {
        if let Ok(path) = file_path() {
            let _ = fs::remove_file(path);
        }
    }

    #[test]
    fn round_trip() {
        let _lock = FILE_LOCK.lock().unwrap();
        reset();
        let queries = vec![
            SavedQuery {
                name: "Users".to_string(),
                sql: "SELECT * FROM users;".to_string(),
            },
            SavedQuery {
                name: "Counts".to_string(),
                sql: "SELECT count(*) FROM orders;".to_string(),
            },
        ];
        save(&queries).expect("save should succeed");
        assert_eq!(load(), queries);
        reset();
    }

    #[test]
    fn saving_under_an_existing_name_overwrites() {
        let _lock = FILE_LOCK.lock().unwrap();
        reset();
        upsert("Users", "SELECT 1;").expect("save should succeed");
        upsert("Users", "SELECT 2;").expect("save should succeed");
        let queries = load();
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].name, "Users");
        assert_eq!(queries[0].sql, "SELECT 2;");
        reset();
    }

    #[test]
    fn removing_a_query_forgets_it() {
        let _lock = FILE_LOCK.lock().unwrap();
        reset();
        upsert("Keep", "SELECT 1;").expect("save should succeed");
        upsert("Drop", "SELECT 2;").expect("save should succeed");
        remove("Drop").expect("remove should succeed");
        let queries = load();
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].name, "Keep");
        remove("Missing").expect("removing nothing is fine");
        assert_eq!(load().len(), 1);
        reset();
    }

    #[test]
    fn missing_or_corrupt_file_loads_empty() {
        let _lock = FILE_LOCK.lock().unwrap();
        reset();
        assert!(load().is_empty());
        let path = file_path().expect("config dir should resolve");
        fs::create_dir_all(path.parent().unwrap()).expect("scratch dir should create");
        fs::write(&path, "{ not json").expect("scratch file should write");
        assert!(load().is_empty());
        reset();
    }
}
