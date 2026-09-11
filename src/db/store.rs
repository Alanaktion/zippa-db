//! Persistence for saved connections.
//!
//! Connection settings go to a JSON file in the user's config directory;
//! passwords go to the OS credential store (Keychain on macOS, Credential
//! Manager on Windows, Secret Service on Linux) keyed by connection id.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use uuid::Uuid;

use super::ConnectionConfig;

const SERVICE: &str = "zippa-db";
const FILE_NAME: &str = "connections.json";

/// Where Zippa keeps its files: connections, settings, and user themes.
pub(crate) fn config_dir() -> Result<PathBuf> {
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
pub fn save(connections: &[ConnectionConfig]) -> Result<()> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;

    let path = dir.join(FILE_NAME);
    let contents = serde_json::to_string_pretty(connections)?;
    fs::write(&path, contents).with_context(|| format!("could not write {}", path.display()))?;
    Ok(())
}

fn entry(id: &Uuid) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, &id.to_string()).context("no OS credential store available")
}

/// The stored password for a connection, if the user saved one.
pub fn password(id: &Uuid) -> Result<Option<String>> {
    match entry(id)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn set_password(id: &Uuid, password: &str) -> Result<()> {
    if password.is_empty() {
        return delete_password(id);
    }
    entry(id)?
        .set_password(password)
        .context("could not save the password to the OS credential store")
}

pub fn delete_password(id: &Uuid) -> Result<()> {
    match entry(id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
}
