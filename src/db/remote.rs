//! A SQLite file that lives on another machine, edited as a local copy.
//!
//! SQLite wants a file it can lock and read at random, which SFTP cannot give
//! it reliably, so the file is not opened where it lies.
//! [`RemoteFile::download`] signs in to the SSH host the connection names,
//! copies the file down over SFTP into a directory of its own, and the pool
//! opens that copy like any local file. Nothing goes back until the user
//! uploads: [`RemoteFile::upload`] writes a snapshot of the copy beside the
//! original on the server and renames it into place, so nothing on the server
//! ever opens a half-written file.
//!
//! Before an upload, the server's file is checked against the size and
//! modification time it had when it was last downloaded or uploaded. One that
//! changed underneath is reported back ([`Upload::ServerChanged`]) rather
//! than overwritten, unless the caller says to go ahead — the user answering
//! that warning.
//!
//! A file with a non-empty `-wal` or `-journal` beside it is open on the
//! server with changes in flight. Its `-wal` is downloaded with it, so the
//! copy is current; but it is never replaced, since a program still writing
//! to the old file would carry on into the new one and corrupt it.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use anyhow::{Context as _, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use russh::client::{Handle, Msg};
use russh::{ChannelStream, Disconnect};
use russh_sftp::client::{RawSftpSession, SftpSession};
use russh_sftp::protocol::{FileAttributes, Packet, StatusCode};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::config::SshConfig;
use super::tunnel::{self, HostCheck};

/// OpenSSH's rename that replaces the target in one step, as `rename(2)`
/// does. Plain SFTP rename refuses a target that exists.
const POSIX_RENAME: &str = "posix-rename@openssh.com";

/// What an upload came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upload {
    /// The server has the copy now.
    Uploaded,
    /// The server's file changed since it was last downloaded or uploaded,
    /// so nothing was written; `modified` is when, if the server says.
    ServerChanged { modified: Option<DateTime<Utc>> },
}

/// What the server says about its file: enough to tell that it changed.
///
/// SFTP gives the modification time to the second, and a SQLite file keeps
/// its size through most writes, so those two alone miss a commit made in
/// the same second as the download. The header is read as well: its file
/// change counter goes up with every commit to a rollback-journal database.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    size: Option<u64>,
    modified: Option<u32>,
    header: Vec<u8>,
}

/// How much of the file [`Stamp`] reads: SQLite's whole database header.
const HEADER_LEN: u64 = 100;

impl Stamp {
    fn modified_at(&self) -> Option<DateTime<Utc>> {
        self.modified
            .and_then(|secs| DateTime::from_timestamp(secs.into(), 0))
    }
}

/// The local copy's size and modification time, and its `-wal`'s: a commit
/// changes one or the other, a read changes neither.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LocalStamp {
    main: Option<(u64, Option<SystemTime>)>,
    wal: Option<(u64, Option<SystemTime>)>,
}

impl LocalStamp {
    fn of(local: &Path) -> Self {
        let stat = |path: &Path| {
            std::fs::metadata(path)
                .ok()
                .map(|metadata| (metadata.len(), metadata.modified().ok()))
        };
        Self {
            main: stat(local),
            wal: stat(&with_suffix(local, "-wal")),
        }
    }
}

/// Where the two copies last agreed.
struct Synced {
    server: Stamp,
    local: LocalStamp,
}

/// A remote SQLite file and the local copy it is edited through.
///
/// Holds the SSH secret in memory (like [`Connection`](super::Connection)
/// does) so an upload need not ask for it again. The local copy's directory
/// is removed when this is dropped, unless the copy holds changes that never
/// reached the server: those are left in the cache directory rather than
/// thrown away.
pub(crate) struct RemoteFile {
    ssh: SshConfig,
    secret: Option<String>,
    /// The file's path on the server, as SFTP takes it: relative to the SSH
    /// user's home directory unless it starts with `/`.
    path: String,
    /// The directory the copy sits in, this file's alone.
    dir: PathBuf,
    local: PathBuf,
    synced: Mutex<Synced>,
}

impl fmt::Debug for RemoteFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemoteFile")
            .field("host", &self.ssh.host)
            .field("path", &self.path)
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

impl RemoteFile {
    /// Copy `path` down from the SSH host `ssh` names into a directory of
    /// its own.
    pub(crate) async fn download(
        ssh: &SshConfig,
        secret: Option<String>,
        path: &str,
    ) -> Result<Self> {
        let path = sftp_path(path);
        if path.is_empty() {
            bail!("no database file selected");
        }
        let dir = scratch_dir()?;
        let local = dir.join(local_name(&path));
        let fetched = async {
            let sftp = Sftp::connect(ssh, secret.as_deref()).await?;
            let stamp = sftp.fetch(&path, &local).await;
            sftp.close().await;
            stamp
        }
        .await;
        let server = match fetched {
            Ok(server) => server,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(error);
            }
        };
        let local_stamp = LocalStamp::of(&local);
        Ok(Self {
            ssh: ssh.clone(),
            secret,
            path,
            dir,
            local,
            synced: Mutex::new(Synced {
                server,
                local: local_stamp,
            }),
        })
    }

    /// The copy the pool opens.
    pub(crate) fn local_path(&self) -> &Path {
        &self.local
    }

    /// Whether the copy has been written to since it was last downloaded or
    /// uploaded. A cheap look at the file's size and time, so the UI can ask
    /// it while drawing.
    pub(crate) fn has_changes(&self) -> bool {
        self.synced().local != LocalStamp::of(&self.local)
    }

    /// Take the copy as it is now as the agreed state: for after the pool
    /// first opens it, which may touch the file without changing a row.
    pub(crate) fn settle(&self) {
        self.synced().local = LocalStamp::of(&self.local);
    }

    /// The copy's state, taken before a snapshot of it is made for
    /// [`upload`](Self::upload): a write that lands while the snapshot
    /// travels is still a change once the upload is done.
    pub(crate) fn mark(&self) -> LocalStamp {
        LocalStamp::of(&self.local)
    }

    /// A path in the copy's own directory for a snapshot to be written to.
    pub(crate) fn snapshot_path(&self) -> PathBuf {
        self.dir
            .join(format!("upload-{}.sqlite", uuid::Uuid::new_v4().simple()))
    }

    /// Put `snapshot` on the server in place of the file, unless the
    /// server's file changed since the copies last agreed and `force` is
    /// off. `mark` is the copy's state the snapshot was taken from.
    pub(crate) async fn upload(
        &self,
        snapshot: &Path,
        mark: LocalStamp,
        force: bool,
    ) -> Result<Upload> {
        let expected = self.synced().server.clone();
        let sftp = Sftp::connect(&self.ssh, self.secret.as_deref()).await?;
        let result = async {
            let (current, attrs) = sftp.stamp(&self.path).await?;
            if current != expected && !force {
                return Ok(Err(current));
            }
            sftp.refuse_in_use(&self.path).await?;
            sftp.put(snapshot, &self.path, attrs.permissions).await?;
            anyhow::Ok(Ok(sftp.stamp(&self.path).await?.0))
        }
        .await;
        sftp.close().await;

        match result? {
            Err(current) => Ok(Upload::ServerChanged {
                modified: current.modified_at(),
            }),
            Ok(server) => {
                *self.synced() = Synced {
                    server,
                    local: mark,
                };
                Ok(Upload::Uploaded)
            }
        }
    }

    fn synced(&self) -> std::sync::MutexGuard<'_, Synced> {
        self.synced.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for RemoteFile {
    fn drop(&mut self) {
        if !self.has_changes() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

/// Put a local file on the SSH host as it is, for a live test to set up or
/// change the server's copy behind a connection's back.
#[cfg(test)]
pub(crate) async fn put_for_test(ssh: &SshConfig, secret: &str, local: &Path, path: &str) {
    let sftp = Sftp::connect(ssh, Some(secret)).await.expect("sign in");
    let mut source = tokio::fs::File::open(local).await.expect("open local");
    let mut remote = sftp.files.create(path).await.expect("create remote");
    tokio::io::copy(&mut source, &mut remote)
        .await
        .expect("copy");
    remote.shutdown().await.expect("close remote");
    sftp.close().await;
}

/// Remove a file a live test put on the SSH host.
#[cfg(test)]
pub(crate) async fn remove_for_test(ssh: &SshConfig, secret: &str, path: &str) {
    let sftp = Sftp::connect(ssh, Some(secret)).await.expect("sign in");
    let _ = sftp.files.remove_file(path).await;
    sftp.close().await;
}

/// One signed-in SSH session with an SFTP channel open on it.
struct Sftp {
    session: Handle<HostCheck>,
    files: SftpSession,
}

impl Sftp {
    async fn connect(ssh: &SshConfig, secret: Option<&str>) -> Result<Self> {
        let session = tunnel::sign_in(ssh, secret).await?;
        let files = SftpSession::new(subsystem(&session).await?)
            .await
            .map_err(|error| anyhow!("the SSH host did not start SFTP: {error}"))?;
        Ok(Self { session, files })
    }

    async fn close(&self) {
        let _ = self.files.close().await;
        let _ = self
            .session
            .disconnect(Disconnect::ByApplication, "", "en")
            .await;
    }

    async fn stat(&self, path: &str) -> Result<FileAttributes> {
        let attrs = self
            .files
            .metadata(path)
            .await
            .map_err(|error| anyhow!("could not read {path} on the SSH host: {error}"))?;
        if attrs.is_dir() {
            bail!("{path} on the SSH host is a directory, not a database file");
        }
        Ok(attrs)
    }

    /// What `path` looks like now, for telling later whether it changed,
    /// with the attributes it was read from.
    async fn stamp(&self, path: &str) -> Result<(Stamp, FileAttributes)> {
        let attrs = self.stat(path).await?;
        let mut header = Vec::new();
        let file = self
            .files
            .open(path)
            .await
            .map_err(|error| anyhow!("could not open {path} on the SSH host: {error}"))?;
        file.take(HEADER_LEN)
            .read_to_end(&mut header)
            .await
            .with_context(|| format!("could not read {path} on the SSH host"))?;
        let stamp = Stamp {
            size: attrs.size,
            modified: attrs.mtime,
            header,
        };
        Ok((stamp, attrs))
    }

    /// The size of `path` if it exists and is not empty.
    async fn non_empty(&self, path: &str) -> Option<u64> {
        let attrs = self.files.metadata(path).await.ok()?;
        attrs.size.filter(|&size| size > 0)
    }

    /// Copy `path` (and its `-wal`, when it has one) to `local`, retrying
    /// while the server's file changes under the copy.
    async fn fetch(&self, path: &str, local: &Path) -> Result<Stamp> {
        let wal = format!("{path}-wal");
        let local_wal = with_suffix(local, "-wal");
        for _ in 0..3 {
            let (before, _) = self.stamp(path).await?;
            self.copy_down(path, local).await?;
            if self.non_empty(&wal).await.is_some() {
                self.copy_down(&wal, &local_wal).await?;
            } else {
                let _ = std::fs::remove_file(&local_wal);
            }
            let (after, _) = self.stamp(path).await?;
            if before == after {
                return Ok(after);
            }
        }
        bail!("{path} kept changing on the SSH host while it was downloaded; try again")
    }

    async fn copy_down(&self, path: &str, local: &Path) -> Result<()> {
        let mut remote = self
            .files
            .open(path)
            .await
            .map_err(|error| anyhow!("could not open {path} on the SSH host: {error}"))?;
        let mut file = tokio::fs::File::from_std(create_restricted(local)?);
        tokio::io::copy(&mut remote, &mut file)
            .await
            .with_context(|| format!("could not download {path}"))?;
        file.flush().await?;
        Ok(())
    }

    /// Refuse to replace a database something on the server has open with
    /// changes in flight.
    async fn refuse_in_use(&self, path: &str) -> Result<()> {
        for suffix in ["-wal", "-journal"] {
            if self.non_empty(&format!("{path}{suffix}")).await.is_some() {
                bail!(
                    "{path} is in use on the SSH host (it has a {suffix} file beside it), so it \
                     was not replaced. Close whatever has it open there and upload again."
                );
            }
        }
        Ok(())
    }

    /// Write `snapshot` beside `path` and move it into place, keeping the
    /// file's permissions.
    async fn put(&self, snapshot: &Path, path: &str, permissions: Option<u32>) -> Result<()> {
        let temp = sibling(
            path,
            &format!("zippa-upload-{}", uuid::Uuid::new_v4().simple()),
        );
        let result = async {
            let mut source = tokio::fs::File::open(snapshot).await?;
            let mut remote = self.files.create(temp.as_str()).await.map_err(|error| {
                anyhow!("could not write beside {path} on the SSH host: {error}")
            })?;
            tokio::io::copy(&mut source, &mut remote)
                .await
                .with_context(|| format!("could not upload {path}"))?;
            remote.shutdown().await?;
            if let Some(permissions) = permissions {
                let mut attrs = FileAttributes::empty();
                attrs.permissions = Some(permissions & 0o7777);
                let _ = self.files.set_metadata(temp.as_str(), attrs).await;
            }
            self.replace(&temp, path).await
        }
        .await;
        if result.is_err() {
            let _ = self.files.remove_file(temp.as_str()).await;
        }
        result
    }

    /// Rename `from` over `to` in one step where the server can, and with
    /// the old file moved aside for the moment between otherwise.
    async fn replace(&self, from: &str, to: &str) -> Result<()> {
        let raw = RawSftpSession::new(subsystem(&self.session).await?);
        let version = raw
            .init()
            .await
            .map_err(|error| anyhow!("the SSH host did not start SFTP: {error}"))?;
        if version.extensions.contains_key(POSIX_RENAME) {
            let mut data = Vec::new();
            put_string(&mut data, from);
            put_string(&mut data, to);
            let reply = raw.extended(POSIX_RENAME, data).await;
            let _ = raw.close_session();
            return match reply {
                Ok(Packet::Status(status)) if status.status_code == StatusCode::Ok => Ok(()),
                Ok(Packet::Status(status)) => Err(anyhow!(
                    "could not move the upload into place on the SSH host: {}",
                    status.error_message
                )),
                Ok(_) => Err(anyhow!(
                    "could not move the upload into place: the SSH host answered oddly"
                )),
                Err(error) => Err(anyhow!(
                    "could not move the upload into place on the SSH host: {error}"
                )),
            };
        }
        let _ = raw.close_session();

        // Without it, plain SFTP rename will not overwrite: the old file is
        // moved aside, the new one in, and the old one dropped — or put back
        // if the new one would not go.
        let aside = sibling(to, &format!("zippa-old-{}", uuid::Uuid::new_v4().simple()));
        let failed =
            |error| anyhow!("could not move the upload into place on the SSH host: {error}");
        self.files
            .rename(to, aside.as_str())
            .await
            .map_err(failed)?;
        if let Err(error) = self.files.rename(from, to).await {
            let _ = self.files.rename(aside.as_str(), to).await;
            return Err(failed(error));
        }
        let _ = self.files.remove_file(aside).await;
        Ok(())
    }
}

/// Open an `sftp` subsystem channel on `session`.
async fn subsystem(session: &Handle<HostCheck>) -> Result<ChannelStream<Msg>> {
    let channel = session
        .channel_open_session()
        .await
        .context("could not open an SFTP channel on the SSH host")?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .context("the SSH host would not start SFTP")?;
    Ok(channel.into_stream())
}

/// An SSH wire string: a big-endian length, then the bytes.
fn put_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

/// `path` the way SFTP takes it. A leading `~/` is the home directory, which
/// is where SFTP resolves a relative path from anyway.
pub(crate) fn sftp_path(path: &str) -> String {
    let path = path.trim();
    match path {
        "~" => ".".to_string(),
        _ => path.strip_prefix("~/").unwrap_or(path).to_string(),
    }
}

/// A hidden file beside `path` on the server, tagged with `tag`.
fn sibling(path: &str, tag: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, name)) => format!("{dir}/.{name}.{tag}"),
        None => format!(".{path}.{tag}"),
    }
}

/// What to call the local copy: the remote file's own name.
fn local_name(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or_default();
    match name {
        "" | "." | ".." => "database.sqlite".to_string(),
        name => name.to_string(),
    }
}

/// `path` with `suffix` added to its file name, as SQLite names its `-wal`.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// A new owner-only directory for one copy, under the cache directory.
fn scratch_dir() -> Result<PathBuf> {
    #[cfg(not(test))]
    let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
    #[cfg(test)]
    let base = std::env::temp_dir();
    let dir = base
        .join("zippa-db")
        .join("remote")
        .join(uuid::Uuid::new_v4().simple().to_string());
    std::fs::create_dir_all(&dir).with_context(|| format!("could not create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Create `path` for writing, owner-only from the first byte: a database copy
/// can hold anything.
fn create_restricted(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .with_context(|| format!("could not write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_home_relative_path_is_relative_for_sftp() {
        assert_eq!(sftp_path(" ~/data/app.db "), "data/app.db");
        assert_eq!(sftp_path("/srv/app.db"), "/srv/app.db");
        assert_eq!(sftp_path("app.db"), "app.db");
    }

    #[test]
    fn the_upload_is_written_beside_the_file_and_hidden() {
        assert_eq!(sibling("/srv/app.db", "tmp"), "/srv/.app.db.tmp");
        assert_eq!(sibling("app.db", "tmp"), ".app.db.tmp");
    }

    #[test]
    fn the_local_copy_keeps_the_remote_name() {
        assert_eq!(local_name("/srv/data/app.sqlite"), "app.sqlite");
        assert_eq!(local_name("app.db"), "app.db");
        assert_eq!(local_name("/srv/"), "database.sqlite");
    }

    #[test]
    fn an_ssh_string_is_length_prefixed() {
        let mut out = Vec::new();
        put_string(&mut out, "ab");
        assert_eq!(out, [0, 0, 0, 2, b'a', b'b']);
    }

    #[test]
    fn a_write_to_the_copy_or_its_wal_is_a_change() {
        let dir = scratch_dir().unwrap();
        let local = dir.join("app.db");
        std::fs::write(&local, b"one").unwrap();
        let before = LocalStamp::of(&local);
        assert_eq!(before, LocalStamp::of(&local), "a look is not a change");
        std::fs::write(with_suffix(&local, "-wal"), b"frame").unwrap();
        assert_ne!(before, LocalStamp::of(&local));
        let _ = std::fs::remove_dir_all(dir);
    }
}
