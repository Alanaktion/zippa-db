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

/// Write `contents` to `path`, restricted to the owner where the platform
/// supports it. These files can hold a pasted secret — a password typed into
/// a query buffer (`workspace.json`), or a database's host and username on a
/// shared machine whose other users have no business reading them
/// (`connections.json`, `settings.json`) — so the file is created with
/// restricted permissions from the first byte rather than tightened
/// afterward, which would leave a window where a default-permissions file is
/// briefly readable by everyone. Unix only: Windows has no equivalent this
/// simple, and its ACL model is out of scope here.
pub(crate) fn write_restricted(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;

        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents.as_bytes())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)
    }
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
    write_restricted(&temporary, &contents)
        .with_context(|| format!("could not write {}", temporary.display()))?;

    if let Err(error) = fs::rename(&temporary, &path) {
        // Windows will not replace an existing file with a rename, so fall back
        // to writing in place rather than leaving the connections unsaved.
        write_restricted(&path, &contents)
            .with_context(|| format!("could not write {}: {error:#}", path.display()))?;
        let _ = fs::remove_file(&temporary);
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("zippa-db-store-test-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).expect("could not create the scratch directory");
            Self(path)
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// `connections.json` can carry a database's host, port, and username —
    /// a shared machine's other users have no business reading that, even
    /// though the password itself lives in the OS credential store.
    #[test]
    #[cfg(unix)]
    fn saved_connections_are_restricted_to_the_owner() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = ScratchDir::new();
        set_config_dir_for_test(dir.0.clone());

        save(&[]).expect("could not save the connections");

        let mode = fs::metadata(dir.0.join(FILE_NAME))
            .expect("the file should exist")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the file should be owner-only");
    }

    /// The Windows fallback path (`save`'s in-place write when `rename`
    /// fails) goes through the same `write_restricted`, so it gets the same
    /// permissions rather than the platform default.
    #[test]
    #[cfg(unix)]
    fn write_restricted_creates_an_owner_only_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = ScratchDir::new();
        let path = dir.0.join("owner-only.json");

        write_restricted(&path, "{}").expect("could not write the file");

        let mode = fs::metadata(&path)
            .expect("the file should exist")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the file should be owner-only");
    }
}
