//! Persistence for saved connections.
//!
//! Connection settings go to a JSON file in the user's config directory;
//! passwords go to the OS credential store (Keychain on macOS, Credential
//! Manager on Windows, Secret Service on Linux) keyed by connection id.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use super::ConnectionConfig;

const SERVICE: &str = "zippa-db";
const FILE_NAME: &str = "connections.json";

#[cfg(test)]
std::thread_local! {
    static TEST_CONFIG_DIR: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Point [`config_dir`] at a scratch directory, so a test never reads or writes
/// the developer's real connections.
#[cfg(test)]
pub(crate) fn set_config_dir_for_test(path: PathBuf) {
    TEST_CONFIG_DIR.with(|dir| *dir.borrow_mut() = Some(path));
}

/// Where Zippa keeps its files: connections, settings, and user themes.
pub(crate) fn config_dir() -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(dir) = TEST_CONFIG_DIR.with(|dir| dir.borrow().clone()) {
        return Ok(dir);
    }

    let dir = dirs::config_dir().context("no config directory for this platform")?;
    Ok(dir.join(SERVICE))
}

fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join(FILE_NAME))
}

/// Read the saved connections. A missing file means "none saved yet".
pub fn load() -> Result<Vec<ConnectionConfig>> {
    let path = config_file()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents =
        fs::read_to_string(&path).with_context(|| format!("could not read {}", path.display()))?;
    let connections = serde_json::from_str(&contents)
        .with_context(|| format!("could not parse {}", path.display()))?;
    Ok(connections)
}

/// Replace the saved connections with `connections`.
///
/// Written beside the real file and renamed into place, so a kill part-way
/// through the write cannot leave a truncated file that `load` would then
/// refuse to parse — the whole connection list would be lost.
pub fn save(connections: &[ConnectionConfig]) -> Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;

    let path = dir.join(FILE_NAME);
    let contents = serde_json::to_string_pretty(connections)?;
    let temporary = dir.join(format!("{FILE_NAME}.tmp"));
    fs::write(&temporary, &contents)
        .with_context(|| format!("could not write {}", temporary.display()))?;

    if let Err(error) = fs::rename(&temporary, &path) {
        // Windows will not replace an existing file with a rename, so fall back
        // to writing in place rather than leaving the connections unsaved.
        fs::write(&path, &contents)
            .with_context(|| format!("could not write {}: {error:#}", path.display()))?;
        let _ = fs::remove_file(&temporary);
    }
    Ok(())
}

/// Mark a connection as the most recently opened one and save it.
///
/// Called on a successful connect so the welcome screen can list connections
/// most-recent-first. A connection that is not saved yet is left alone.
pub fn record_connected(id: &Uuid) -> Result<()> {
    let mut connections = load()?;
    let Some(config) = connections.iter_mut().find(|config| &config.id == id) else {
        return Ok(());
    };
    config.last_connected = Some(Utc::now());
    save(&connections)
}

#[cfg(not(test))]
fn entry(id: &Uuid) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, &id.to_string()).context("no OS credential store available")
}

/// The stored password for a connection, if the user saved one.
pub fn password(id: &Uuid) -> Result<Option<String>> {
    #[cfg(not(test))]
    {
        match entry(id)?.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    #[cfg(test)]
    {
        let _ = id;
        Ok(None)
    }
}

pub fn set_password(id: &Uuid, password: &str) -> Result<()> {
    #[cfg(not(test))]
    {
        if password.is_empty() {
            return delete_password(id);
        }
        entry(id)?
            .set_password(password)
            .context("could not save the password to the OS credential store")
    }
    #[cfg(test)]
    {
        let _ = (id, password);
        Ok(())
    }
}

pub fn delete_password(id: &Uuid) -> Result<()> {
    #[cfg(not(test))]
    {
        match entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    #[cfg(test)]
    {
        let _ = id;
        Ok(())
    }
}
