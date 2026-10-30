//! Shared fixtures for the database-layer tests, one file per area. SQLite covers
//! the query path with no external server; `live` holds the `#[ignore]`d tests
//! against real MySQL and Postgres.

use std::env;
use std::path::PathBuf;
use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{AssertSqlSafe, Executor};
use uuid::Uuid;

use super::query::Cell;
use super::schema::ReferentialAction;
use super::{
    CatalogKind, Connection, ConnectionConfig, Credentials, DatabaseObject, Engine, ObjectKind,
    QueryDigest, RowKey, SafetyMode, SshAuth, SshConfig, SslConfig, SslMode, TagColor,
    binary_placeholder, keyword_literal, quote_identifier, typed_placeholder,
};
use super::{
    Decision, OnFailure, PinnedConnection, QueryOutcome, QuerySource, ScriptFailure, ScriptMode,
    ScriptOutcome, ScriptRun, Step, TxnState,
};

pub(crate) struct TempDatabase {
    path: PathBuf,
}

impl TempDatabase {
    /// Create a SQLite file with one populated table.
    pub(crate) async fn new() -> Self {
        let path = env::temp_dir().join(format!("zippa-db-test-{}.sqlite", Uuid::new_v4()));
        let pool = SqlitePoolOptions::new()
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&path)
                    .create_if_missing(true),
            )
            .await
            .expect("could not create the test database");

        pool.execute(AssertSqlSafe(
            "CREATE TABLE items (
                id INTEGER PRIMARY KEY,
                name TEXT,
                score REAL,
                payload BLOB
            );
            INSERT INTO items VALUES (1, 'alpha', 1.5, x'001122');
            INSERT INTO items VALUES (2, NULL, NULL, NULL);
            CREATE VIEW named_items AS SELECT id, name FROM items WHERE name IS NOT NULL;"
                .to_string(),
        ))
        .await
        .expect("could not seed the test database");
        pool.close().await;

        Self { path }
    }

    pub(crate) fn config(&self) -> ConnectionConfig {
        ConnectionConfig {
            database: self.path.to_string_lossy().to_string(),
            ..ConnectionConfig::new(Engine::Sqlite)
        }
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The object the sidebar would hand a table view for `name`.
fn table(name: &str) -> DatabaseObject {
    DatabaseObject {
        schema: None,
        name: name.to_string(),
        kind: ObjectKind::Table,
    }
}

/// Open `database` the way a session holds a connection, for a script run.
async fn shared(database: &TempDatabase) -> Arc<Connection> {
    Arc::new(
        Connection::open(database.config(), None)
            .await
            .expect("could not open the test database"),
    )
}

/// The outcome of a script that is expected to have run to the end.
fn finished(step: Result<Step, anyhow::Error>) -> ScriptOutcome {
    match step.expect("the script could not run") {
        Step::Finished(outcome) => outcome,
        Step::Paused(_, failure) => panic!("the script paused on {failure:?}"),
    }
}

/// The run and its failure, for a script expected to have paused.
fn paused(step: Result<Step, anyhow::Error>) -> (ScriptRun, ScriptFailure) {
    match step.expect("the script could not run") {
        Step::Paused(run, failure) => (*run, failure),
        Step::Finished(outcome) => panic!("the script finished: {outcome:?}"),
    }
}

async fn item_count(connection: &Connection) -> String {
    connection
        .run_query("SELECT COUNT(*) FROM items")
        .await
        .expect("the count failed")
        .rows[0][0]
        .clone()
        .expect("a count is never NULL")
}

/// Two inserts with a failure between them.
const FAILS_IN_THE_MIDDLE: &str = "INSERT INTO items VALUES (3, 'gamma', 3.0, NULL);\n\
                                   INSERT INTO not_a_table VALUES (1);\n\
                                   INSERT INTO items VALUES (4, 'delta', 4.0, NULL);";

/// The seeded database, opened in `safety` mode.
async fn open_with(database: &TempDatabase, safety: SafetyMode) -> Connection {
    let config = ConnectionConfig {
        safety,
        ..database.config()
    };
    Connection::open(config, None)
        .await
        .expect("could not open the test database")
}

/// Every label in a tree, for tests that only care that one is there.
fn labels(node: &super::plan::PlanNode) -> Vec<String> {
    let mut all = vec![node.label.clone()];
    for child in &node.children {
        all.extend(labels(child));
    }
    all
}

/// The jump host the SSH tunnel live tests sign in to, and the Postgres server
/// as that host sees it. The defaults are the compose `ssh` service, which
/// reaches the compose `postgres` service by name; `ZIPPA_TEST_SSH_URL`
/// (`ssh://user:password@host:port`) and `ZIPPA_TEST_SSH_TARGET` (`host:port`)
/// point them elsewhere, such as a local `sshd` that reaches Postgres as
/// `127.0.0.1:5433`.
fn live_ssh() -> (SshConfig, String, String, u16) {
    let url = env::var("ZIPPA_TEST_SSH_URL")
        .unwrap_or_else(|_| "ssh://tunnel:secret@127.0.0.1:2222".to_string());
    let rest = url.strip_prefix("ssh://").expect("an ssh:// URL");
    let (credentials, authority) = rest.split_once('@').expect("user:pass@host:port");
    let (username, password) = credentials.split_once(':').expect("user:pass");
    let (host, port) = authority.split_once(':').expect("host:port");
    let ssh = SshConfig {
        enabled: true,
        host: host.to_string(),
        port: port.parse().expect("a port"),
        username: username.to_string(),
        auth: SshAuth::Password,
        key_path: String::new(),
    };

    let target = env::var("ZIPPA_TEST_SSH_TARGET").unwrap_or_else(|_| "postgres:5432".to_string());
    let (target_host, target_port) = target.split_once(':').expect("host:port");
    (
        ssh,
        password.to_string(),
        target_host.to_string(),
        target_port.parse().expect("a port"),
    )
}

/// The console shows every statement the connection sent, so a password in
/// one would be on screen: the log keeps a stand-in instead.
fn assert_passwords_not_logged(connection: &Connection, passwords: &[&str]) {
    let log = connection.query_log().snapshot();
    assert!(
        log.iter().any(|entry| entry.sql.contains("********")),
        "the password statements should be logged with a stand-in"
    );
    for entry in &log {
        for password in passwords {
            assert!(
                !entry.sql.contains(password),
                "a password reached the query log: {}",
                entry.sql
            );
        }
    }
}

/// The first column of [`Connection::users`], which is the name on both
/// engines.
async fn user_names(connection: &Connection) -> Vec<String> {
    connection
        .users()
        .await
        .expect("could not read the user list")
        .rows
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect()
}

/// The same, with the saved config adjusted by `adjust` before it is opened.
async fn live_postgres_with(
    var: &str,
    database: &str,
    adjust: impl FnOnce(&mut ConnectionConfig),
) -> Connection {
    let (mut config, password) = live_postgres_config(var, database);
    adjust(&mut config);
    Connection::open(config, Some(password))
        .await
        .expect("could not open the live Postgres connection")
}

/// The saved connection and password `live_postgres` opens, for a test that
/// opens it some other way.
fn live_postgres_config(var: &str, database: &str) -> (ConnectionConfig, String) {
    let url = env::var(var)
        .unwrap_or_else(|_| format!("postgres://postgres:secret@127.0.0.1:5433/{database}"));
    let rest = url.strip_prefix("postgres://").expect("a postgres:// URL");
    let (credentials, rest) = rest.split_once('@').expect("user:pass@host:port/db");
    let (username, password) = credentials.split_once(':').expect("user:pass");
    let (authority, database) = rest.split_once('/').expect("host:port/db");
    let (host, port) = authority.split_once(':').expect("host:port");
    let port: u16 = port.parse().expect("a port");

    let config = ConnectionConfig {
        host: host.to_string(),
        port,
        username: username.to_string(),
        database: database.to_string(),
        ..ConnectionConfig::new(Engine::Postgres)
    };
    (config, password.to_string())
}

/// The saved connection and password `live_mysql` opens, for a test that
/// opens it some other way.
fn live_mysql_config() -> (ConnectionConfig, String) {
    let url = env::var("ZIPPA_TEST_MYSQL_URL")
        .unwrap_or_else(|_| "mysql://root:secret@127.0.0.1:3307/app".to_string());
    let rest = url.strip_prefix("mysql://").expect("a mysql:// URL");
    let (credentials, rest) = rest.split_once('@').expect("user:pass@host:port/db");
    let (username, password) = credentials.split_once(':').expect("user:pass");
    let (authority, database) = rest.split_once('/').expect("host:port/db");
    let (host, port) = authority.split_once(':').expect("host:port");
    let port: u16 = port.parse().expect("a port");

    let config = ConnectionConfig {
        host: host.to_string(),
        port,
        username: username.to_string(),
        database: database.to_string(),
        ..ConnectionConfig::new(Engine::MySql)
    };
    (config, password.to_string())
}

/// Open the MySQL server a live test runs against, and a pool onto the same
/// database for fixtures that need the text protocol.
async fn live_mysql() -> (Connection, sqlx::MySqlPool) {
    live_mysql_with(|_| {}).await
}

/// The same, with the saved config adjusted by `adjust` before it is opened.
async fn live_mysql_with(
    adjust: impl FnOnce(&mut ConnectionConfig),
) -> (Connection, sqlx::MySqlPool) {
    use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};

    let (mut config, password) = live_mysql_config();
    let options = MySqlConnectOptions::new()
        .host(&config.host)
        .port(config.port)
        .username(&config.username)
        .password(&password)
        .database(&config.database);
    let pool = MySqlPoolOptions::new()
        .connect_with(options)
        .await
        .expect("could not open the live MySQL server");

    adjust(&mut config);
    let connection = Connection::open(config, Some(password))
        .await
        .expect("could not open the live MySQL connection");

    (connection, pool)
}

/// Open a Postgres connection for a live test, against the database named by
/// `var` (falling back to `127.0.0.1:5433`/`database` when unset).
///
/// `pub(crate)` rather than private: `ui::tests` reuses this for the one live
/// test that needs a second real database to switch to, since the fast
/// SQLite-backed suite has no such thing.
pub(crate) async fn live_postgres(var: &str, database: &str) -> Connection {
    live_postgres_with(var, database, |_| {}).await
}

mod agent;
mod binary;
mod config_store;
mod connection;
mod explain;
mod live;
mod pinned;
mod safety;
mod schema;
mod script;
mod sql;
mod sqlite;
