//! What the user configures about a connection, before it is opened.
//!
//! Nothing here touches the network or sqlx: it is the saved shape of a
//! connection ([`ConnectionConfig`]), the engine it targets ([`Engine`]), and
//! how careful it is about writes ([`SafetyMode`]). [`store`](super::store)
//! persists it; [`Connection`](super::Connection) opens it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Database engine a connection targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    Postgres,
    MySql,
    Sqlite,
}

impl Engine {
    pub const ALL: [Engine; 3] = [Engine::Postgres, Engine::MySql, Engine::Sqlite];

    pub fn label(self) -> &'static str {
        match self {
            Engine::Postgres => "PostgreSQL",
            Engine::MySql => "MySQL",
            Engine::Sqlite => "SQLite",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            Engine::Postgres => 5432,
            Engine::MySql => 3306,
            Engine::Sqlite => 0,
        }
    }

    /// SQLite connects to a file, so it has no host, port, or credentials.
    pub fn is_file_based(self) -> bool {
        matches!(self, Engine::Sqlite)
    }
}

/// How much ceremony a connection asks for before anything is written.
///
/// One scale, from the most careful to the least: refuse writes, ask about
/// each one, hold inline edits until they are applied, apply them as the user
/// moves on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SafetyMode {
    /// Nothing may be written: the session is opened read-only and statements
    /// that do not plainly read are refused before they are sent.
    ReadOnly,
    /// Every write is shown to the user before it runs.
    ConfirmWrites,
    /// Inline edits wait in the grid until they are applied.
    #[default]
    Staged,
    /// Inline edits are written as soon as the selection leaves the row.
    AutoApply,
}

impl SafetyMode {
    pub const ALL: [SafetyMode; 4] = [
        SafetyMode::ReadOnly,
        SafetyMode::ConfirmWrites,
        SafetyMode::Staged,
        SafetyMode::AutoApply,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SafetyMode::ReadOnly => "Read-only",
            SafetyMode::ConfirmWrites => "Confirm writes",
            SafetyMode::Staged => "Staged edits",
            SafetyMode::AutoApply => "Auto-apply",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            SafetyMode::ReadOnly => "Refuses anything that writes",
            SafetyMode::ConfirmWrites => "Shows every write before it runs",
            SafetyMode::Staged => "Edits wait until you apply them",
            SafetyMode::AutoApply => "Edits are written when you leave the row",
        }
    }

    pub fn is_read_only(self) -> bool {
        matches!(self, SafetyMode::ReadOnly)
    }

    pub fn confirms_writes(self) -> bool {
        matches!(self, SafetyMode::ConfirmWrites)
    }

    pub fn auto_applies(self) -> bool {
        matches!(self, SafetyMode::AutoApply)
    }
}

/// Whether, and how strictly, a server connection is encrypted.
///
/// One scale for both servers, named the way libpq names it; each engine maps
/// it onto its own driver's modes (MySQL calls `VerifyFull` "verify identity").
/// SQLite opens a file and ignores it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SslMode {
    /// Never encrypt.
    Disable,
    /// Encrypt when the server offers it, otherwise connect in the clear. What
    /// the drivers do when nothing is said, so a connection saved before the
    /// setting existed behaves as it always did.
    #[default]
    Prefer,
    /// Refuse to connect unencrypted, but trust whatever certificate the
    /// server presents.
    Require,
    /// Encrypt and check the server's certificate against a trusted
    /// authority.
    VerifyCa,
    /// As `VerifyCa`, and check that the certificate names the host.
    VerifyFull,
}

impl SslMode {
    pub const ALL: [SslMode; 5] = [
        SslMode::Disable,
        SslMode::Prefer,
        SslMode::Require,
        SslMode::VerifyCa,
        SslMode::VerifyFull,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SslMode::Disable => "Disable",
            SslMode::Prefer => "Prefer",
            SslMode::Require => "Require",
            SslMode::VerifyCa => "Verify CA",
            SslMode::VerifyFull => "Verify full",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            SslMode::Disable => "Never encrypt the connection",
            SslMode::Prefer => "Encrypt when the server offers it",
            SslMode::Require => "Always encrypt, without checking the certificate",
            SslMode::VerifyCa => "Always encrypt, and check the certificate's authority",
            SslMode::VerifyFull => "Always encrypt, and check the authority and the host name",
        }
    }

    /// Whether the certificate files are used: only once encryption is
    /// insisted on. Under `Prefer` a connection may well end up in the clear,
    /// and the editor hides the files, so they are not sent either; a mode
    /// stepped down to keeps them saved for when it is stepped back up.
    pub fn uses_files(self) -> bool {
        matches!(
            self,
            SslMode::Require | SslMode::VerifyCa | SslMode::VerifyFull
        )
    }
}

/// How a server connection is encrypted: the mode, plus the certificate files
/// it may need. Each path is empty when unused; none of them is a secret (a
/// client key's passphrase is not supported), so they ride along in
/// `connections.json` like the host does.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SslConfig {
    pub mode: SslMode,
    /// A PEM file of the authorities to trust, in place of the system's own.
    pub ca_cert: String,
    /// A PEM client certificate, for a server that asks the client to prove
    /// who it is.
    pub client_cert: String,
    /// The PEM private key that goes with `client_cert`.
    pub client_key: String,
}

impl SslConfig {
    /// Why these settings cannot be used, in words for the user.
    ///
    /// A client certificate is only half of an identity without its key, and
    /// the other way round; the drivers would otherwise ignore the half given
    /// without a word.
    pub fn invalid(&self) -> Option<&'static str> {
        if !self.mode.uses_files() {
            return None;
        }
        match (
            self.client_cert.trim().is_empty(),
            self.client_key.trim().is_empty(),
        ) {
            (false, true) => Some("A client certificate needs its private key too."),
            (true, false) => Some("A client key needs its certificate too."),
            _ => None,
        }
    }

    /// A path the user filled in, or `None` for an empty box.
    pub(crate) fn path(value: &str) -> Option<&str> {
        Some(value.trim()).filter(|value| !value.is_empty())
    }
}

/// A fixed palette a connection can be coloured with.
///
/// A palette rather than arbitrary hex: a colour is resolved against the theme
/// for contrast in light and dark, and stays valid when a theme changes. The
/// colour's [`label`](TagColor::label) is what a screen reader hears in its
/// place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TagColor {
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
    Gray,
}

impl TagColor {
    pub const ALL: [TagColor; 9] = [
        TagColor::Red,
        TagColor::Orange,
        TagColor::Yellow,
        TagColor::Green,
        TagColor::Teal,
        TagColor::Blue,
        TagColor::Purple,
        TagColor::Pink,
        TagColor::Gray,
    ];

    pub fn label(self) -> &'static str {
        match self {
            TagColor::Red => "Red",
            TagColor::Orange => "Orange",
            TagColor::Yellow => "Yellow",
            TagColor::Green => "Green",
            TagColor::Teal => "Teal",
            TagColor::Blue => "Blue",
            TagColor::Purple => "Purple",
            TagColor::Pink => "Pink",
            TagColor::Gray => "Gray",
        }
    }

    /// The kebab-case key the palette colour serialises as.
    pub fn key(self) -> &'static str {
        match self {
            TagColor::Red => "red",
            TagColor::Orange => "orange",
            TagColor::Yellow => "yellow",
            TagColor::Green => "green",
            TagColor::Teal => "teal",
            TagColor::Blue => "blue",
            TagColor::Purple => "purple",
            TagColor::Pink => "pink",
            TagColor::Gray => "gray",
        }
    }
}

/// A saved connection as configured by the user.
///
/// The password is not part of this struct: it lives in the OS keychain, keyed
/// by [`ConnectionConfig::id`]. See [`store`](super::store).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionConfig {
    pub id: Uuid,
    pub name: String,
    pub engine: Engine,
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Database name, or the file path for SQLite.
    pub database: String,
    /// How careful this connection is about writes. Absent in files written
    /// before the setting existed, which read back as the safer mode.
    #[serde(default)]
    pub safety: SafetyMode,
    /// The palette colour the connection is marked with, if any: a dot in the
    /// title bar and a tint on its tab. Absent in files written before
    /// colouring existed. (Files from when a connection also had a free-text
    /// `tag` still read: serde skips the field.)
    #[serde(default)]
    pub color: Option<TagColor>,
    /// When this connection was last opened, for most-recent-first ordering.
    #[serde(default)]
    pub last_connected: Option<DateTime<Utc>>,
    /// How a server connection is encrypted. Absent in files written before
    /// the setting existed, which read back as `Prefer`: what the drivers did
    /// all along.
    #[serde(default)]
    pub ssl: SslConfig,
}

impl ConnectionConfig {
    pub fn new(engine: Engine) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: String::new(),
            engine,
            host: "localhost".into(),
            port: engine.default_port(),
            username: String::new(),
            database: String::new(),
            safety: SafetyMode::default(),
            color: None,
            last_connected: None,
            ssl: SslConfig::default(),
        }
    }

    /// Name to show when the user has not given the connection one.
    pub fn display_name(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.clone();
        }

        // Showing the whole path is the target line's job; a name wants to be
        // short enough to sit in a tab.
        if self.engine.is_file_based() {
            return file_name(&self.database);
        }
        self.display_target()
    }

    /// `localhost:5432/app`, or the file's path for SQLite.
    pub fn display_target(&self) -> String {
        if self.engine.is_file_based() {
            return self.database.clone();
        }
        format!("{}:{}/{}", self.host, self.port, self.database)
    }

    /// Whether this connection is the footgun the colour exists to flag: one
    /// that reads as production and writes without asking. Shared by the
    /// connection editor's inline warning and the launcher's confirm-before-
    /// connect, so both draw the same line.
    pub fn is_risky_auto_apply(&self) -> bool {
        is_risky_auto_apply(&self.name, self.color, self.safety)
    }
}

/// The rule [`ConnectionConfig::is_risky_auto_apply`] applies, taken as plain
/// values so the connection editor can ask it about a name still being typed
/// — before there is a whole `ConnectionConfig` to ask.
///
/// A connection reads as production when it is coloured red or its name
/// mentions "prod" (case-insensitively).
pub fn is_risky_auto_apply(name: &str, color: Option<TagColor>, safety: SafetyMode) -> bool {
    safety.auto_applies() && (color == Some(TagColor::Red) || name.to_lowercase().contains("prod"))
}

impl Default for ConnectionConfig {
    fn default() -> Self {
        Self::new(Engine::Postgres)
    }
}

/// The file's own name, or the whole path when it has none.
pub(crate) fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_client_certificate_needs_its_key() {
        let half = |cert: &str, key: &str| SslConfig {
            mode: SslMode::Require,
            client_cert: cert.into(),
            client_key: key.into(),
            ..SslConfig::default()
        };
        assert!(half("client.crt", "").invalid().is_some());
        assert!(half("", "client.key").invalid().is_some());
        assert_eq!(half("client.crt", "client.key").invalid(), None);
        assert_eq!(half("", "").invalid(), None);
        // Below `Require` the files are never read, so a half-filled pair
        // left over from an earlier mode is no reason to refuse.
        for mode in [SslMode::Disable, SslMode::Prefer] {
            assert_eq!(
                SslConfig {
                    mode,
                    ..half("client.crt", "")
                }
                .invalid(),
                None
            );
        }
    }

    #[test]
    fn a_connection_saved_before_ssl_settings_reads_back_as_prefer() {
        let config: ConnectionConfig = serde_json::from_str(
            r#"{"id":"6f1c1f0e-8a5c-4d36-9a49-1b1f3c0f2a10","name":"","engine":"Postgres",
                "host":"localhost","port":5432,"username":"","database":""}"#,
        )
        .unwrap();
        assert_eq!(config.ssl, SslConfig::default());
        assert_eq!(config.ssl.mode, SslMode::Prefer);
    }

    #[test]
    fn ssl_settings_round_trip_through_json() {
        let config = ConnectionConfig {
            ssl: SslConfig {
                mode: SslMode::VerifyFull,
                ca_cert: "/etc/ssl/ca.pem".into(),
                ..SslConfig::default()
            },
            ..ConnectionConfig::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains(r#""mode":"verify-full""#), "{json}");
        let read: ConnectionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(read.ssl, config.ssl);
    }

    #[test]
    fn a_production_connection_with_auto_apply_is_risky() {
        assert!(is_risky_auto_apply(
            "App",
            Some(TagColor::Red),
            SafetyMode::AutoApply
        ));
        // Case-insensitive, and matches a name that merely contains the word.
        assert!(is_risky_auto_apply(
            "prod-east",
            None,
            SafetyMode::AutoApply
        ));
    }

    #[test]
    fn a_production_connection_is_not_risky_under_a_safer_mode() {
        for safety in [
            SafetyMode::ReadOnly,
            SafetyMode::ConfirmWrites,
            SafetyMode::Staged,
        ] {
            assert!(!is_risky_auto_apply(
                "Production",
                Some(TagColor::Red),
                safety
            ));
        }
    }

    #[test]
    fn auto_apply_is_not_risky_without_a_production_marker() {
        assert!(!is_risky_auto_apply("", None, SafetyMode::AutoApply));
        assert!(!is_risky_auto_apply(
            "Staging",
            Some(TagColor::Orange),
            SafetyMode::AutoApply
        ));
    }
}
