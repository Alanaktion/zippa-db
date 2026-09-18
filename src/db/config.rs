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

/// A fixed palette a connection can be tagged with.
///
/// A palette rather than arbitrary hex: a colour is resolved against the theme
/// for contrast in light and dark, and stays valid when a theme changes. The
/// colour is always shown with its [`label`](TagColor::label) next to it, so it
/// is never the only way to tell one environment from another.
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
    /// A free-text environment label (e.g. "Production"), shown with its
    /// colour everywhere the connection appears.
    #[serde(default)]
    pub tag: Option<String>,
    /// The palette colour behind [`ConnectionConfig::tag`]. Absent in files
    /// written before tagging existed.
    #[serde(default)]
    pub color: Option<TagColor>,
    /// When this connection was last opened, for most-recent-first ordering.
    #[serde(default)]
    pub last_connected: Option<DateTime<Utc>>,
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
            tag: None,
            color: None,
            last_connected: None,
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
