//! Persistence for saved connections.
//!
//! Connection settings go to a JSON file in the user's config directory;
//! passwords go to the OS credential store (Keychain on macOS, Credential
//! Manager on Windows, Secret Service on Linux) keyed by connection id.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use anyhow::{Context, Result};
use uuid::Uuid;

use super::{ConnectionConfig, Credentials, SshAuth};

#[cfg_attr(test, allow(dead_code))]
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
    // A test never sees the developer's real directory: one that has not
    // pointed this at its own scratch directory gets a per-process one, so a
    // test that forgets cannot overwrite the real files.
    #[cfg(test)]
    return Ok({
        static FALLBACK: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        TEST_CONFIG_DIR
            .with(|dir| dir.borrow().clone())
            .unwrap_or_else(|| {
                FALLBACK
                    .get_or_init(|| {
                        std::env::temp_dir()
                            .join(format!("zippa-db-test-config-{}", std::process::id()))
                    })
                    .clone()
            })
    });

    #[cfg(not(test))]
    {
        let dir = dirs::config_dir().context("no config directory for this platform")?;
        Ok(dir.join(SERVICE))
    }
}

fn config_file() -> Result<PathBuf> {
    Ok(config_dir()?.join(FILE_NAME))
}

/// Read the saved connections. A missing file means "none saved yet"; an
/// unreadable one is moved aside (see [`load_json`]).
#[cfg(test)]
pub fn load() -> Result<Vec<ConnectionConfig>> {
    Ok(load_with_notice()?.0)
}

/// The saved connections, and what to tell the user when some of them could
/// not be read and were set aside (see [`load_json_list`]).
pub fn load_with_notice() -> Result<(Vec<ConnectionConfig>, Option<String>)> {
    load_json_list(&config_file()?, ["saved connection", "saved connections"])
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
/// `ticket` is taken when the list was decided (see [`ticket`]), so a write
/// that lands after a newer one is dropped rather than putting the older
/// list back.
pub fn save(connections: &[ConnectionConfig], ticket: Ticket) -> Result<()> {
    let contents = serde_json::to_string_pretty(connections)?;
    write_atomic(&config_file()?, &contents, ticket)
}

/// When a write was decided, in the order the app decided them.
///
/// Writes go to the background executor, which can run two of them at once
/// and in either order; taking the ticket on the thread that built the
/// contents is what keeps "last decided" and "last on disk" the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Ticket(u64);

/// A ticket for a write about to be handed to the background.
pub(crate) fn ticket() -> Ticket {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    Ticket(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// Replace `path` with `contents`, whole or not at all.
///
/// Written beside the real file and renamed into place, so a kill part-way
/// through cannot leave a truncated file the next launch would refuse to
/// parse. Writes are taken one at a time (two writers sharing the scratch file
/// could interleave their bytes), and one whose `ticket` is older than the
/// last write to the same path is skipped: it holds what the app has since
/// changed its mind about.
pub(crate) fn write_atomic(path: &Path, contents: &str, ticket: Ticket) -> Result<()> {
    static WRITTEN: Mutex<BTreeMap<PathBuf, Ticket>> = Mutex::new(BTreeMap::new());
    let mut written = WRITTEN.lock().unwrap_or_else(PoisonError::into_inner);
    if written.get(path).is_some_and(|last| *last > ticket) {
        return Ok(());
    }

    let dir = path.parent().context("no config directory")?;
    fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    let name = path
        .file_name()
        .context("no file name")?
        .to_string_lossy()
        .into_owned();
    let temporary = dir.join(format!("{name}.tmp"));
    write_restricted(&temporary, contents)
        .with_context(|| format!("could not write {}", temporary.display()))?;

    if let Err(error) = fs::rename(&temporary, path) {
        // Windows will not replace an existing file with a rename, so fall back
        // to writing in place rather than leaving the file unsaved.
        write_restricted(path, contents)
            .with_context(|| format!("could not write {}: {error:#}", path.display()))?;
        let _ = fs::remove_file(&temporary);
    }
    written.insert(path.to_path_buf(), ticket);
    Ok(())
}

/// Parse `path` as JSON, or `T::default()` when there is no file yet.
///
/// A file that is there but cannot be parsed — written by a newer build, edited
/// by hand, or cut short — is moved aside to `<name>.unreadable` before the
/// error is returned. The caller carries on from the default, and its next
/// save would otherwise replace the user's only copy with an empty one.
pub(crate) fn load_json<T>(path: &Path) -> Result<T>
where
    T: serde::de::DeserializeOwned + Default,
{
    if !path.exists() {
        return Ok(T::default());
    }
    let contents =
        fs::read_to_string(path).with_context(|| format!("could not read {}", path.display()))?;
    match serde_json::from_str(&contents) {
        Ok(value) => Ok(value),
        Err(error) => {
            let mut aside = path.as_os_str().to_owned();
            aside.push(".unreadable");
            let aside = PathBuf::from(aside);
            let kept = match fs::rename(path, &aside) {
                Ok(()) => format!("it was kept as {}", aside.display()),
                Err(rename) => format!("it could not be moved aside: {rename}"),
            };
            Err(anyhow::Error::new(error)
                .context(format!("could not parse {}; {kept}", path.display())))
        }
    }
}

/// Read a JSON list one entry at a time, so one entry this build cannot read
/// (a colour or mode added by a newer release, a hand edit) costs that entry
/// alone rather than the whole list.
///
/// The entries that do not read are written to `<name>.unreadable` (or
/// `<name>.unreadable.N` when that is taken) and the file is rewritten
/// without them, so the next launch does not set them aside again; the
/// second value says so in words, naming an entry `one` and several `many`. A file that is
/// not a JSON list at all goes aside whole, as [`load_json`] does.
pub(crate) fn load_json_list<T>(
    path: &Path,
    [one, many]: [&str; 2],
) -> Result<(Vec<T>, Option<String>)>
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let entries: Vec<serde_json::Value> = load_json(path)?;
    let mut items = Vec::with_capacity(entries.len());
    let mut unread = Vec::new();
    let mut first_error = None;
    for entry in entries {
        match serde_json::from_value::<T>(entry.clone()) {
            Ok(item) => items.push(item),
            Err(error) => {
                first_error.get_or_insert(error);
                unread.push(entry);
            }
        }
    }
    let Some(error) = first_error else {
        return Ok((items, None));
    };

    let count = unread.len();
    let noun = if count == 1 { one } else { many };
    let kept = serde_json::to_string_pretty(&unread)
        .map_err(anyhow::Error::from)
        .and_then(|text| write_aside(path, &text).map_err(anyhow::Error::from));
    let notice = match kept {
        Ok(aside) => {
            // Only once the entries are safe elsewhere is the file rewritten
            // without them.
            if let Err(rewrite) = serde_json::to_string_pretty(&items)
                .map_err(anyhow::Error::from)
                .and_then(|text| write_atomic(path, &text, ticket()))
            {
                eprintln!("could not rewrite {}: {rewrite:#}", path.display());
            }
            format!(
                "{count} {noun} could not be read by this version and {verb} set aside in {} \
                 ({error}).",
                aside.display(),
                verb = if count == 1 { "was" } else { "were" },
            )
        }
        Err(write) => format!(
            "{count} {noun} could not be read by this version ({error}), and could not be set \
             aside ({write:#}); {} was left as it is.",
            path.display()
        ),
    };
    Ok((items, Some(notice)))
}

/// Write `contents` to a new aside file next to `path`, owner-only like
/// [`write_restricted`], and say where. The name is taken with an exclusive
/// create, so a second launcher setting the same file aside at the same
/// moment takes the next name rather than truncating this one's copy.
fn write_aside(path: &Path, contents: &str) -> std::io::Result<PathBuf> {
    use std::io::Write as _;

    loop {
        let aside = free_aside_path(path);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(&aside) {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())?;
                return Ok(aside);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// `<path>.unreadable`, or the first `<path>.unreadable.N` not yet taken, so
/// setting entries aside never replaces ones set aside before.
fn free_aside_path(path: &Path) -> PathBuf {
    let mut base = path.as_os_str().to_owned();
    base.push(".unreadable");
    let base = PathBuf::from(base);
    if !base.exists() {
        return base;
    }
    (1..)
        .map(|n| {
            let mut candidate = base.as_os_str().to_owned();
            candidate.push(format!(".{n}"));
            PathBuf::from(candidate)
        })
        .find(|candidate| !candidate.exists())
        .unwrap_or(base)
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

/// What the connection editor's secret boxes mean for the keychain: `None`
/// for a box the user never touched (keep what is stored), `Some("")` for one
/// they emptied (forget it), anything else a new secret.
#[derive(Clone, Default)]
pub struct SecretEdits {
    pub password: Option<String>,
    pub ssh: Option<String>,
}

impl SecretEdits {
    /// Write the boxes the user touched to the keychain, leaving the rest.
    pub fn save(&self, id: &Uuid) -> Result<()> {
        if let Some(password) = &self.password {
            set_password(id, password)?;
        }
        if let Some(secret) = &self.ssh {
            set_ssh_secret(id, secret)?;
        }
        Ok(())
    }
}

/// The secrets to open `config` with: what the user typed, or for a box left
/// untouched on a `saved` connection, what the keychain holds. Only the
/// secrets the connection can use are looked up, since asking the keychain for
/// one it never stored can prompt the user for nothing.
///
/// Blocking: the keychain can prompt, so call it off the UI thread.
pub fn credentials(
    config: &ConnectionConfig,
    edits: &SecretEdits,
    saved: bool,
) -> Result<Credentials> {
    let typed = |secret: &str| Some(secret.to_string()).filter(|secret| !secret.is_empty());
    let server = !config.engine.is_file_based();

    let password = match &edits.password {
        Some(password) => typed(password),
        None if saved && server => password(&config.id)?,
        None => None,
    };
    // A SQLite file on an SSH host signs in the same way a tunnel does.
    let ssh = if config.ssh.enabled && config.ssh.auth != SshAuth::Agent {
        match &edits.ssh {
            Some(secret) => typed(secret),
            None if saved => ssh_secret(&config.id)?,
            None => None,
        }
    } else {
        None
    };
    Ok(Credentials { password, ssh })
}

/// The keychain account the Agents panel's API key is kept under. One key for
/// the whole app, so no connection id: it is the model endpoint's, not a
/// database's.
#[cfg(not(test))]
fn api_key_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, "agents/api-key").context("no OS credential store available")
}

/// The stored API key for the model endpoint, if any.
pub fn api_key() -> Result<Option<String>> {
    #[cfg(not(test))]
    {
        match api_key_entry()?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
    #[cfg(test)]
    {
        Ok(None)
    }
}

/// Store the API key; an empty one forgets it.
pub fn set_api_key(key: &str) -> Result<()> {
    #[cfg(not(test))]
    {
        if key.is_empty() {
            return match api_key_entry()?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(error) => Err(error.into()),
            };
        }
        api_key_entry()?
            .set_password(key)
            .context("could not save the API key to the OS credential store")
    }
    #[cfg(test)]
    {
        let _ = key;
        Ok(())
    }
}

/// The keychain account an SSH tunnel's password or key passphrase is kept
/// under: beside the database password, under the same connection id.
#[cfg(not(test))]
fn ssh_entry(id: &Uuid) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, &format!("{id}/ssh")).context("no OS credential store available")
}

/// The stored SSH password or key passphrase for a connection, if any.
pub fn ssh_secret(id: &Uuid) -> Result<Option<String>> {
    #[cfg(not(test))]
    {
        match ssh_entry(id)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
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

/// Store the SSH password or key passphrase; an empty one forgets it.
pub fn set_ssh_secret(id: &Uuid, secret: &str) -> Result<()> {
    #[cfg(not(test))]
    {
        if secret.is_empty() {
            return delete_ssh_secret(id);
        }
        ssh_entry(id)?
            .set_password(secret)
            .context("could not save the SSH secret to the OS credential store")
    }
    #[cfg(test)]
    {
        let _ = (id, secret);
        Ok(())
    }
}

pub fn delete_ssh_secret(id: &Uuid) -> Result<()> {
    #[cfg(not(test))]
    {
        match ssh_entry(id)?.delete_credential() {
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

        save(&[], ticket()).expect("could not save the connections");

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
