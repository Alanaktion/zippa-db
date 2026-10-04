//! An SSH tunnel a server connection is opened through.
//!
//! [`Tunnel::open`] signs in to the jump host, then listens on a loopback
//! port of its own; every connection the pool makes to that port is carried
//! to the database through a `direct-tcpip` channel, the same thing `ssh -L`
//! does. The pool never knows: it is simply pointed at `127.0.0.1` and the
//! tunnel's port. Dropping the tunnel stops the listener, every forwarded
//! connection, and the SSH session with them.
//!
//! The jump host's key is checked against `~/.ssh/known_hosts`, the file
//! OpenSSH keeps: a host seen before must present the same key, and one never
//! seen is recorded on first use (OpenSSH's `accept-new`). A changed key is
//! refused with the line to remove, because that is what a man in the middle
//! looks like.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use russh::Disconnect;
use russh::client::{self, AuthResult, Handle};
use russh::keys::{self, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate, known_hosts};
use tokio::net::TcpListener;
use tokio::task::{JoinHandle, JoinSet};

use super::config::{SshAuth, SshConfig};

/// A live tunnel: the SSH session and the loopback listener in front of it.
pub(crate) struct Tunnel {
    local_port: u16,
    session: Arc<Handle<HostCheck>>,
    listener: JoinHandle<()>,
}

impl fmt::Debug for Tunnel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tunnel")
            .field("local_port", &self.local_port)
            .finish_non_exhaustive()
    }
}

impl Tunnel {
    /// Sign in to the jump host `ssh` names and start forwarding a loopback
    /// port to `target_host:target_port`, as the jump host resolves it.
    ///
    /// `secret` is the SSH password or the key's passphrase, whichever the
    /// auth method takes; the agent takes neither.
    pub(crate) async fn open(
        ssh: &SshConfig,
        secret: Option<&str>,
        target_host: &str,
        target_port: u16,
    ) -> Result<Self> {
        let known_hosts = known_hosts_file()?;
        let host = ssh.host.trim().to_string();
        let config = Arc::new(client::Config {
            nodelay: true,
            ..client::Config::default()
        });
        let check = HostCheck {
            host: host.clone(),
            port: ssh.port,
            known_hosts,
        };
        let mut session = client::connect(config, (host.as_str(), ssh.port), check)
            .await
            .with_context(|| format!("could not reach the SSH host {host}:{}", ssh.port))?;

        authenticate(&mut session, ssh, secret).await?;

        // Bound before the session is shared, so a failure here still drops
        // the session and closes it.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("could not open a local port for the SSH tunnel")?;
        let local_port = listener.local_addr()?.port();

        let session = Arc::new(session);
        let listener = tokio::spawn(forward(
            listener,
            session.clone(),
            target_host.to_string(),
            target_port,
        ));

        Ok(Self {
            local_port,
            session,
            listener,
        })
    }

    /// The loopback port the pool connects to.
    pub(crate) fn local_port(&self) -> u16 {
        self.local_port
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // Aborting the listener drops its `JoinSet`, which aborts every
        // forwarded connection; the session goes once the last handle does.
        self.listener.abort();
        let session = self.session.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = session
                    .disconnect(Disconnect::ByApplication, "", "en")
                    .await;
            });
        }
    }
}

/// Accept connections on `listener` for as long as the tunnel lives, carrying
/// each to the target through its own channel.
async fn forward(
    listener: TcpListener,
    session: Arc<Handle<HostCheck>>,
    target_host: String,
    target_port: u16,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((mut socket, peer)) = accepted else { continue };
                let session = session.clone();
                let target_host = target_host.clone();
                connections.spawn(async move {
                    let channel = session
                        .channel_open_direct_tcpip(
                            target_host,
                            target_port.into(),
                            peer.ip().to_string(),
                            peer.port().into(),
                        )
                        .await;
                    // A channel the jump host refuses shows up to the driver
                    // as a connection closed straight away, which it reports.
                    if let Ok(channel) = channel {
                        let mut stream = channel.into_stream();
                        let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                    }
                });
            }
            // Reap the finished ones so a long session does not pile them up.
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
        }
    }
}

/// Sign in the way `ssh.auth` says, or explain why the host said no.
async fn authenticate(
    session: &mut Handle<HostCheck>,
    ssh: &SshConfig,
    secret: Option<&str>,
) -> Result<()> {
    let user = ssh.username.trim().to_string();
    let result = match ssh.auth {
        SshAuth::Password => {
            session
                .authenticate_password(user, secret.unwrap_or_default())
                .await?
        }
        SshAuth::PrivateKey => {
            let path = expand_home(ssh.key_path.trim());
            let key = keys::load_secret_key(&path, secret.filter(|secret| !secret.is_empty()))
                .with_context(|| {
                    format!("could not read the SSH private key {}", path.display())
                })?;
            let hash = session.best_supported_rsa_hash().await?.flatten();
            session
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await?
        }
        SshAuth::Agent => authenticate_with_agent(session, &user).await?,
    };

    match result {
        AuthResult::Success => Ok(()),
        AuthResult::Failure { .. } => bail!(
            "the SSH host refused {} for {}",
            match ssh.auth {
                SshAuth::Agent => "every key the SSH agent offered",
                SshAuth::PrivateKey => "the private key",
                SshAuth::Password => "the password",
            },
            ssh.username.trim()
        ),
    }
}

/// Offer each key the running agent holds until one is accepted.
async fn authenticate_with_agent(
    session: &mut Handle<HostCheck>,
    user: &str,
) -> Result<AuthResult> {
    #[cfg(unix)]
    let agent = keys::agent::client::AgentClient::connect_env().await;
    #[cfg(windows)]
    let agent =
        keys::agent::client::AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await;
    let mut agent = agent.map_err(|error| anyhow!("could not reach the SSH agent: {error}"))?;

    let identities = agent
        .request_identities()
        .await
        .map_err(|error| anyhow!("could not list the SSH agent's keys: {error}"))?;
    if identities.is_empty() {
        bail!("the SSH agent holds no keys; add one with ssh-add");
    }

    let hash = session.best_supported_rsa_hash().await?.flatten();
    let mut last = None;
    for identity in identities {
        let key = identity.public_key().into_owned();
        match session
            .authenticate_publickey_with(user, key, hash, &mut agent)
            .await
        {
            Ok(AuthResult::Success) => return Ok(AuthResult::Success),
            Ok(failure) => last = Some(failure),
            Err(error) => bail!("the SSH agent could not sign in: {error:?}"),
        }
    }
    Ok(last.expect("at least one identity was offered"))
}

/// Checks the jump host's key against a known-hosts file, learning a host
/// the first time it is seen.
pub(crate) struct HostCheck {
    host: String,
    port: u16,
    known_hosts: PathBuf,
}

impl client::Handler for HostCheck {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key: &PublicKey = match server_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key,
            PublicKeyOrCertificate::Certificate(_) => {
                bail!("the SSH host presented a certificate, which is not supported yet")
            }
        };
        check_host_key(&self.host, self.port, key, &self.known_hosts)?;
        Ok(true)
    }
}

/// Accept `key` for `host:port` if `known_hosts` has it or has nothing for the
/// host (recording it then), and refuse it if the file has another.
fn check_host_key(host: &str, port: u16, key: &PublicKey, known_hosts: &Path) -> Result<()> {
    match known_hosts::check_known_hosts_path(host, port, key, known_hosts) {
        Ok(true) => Ok(()),
        Ok(false) => known_hosts::learn_known_hosts_path(host, port, key, known_hosts)
            .with_context(|| {
                format!(
                    "could not record the SSH host key in {}",
                    known_hosts.display()
                )
            }),
        Err(keys::Error::KeyChanged { line }) => bail!(
            "the SSH host key for {host} has changed since it was recorded in {} (line {line}). \
             That can mean someone is intercepting the connection. If the host was rebuilt, \
             remove that line and connect again.",
            known_hosts.display()
        ),
        Err(error) => Err(anyhow!("could not read {}: {error}", known_hosts.display())),
    }
}

/// `~/.ssh/known_hosts`, OpenSSH's own file, so a host trusted from a
/// terminal is trusted here too.
#[cfg(not(test))]
fn known_hosts_file() -> Result<PathBuf> {
    let home = dirs::home_dir().context("no home directory to find ~/.ssh/known_hosts in")?;
    Ok(home.join(".ssh").join("known_hosts"))
}

/// A file of the test run's own, so a live test never writes to the user's
/// `~/.ssh/known_hosts`.
#[cfg(test)]
fn known_hosts_file() -> Result<PathBuf> {
    Ok(std::env::temp_dir().join(format!("zippa-db-test-known-hosts-{}", std::process::id())))
}

/// A leading `~/` (or a bare `~`) is the home directory, the way a shell
/// reads it.
fn expand_home(path: &str) -> PathBuf {
    let rest = match path {
        "~" => Some(""),
        _ => path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")),
    };
    match (rest, dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> PublicKey {
        let private = keys::PrivateKey::from(keys::ssh_key::private::Ed25519Keypair::from_seed(
            &[seed; 32],
        ));
        private.public_key().clone()
    }

    fn scratch() -> PathBuf {
        std::env::temp_dir()
            .join(format!("zippa-db-known-hosts-{}", uuid::Uuid::new_v4()))
            .join("known_hosts")
    }

    #[test]
    fn a_new_host_is_learned_and_then_trusted() {
        let file = scratch();
        check_host_key("bastion", 22, &key(1), &file).expect("a new host is accepted");
        assert!(file.exists(), "the key should have been recorded");
        check_host_key("bastion", 22, &key(1), &file).expect("the same key is trusted");
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_changed_host_key_is_refused() {
        let file = scratch();
        check_host_key("bastion", 22, &key(1), &file).unwrap();
        let error = check_host_key("bastion", 22, &key(2), &file).unwrap_err();
        assert!(format!("{error}").contains("has changed"), "{error}");
        // Another host, or the same one on another port, is a host of its own.
        check_host_key("bastion", 2222, &key(2), &file).unwrap();
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn a_leading_tilde_is_the_home_directory() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            expand_home("~/.ssh/id_ed25519"),
            home.join(".ssh/id_ed25519")
        );
        assert_eq!(expand_home("/etc/key"), PathBuf::from("/etc/key"));
        assert_eq!(expand_home("~other/key"), PathBuf::from("~other/key"));
    }
}
