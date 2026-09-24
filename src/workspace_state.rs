//! Reopening the tabs the window had at the last quit.
//!
//! The shape of the window — which connections were open, what tabs each one
//! held, and which of them was in front — is written to `workspace.json` beside
//! `connections.json`. No password is stored here: only connection ids, which
//! are already in `connections.json`, and the tabs' own contents.
//!
//! State is written at checkpoints rather than on every keystroke (a save is
//! asked for when a tab is opened, closed, switched to, run, or saved), and the
//! final buffer text is flushed on quit. A file that fails to parse is reported
//! and treated as empty; it must never keep the app from starting.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::db::{DatabaseObject, store};

const FILE_NAME: &str = "workspace.json";

/// The shape of the file this build writes.
///
/// A newer file still reads — an unknown field is ignored and a missing one
/// takes its default — so this is a marker rather than a gate.
pub const VERSION: u32 = 1;

/// The window's tabs as they were, ready to be restored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WorkspaceState {
    pub version: u32,
    /// Index into `sessions` of the workspace tab that was in front.
    pub active: usize,
    /// One per connection tab, in tab order.
    pub sessions: Vec<SessionState>,
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            version: VERSION,
            active: 0,
            sessions: Vec::new(),
        }
    }
}

/// One open connection, and the tabs it held.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionState {
    pub connection: Uuid,
    /// The database the session was on, since the user may have switched away
    /// from the config's own.
    pub database: Option<String>,
    /// Index into `panels` of the tab that was in front.
    pub active: usize,
    pub panels: Vec<PanelState>,
}

/// One tab of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "panel", rename_all = "snake_case")]
pub enum PanelState {
    Query {
        title: String,
        sql: String,
        /// The file the buffer was bound to, if any.
        #[serde(default)]
        file: Option<PathBuf>,
    },
    Table {
        object: DatabaseObject,
    },
    Schema {
        object: DatabaseObject,
    },
    /// The console tab. Carries nothing of its own — the log it shows lives
    /// on the connection, not on disk — so restoring one just reopens it.
    Console,
    /// A panel kind a newer build wrote. It is skipped rather than failing the
    /// whole file, so a newer version's workspace still opens in an older one.
    #[serde(other)]
    Unknown,
}

fn workspace_file() -> Result<PathBuf> {
    Ok(store::config_dir()?.join(FILE_NAME))
}

/// Read the workspace file. A missing file means "nothing to restore".
pub fn load() -> Result<WorkspaceState> {
    let path = workspace_file()?;
    if !path.exists() {
        return Ok(WorkspaceState::default());
    }

    let contents =
        fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
    serde_json::from_str(&contents).with_context(|| format!("could not parse {}", path.display()))
}

/// Write the workspace file, replacing whatever was there.
///
/// Written beside the real file and renamed into place, so a kill part-way
/// through the write cannot leave a truncated file for the next launch.
#[cfg(not(test))]
pub fn save(state: &WorkspaceState) -> Result<()> {
    let path = workspace_file()?;
    let dir = path.parent().context("no config directory")?;
    fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;

    let contents = serde_json::to_string_pretty(state)?;
    let temporary = dir.join(format!("{FILE_NAME}.tmp"));
    fs::write(&temporary, &contents)
        .with_context(|| format!("could not write {}", temporary.display()))?;

    if let Err(error) = fs::rename(&temporary, &path) {
        // Windows will not replace an existing file with a rename, so fall back
        // to writing in place rather than leaving the workspace unsaved.
        fs::write(&path, &contents)
            .with_context(|| format!("could not write {}: {error:#}", path.display()))?;
        let _ = fs::remove_file(&temporary);
    }
    Ok(())
}

/// Under test the file is left alone: it is the developer's own window, and a
/// test run should not rewrite it.
#[cfg(test)]
pub fn save(_state: &WorkspaceState) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ObjectKind;

    fn scratch_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("zippa-workspace-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("could not create the scratch directory");
        path
    }

    fn object() -> DatabaseObject {
        DatabaseObject {
            schema: Some("public".into()),
            name: "items".into(),
            kind: ObjectKind::Table,
        }
    }

    #[test]
    fn a_full_state_survives_a_round_trip_through_json() {
        let state = WorkspaceState {
            active: 1,
            sessions: vec![SessionState {
                connection: Uuid::new_v4(),
                database: Some("app".into()),
                active: 2,
                panels: vec![
                    PanelState::Query {
                        title: "Query 1".into(),
                        sql: "select 1".into(),
                        file: None,
                    },
                    PanelState::Query {
                        title: "report.sql".into(),
                        sql: "select 2".into(),
                        file: Some(PathBuf::from("/tmp/report.sql")),
                    },
                    PanelState::Table { object: object() },
                    PanelState::Schema { object: object() },
                ],
            }],
            ..WorkspaceState::default()
        };

        let written = serde_json::to_string(&state).expect("the state should serialize");
        let read: WorkspaceState = serde_json::from_str(&written).expect("the state should parse");
        assert_eq!(read, state);
        assert_eq!(read.version, VERSION);
    }

    #[test]
    fn an_empty_file_object_loads_as_the_defaults() {
        let dir = scratch_dir();
        store::set_config_dir_for_test(dir.clone());
        fs::write(dir.join(FILE_NAME), "{}").expect("could not write the file");

        let state = load().expect("an empty file should load");
        assert_eq!(state, WorkspaceState::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unknown_field_is_ignored() {
        let dir = scratch_dir();
        store::set_config_dir_for_test(dir.clone());
        fs::write(
            dir.join(FILE_NAME),
            r#"{"version": 99, "future": true, "sessions": []}"#,
        )
        .expect("could not write the file");

        let state = load().expect("an unknown field should not stop the load");
        assert_eq!(state.version, 99);
        assert!(state.sessions.is_empty());

        // The field this build knows about, from a file written before it: the
        // variant is skipped rather than failing the whole file.
        fs::write(
            dir.join(FILE_NAME),
            r#"{"sessions": [{"connection": "00000000-0000-0000-0000-000000000000", "unknown": 1}]}"#,
        )
        .expect("could not write the file");
        assert!(load().is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_panel_kind_a_newer_build_wrote_is_skipped() {
        let dir = scratch_dir();
        store::set_config_dir_for_test(dir.clone());
        fs::write(
            dir.join(FILE_NAME),
            r#"{"sessions": [{"panels": [{"panel": "graph"}]}]}"#,
        )
        .expect("could not write the file");

        let state = load().expect("a newer panel kind should not stop the load");
        assert_eq!(state.sessions[0].panels, [PanelState::Unknown]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_garbage_file_is_an_error_the_caller_can_fall_back_from() {
        let dir = scratch_dir();
        store::set_config_dir_for_test(dir.clone());
        fs::write(dir.join(FILE_NAME), "not json at all").expect("could not write the file");

        assert!(load().is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
