//! Whole-database backups through the engines' own dump tools.
//!
//! The app already exports single tables as CSV/JSON/SQL, but that is not a
//! backup: it drops the schema, the indexes, and everything that is not one
//! table's rows. This module shells out to `pg_dump` or `mysqldump` instead
//! of reimplementing either dump format, so a backup file is one the
//! engine's own restore path understands.
//!
//! A backup is one dump invocation per selected database, streamed straight
//! into the output file — never held in memory whole. Compression, when the
//! user asks for it, is a gzip stream over that file written with `flate2`,
//! so no external `gzip` binary is needed. SQLite is out of scope: it has
//! no equivalent dump tool, and copying its file is a different feature.
//!
//! Secrets never reach a command line: both tools take the password from
//! the environment (`PGPASSWORD` / `MYSQL_PWD`), which does not show up in
//! a process listing the way a `--password=…` argument would. Anything a
//! failed tool prints is redacted against the password before it is shown,
//! on the off chance a server error echoes it back.

use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::config::{Engine, SslMode};

/// The password is held in memory, like [`Connection`](super::Connection)'s
/// own, and must never be printed: the manual [`Debug`] below redacts it.
pub struct BackupRequest {
    pub engine: Engine,
    /// The host the dump tool connects to — the tunnel's loopback address
    /// when the connection runs through one, not the saved host.
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub ssl_mode: SslMode,
    /// Databases to dump, in the order they are written to the file.
    pub databases: Vec<String>,
    /// Tables to dump instead of the whole database. Only meaningful with
    /// exactly one database (see [`commands_for`]); a table belongs to the
    /// database named here.
    pub tables: Vec<TableSelection>,
    /// Gzip the output file rather than writing the dump plain.
    pub compress: bool,
    pub output: PathBuf,
}

impl fmt::Debug for BackupRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BackupRequest")
            .field("engine", &self.engine)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "********"))
            .field("ssl_mode", &self.ssl_mode)
            .field("databases", &self.databases)
            .field("tables", &self.tables)
            .field("compress", &self.compress)
            .field("output", &self.output)
            .finish()
    }
}

/// One table picked for a table-scoped backup: the database it lives in,
/// and — on Postgres — the schema the sidebar showed for it (`None` is
/// `public`, the schema the sidebar drops).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSelection {
    pub database: String,
    pub schema: Option<String>,
    pub name: String,
}

/// One dump-tool invocation: the program, its arguments, and the database
/// it dumps. The password is deliberately not here — it travels in the
/// child process's environment (see [`run`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpCommand {
    pub program: &'static str,
    pub args: Vec<String>,
    pub database: String,
}

/// Progress while a backup runs: which database is being dumped, and how
/// many bytes of the output file have been written so far. A dump tool
/// reports no percentage, so bytes are the honest measure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupProgress {
    pub database: String,
    pub bytes_written: u64,
}

/// A finished backup: where it landed, how large it is, and what it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupSummary {
    pub output: PathBuf,
    pub bytes_written: u64,
    pub databases: Vec<String>,
}

/// The dump tool for `engine`, or the reason there is none.
pub fn tool_name(engine: Engine) -> Result<&'static str> {
    match engine {
        Engine::Postgres => Ok("pg_dump"),
        Engine::MySql => Ok("mysqldump"),
        Engine::Sqlite => anyhow::bail!(
            "SQLite has no pg_dump/mysqldump equivalent, so whole-database backups are \
             not available for SQLite connections"
        ),
    }
}

/// What to tell the user when the dump tool is not installed: its name, and
/// where to get it, rather than the operating system's bare "not found".
pub fn missing_tool_message(tool: &str) -> String {
    let package = match tool {
        "pg_dump" => "the PostgreSQL client tools",
        "mysqldump" => "the MySQL client tools",
        _ => "the database client tools",
    };
    format!(
        "{tool} was not found on this computer. Install {package} and make sure {tool} \
         is on PATH, then try again."
    )
}

/// A spawn failure as a user-facing error: a missing binary gets the
/// install hint, anything else keeps the operating system's reason.
fn spawn_error(tool: &str, error: std::io::Error) -> anyhow::Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        anyhow::anyhow!(missing_tool_message(tool))
    } else {
        anyhow::anyhow!("could not start {tool}: {error}")
    }
}

/// The `PGSSLMODE` value for a connection's SSL mode, so the dump tool
/// encrypts the way the connection itself does.
fn pg_ssl_mode(mode: SslMode) -> &'static str {
    match mode {
        SslMode::Disable => "disable",
        SslMode::Prefer => "prefer",
        SslMode::Require => "require",
        SslMode::VerifyCa => "verify-ca",
        SslMode::VerifyFull => "verify-full",
    }
}

/// The `mysqldump --ssl-mode` value for a connection's SSL mode.
fn mysql_ssl_mode(mode: SslMode) -> &'static str {
    match mode {
        SslMode::Disable => "DISABLED",
        SslMode::Prefer => "PREFERRED",
        SslMode::Require => "REQUIRED",
        SslMode::VerifyCa => "VERIFY_CA",
        SslMode::VerifyFull => "VERIFY_IDENTITY",
    }
}

/// The dump invocations for a selection, one per database in order.
///
/// The rules the dialog enforces, checked again here so a caller that skips
/// the dialog cannot build a nonsense backup:
///
/// * at least one database, duplicates dropped;
/// * a table selection needs exactly one database — a table-scoped dump is
///   a single-database invocation, and silently dumping the other databases
///   whole (or dropping the tables) would not be what was asked for.
/// * with several databases, each invocation carries `--create`
///   (`pg_dump`) / `--databases` (`mysqldump`), so each chunk of the file
///   creates and selects its own database and the concatenated file
///   restores as several databases rather than one jumbled one. A lone
///   database dumps the way the tools default to — into a database that
///   already exists — which is what a restore into an existing database
///   expects.
pub fn commands_for(
    engine: Engine,
    host: &str,
    port: u16,
    username: &str,
    ssl_mode: SslMode,
    databases: &[String],
    tables: &[TableSelection],
) -> Result<Vec<DumpCommand>> {
    let tool = tool_name(engine)?;
    let mut selected: Vec<String> = Vec::new();
    for database in databases {
        if !database.trim().is_empty() && !selected.contains(database) {
            selected.push(database.clone());
        }
    }
    let databases = selected;
    if databases.is_empty() {
        anyhow::bail!("Select at least one database to back up.");
    }
    if !tables.is_empty() && databases.len() != 1 {
        anyhow::bail!(
            "Tables can only be picked for a single database: back up one database with \
             its tables, or clear the table selection to back up several databases whole."
        );
    }
    let multiple = databases.len() > 1;

    let mut commands = Vec::with_capacity(databases.len());
    for database in &databases {
        let mut args: Vec<String> = Vec::new();
        match engine {
            Engine::Postgres => {
                if !host.is_empty() {
                    args.extend(["-h".to_string(), host.to_string()]);
                }
                if port != 0 {
                    args.extend(["-p".to_string(), port.to_string()]);
                }
                if !username.is_empty() {
                    args.extend(["-U".to_string(), username.to_string()]);
                }
                // Never prompt: stdin is null (see `run`), and a prompt on
                // a headless child would hang the backup instead of
                // failing. The password, when there is one, arrives via
                // `PGPASSWORD`, which this flag does not block.
                args.push("--no-password".to_string());
                if multiple {
                    args.push("--create".to_string());
                }
                args.extend(["-d".to_string(), database.clone()]);
                for table in tables.iter().filter(|table| &table.database == database) {
                    let schema = table.schema.as_deref().unwrap_or("public");
                    args.extend(["-t".to_string(), format!("{schema}.{}", table.name)]);
                }
            }
            Engine::MySql => {
                if !host.is_empty() {
                    args.extend(["-h".to_string(), host.to_string()]);
                }
                if port != 0 {
                    args.extend(["-P".to_string(), port.to_string()]);
                }
                if !username.is_empty() {
                    args.extend(["-u".to_string(), username.to_string()]);
                }
                // Set explicitly rather than inherited: the tool's default
                // is PREFERRED, but saying so keeps the dump's encryption
                // from depending on the environment the app was launched in.
                args.extend([
                    "--ssl-mode".to_string(),
                    mysql_ssl_mode(ssl_mode).to_string(),
                ]);
                if multiple {
                    args.push("--databases".to_string());
                }
                args.push(database.clone());
                for table in tables.iter().filter(|table| &table.database == database) {
                    args.push(table.name.clone());
                }
            }
            Engine::Sqlite => unreachable!("tool_name refuses SQLite above"),
        }
        commands.push(DumpCommand {
            program: tool,
            args,
            database: database.clone(),
        });
    }
    Ok(commands)
}

/// The file name a save prompt suggests for a backup of `database`.
pub fn suggested_file_name(database: &str, compress: bool) -> String {
    if compress {
        format!("{database}-backup.sql.gz")
    } else {
        format!("{database}-backup.sql")
    }
}

/// The output path for `compress`, given the path the save prompt returned.
///
/// The prompt adds an extension only when the name has none, so a name
/// typed with the "wrong" one is fixed up here: a gzipped backup ends in
/// `.gz` (keeping a `.sql` before it), and a plain backup never ends in
/// `.gz` — a plain dump named `.gz` would mislead every tool that reads it.
pub fn normalize_output_path(path: &Path, compress: bool) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    let Some(mut name) = name else {
        return path.to_path_buf();
    };
    if compress {
        if !name.ends_with(".gz") {
            name.push_str(".gz");
        }
    } else if let Some(stripped) = name.strip_suffix(".gz") {
        name = stripped.to_string();
        if !name.contains('.') {
            name.push_str(".sql");
        }
    }
    path.with_file_name(name)
}

/// `text` with every occurrence of `password` replaced, so a tool error that
/// echoes a credential back never reaches the screen or a copied log.
fn redact(text: &str, password: Option<&str>) -> String {
    match password.filter(|password| !password.is_empty()) {
        Some(password) => text.replace(password, "********"),
        None => text.to_string(),
    }
}

/// A gzip stream fed one chunk at a time: each [`push`](GzipSink::push)
/// returns the compressed bytes for that chunk, and
/// [`finish`](GzipSink::finish) the tail of the stream.
struct GzipSink {
    encoder: flate2::write::GzEncoder<Vec<u8>>,
}

impl GzipSink {
    fn new() -> Self {
        Self {
            encoder: flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> std::io::Result<Vec<u8>> {
        self.encoder.write_all(bytes)?;
        self.encoder.flush()?;
        Ok(std::mem::take(self.encoder.get_mut()))
    }

    fn finish(self) -> std::io::Result<Vec<u8>> {
        self.encoder.finish()
    }
}

/// How much of a tool's stderr is kept for the error message: enough to say
/// what went wrong, not a whole dump log.
const STDERR_TAIL: usize = 2000;

/// The last `count` characters of `text`, on a character boundary.
fn tail_chars(text: &str, count: usize) -> String {
    let skip = text.chars().count().saturating_sub(count);
    text.chars().skip(skip).collect()
}

/// Bytes of stderr a failing tool may have produced before the reader stops
/// keeping them — it still drains the pipe past this, so a chatty tool
/// cannot block on a full pipe.
const STDERR_KEEP: usize = 32 * 1024;

/// Run a backup: one dump invocation per database, streamed into
/// `request.output` (gzipped when asked). The output file is removed when
/// a run fails, so a failed backup never leaves a truncated file that
/// looks complete.
///
/// The child processes are killed if the task running this is aborted —
/// [`tokio::process::Command::kill_on_drop`] — which is how the dialog's
/// Cancel reaches a dump already in flight.
pub async fn run(
    request: BackupRequest,
    progress: tokio::sync::mpsc::UnboundedSender<BackupProgress>,
) -> Result<BackupSummary> {
    let commands = commands_for(
        request.engine,
        &request.host,
        request.port,
        &request.username,
        request.ssl_mode,
        &request.databases,
        &request.tables,
    )?;

    let mut file = tokio::fs::File::create(&request.output)
        .await
        .with_context(|| format!("could not create {}", request.output.display()))?;
    let result = run_commands(&request, &commands, &mut file, &progress).await;
    match result {
        Ok(bytes_written) => Ok(BackupSummary {
            output: request.output.clone(),
            bytes_written,
            databases: commands
                .iter()
                .map(|command| command.database.clone())
                .collect(),
        }),
        Err(error) => {
            // A failed run's file is worse than no file: a truncated dump
            // restores partway and then stops, which reads as data loss.
            let _ = tokio::fs::remove_file(&request.output).await;
            Err(error)
        }
    }
}

async fn run_commands(
    request: &BackupRequest,
    commands: &[DumpCommand],
    file: &mut tokio::fs::File,
    progress: &tokio::sync::mpsc::UnboundedSender<BackupProgress>,
) -> Result<u64> {
    let mut sink = request.compress.then(GzipSink::new);
    let mut bytes_written: u64 = 0;

    for command in commands {
        let mut cmd = tokio::process::Command::new(command.program);
        cmd.args(&command.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .env("PGSSLMODE", pg_ssl_mode(request.ssl_mode));
        // The password travels only in the environment, never on the
        // command line. When the connection has none stored, both
        // variables are removed rather than emptied, so a password the
        // app itself was launched with cannot leak into the child.
        match request.password.as_deref() {
            Some(password) => {
                cmd.env("PGPASSWORD", password);
                cmd.env("MYSQL_PWD", password);
            }
            None => {
                cmd.env_remove("PGPASSWORD");
                cmd.env_remove("MYSQL_PWD");
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|error| spawn_error(command.program, error))?;

        let mut stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        let stderr_task = tokio::spawn(async move {
            let mut kept = Vec::new();
            let mut reader = stderr;
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer).await {
                    Ok(0) => break,
                    Ok(read) => {
                        let chunk = &buffer[..read];
                        if kept.len() < STDERR_KEEP {
                            let room = STDERR_KEEP - kept.len();
                            kept.extend_from_slice(&chunk[..chunk.len().min(room)]);
                        }
                    }
                    Err(_) => break,
                }
            }
            kept
        });

        let mut buffer = [0u8; 65536];
        loop {
            match stdout.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => {
                    let chunk = &buffer[..read];
                    let written = match sink.as_mut() {
                        Some(sink) => sink.push(chunk)?,
                        None => chunk.to_vec(),
                    };
                    bytes_written += written.len() as u64;
                    file.write_all(&written)
                        .await
                        .with_context(|| format!("could not write {}", request.output.display()))?;
                    let _ = progress.send(BackupProgress {
                        database: command.database.clone(),
                        bytes_written,
                    });
                }
                Err(error) => {
                    let _ = child.kill().await;
                    let _ = stderr_task.await;
                    return Err(error).with_context(|| {
                        format!("could not read output from {}", command.program)
                    });
                }
            }
        }

        let status = child
            .wait()
            .await
            .with_context(|| format!("could not wait for {} to finish", command.program))?;
        let stderr = stderr_task.await.unwrap_or_default();
        if !status.success() {
            let text = String::from_utf8_lossy(&stderr);
            let text = redact(&text, request.password.as_deref());
            let tail = tail_chars(&text, STDERR_TAIL);
            let detail = if tail.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", tail.trim())
            };
            anyhow::bail!(
                "{} failed for database {} ({status}){detail}",
                command.program,
                command.database,
            );
        }
    }

    if let Some(sink) = sink {
        let tail = sink.finish()?;
        bytes_written += tail.len() as u64;
        file.write_all(&tail)
            .await
            .with_context(|| format!("could not write {}", request.output.display()))?;
    }
    file.flush()
        .await
        .with_context(|| format!("could not write {}", request.output.display()))?;
    Ok(bytes_written)
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;

    fn tables(database: &str, names: &[&str]) -> Vec<TableSelection> {
        names
            .iter()
            .map(|name| TableSelection {
                database: database.to_string(),
                schema: None,
                name: (*name).to_string(),
            })
            .collect()
    }

    #[test]
    fn a_postgres_backup_passes_host_port_user_and_database() {
        let commands = commands_for(
            Engine::Postgres,
            "db.example",
            5432,
            "alan",
            SslMode::Prefer,
            &["app".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].program, "pg_dump");
        assert_eq!(
            commands[0].args,
            vec![
                "-h",
                "db.example",
                "-p",
                "5432",
                "-U",
                "alan",
                "--no-password",
                "-d",
                "app",
            ]
        );
    }

    #[test]
    fn postgres_tables_are_schema_qualified_and_public_is_the_default() {
        let commands = commands_for(
            Engine::Postgres,
            "localhost",
            5432,
            "alan",
            SslMode::Prefer,
            &["app".to_string()],
            &[
                TableSelection {
                    database: "app".to_string(),
                    schema: None,
                    name: "users".to_string(),
                },
                TableSelection {
                    database: "app".to_string(),
                    schema: Some("audit".to_string()),
                    name: "events".to_string(),
                },
            ],
        )
        .unwrap();
        assert_eq!(
            commands[0].args,
            vec![
                "-h",
                "localhost",
                "-p",
                "5432",
                "-U",
                "alan",
                "--no-password",
                "-d",
                "app",
                "-t",
                "public.users",
                "-t",
                "audit.events",
            ]
        );
    }

    #[test]
    fn several_postgres_databases_each_create_their_own() {
        let commands = commands_for(
            Engine::Postgres,
            "localhost",
            5432,
            "alan",
            SslMode::Prefer,
            &["app".to_string(), "other".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(commands.len(), 2);
        assert!(commands[0].args.contains(&"--create".to_string()));
        assert_eq!(commands[0].args.last().unwrap(), "app");
        assert_eq!(commands[1].args.last().unwrap(), "other");
    }

    #[test]
    fn a_mysql_backup_uses_the_capital_port_flag() {
        let commands = commands_for(
            Engine::MySql,
            "db.example",
            3306,
            "alan",
            SslMode::Prefer,
            &["app".to_string()],
            &[],
        )
        .unwrap();
        assert_eq!(commands[0].program, "mysqldump");
        assert!(commands[0].args.contains(&"-P".to_string()));
        assert!(commands[0].args.contains(&"3306".to_string()));
        assert!(!commands[0].args.iter().any(|arg| arg.contains("alan-")));
        assert_eq!(commands[0].args.last().unwrap(), "app");
    }

    #[test]
    fn mysql_tables_follow_the_database_and_need_one_database() {
        let commands = commands_for(
            Engine::MySql,
            "localhost",
            3306,
            "alan",
            SslMode::Prefer,
            &["app".to_string()],
            &tables("app", &["users", "orders"]),
        )
        .unwrap();
        assert_eq!(commands[0].args.last().unwrap(), "orders");
        assert!(commands[0].args.contains(&"users".to_string()));

        let error = commands_for(
            Engine::MySql,
            "localhost",
            3306,
            "alan",
            SslMode::Prefer,
            &["app".to_string(), "other".to_string()],
            &tables("app", &["users"]),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("single database"),
            "{error:#}"
        );
    }

    #[test]
    fn several_mysql_databases_use_the_databases_flag() {
        let commands = commands_for(
            Engine::MySql,
            "localhost",
            3306,
            "alan",
            SslMode::Prefer,
            &["app".to_string(), "other".to_string()],
            &[],
        )
        .unwrap();
        assert!(commands[0].args.contains(&"--databases".to_string()));
        assert_eq!(commands[0].args.last().unwrap(), "app");
    }

    #[test]
    fn no_database_is_an_error_not_an_empty_dump() {
        let error = commands_for(
            Engine::Postgres,
            "localhost",
            5432,
            "alan",
            SslMode::Prefer,
            &[],
            &[],
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("at least one database"),
            "{error:#}"
        );
    }

    #[test]
    fn sqlite_has_no_dump_tool() {
        let error = tool_name(Engine::Sqlite).unwrap_err();
        assert!(format!("{error:#}").contains("SQLite"), "{error:#}");
        assert!(
            commands_for(
                Engine::Sqlite,
                "localhost",
                0,
                "",
                SslMode::Prefer,
                &["x".to_string()],
                &[]
            )
            .is_err()
        );
    }

    #[test]
    fn a_missing_tool_names_the_tool_and_how_to_get_it() {
        assert!(
            missing_tool_message("pg_dump").contains("PostgreSQL client tools"),
            "{}",
            missing_tool_message("pg_dump")
        );
        let not_found = spawn_error("mysqldump", std::io::Error::from_raw_os_error(2));
        assert!(
            format!("{not_found:#}").contains("mysqldump was not found"),
            "{not_found:#}"
        );
    }

    #[test]
    fn a_password_never_appears_in_a_redacted_error() {
        let text = "FATAL: password authentication failed for user \"alan\" (secret-hunter2)";
        assert_eq!(
            redact(text, Some("secret-hunter2")),
            "FATAL: password authentication failed for user \"alan\" (********)"
        );
        assert_eq!(redact(text, None), text);
    }

    #[test]
    fn output_names_keep_sql_before_gz_and_never_lie_about_compression() {
        assert_eq!(
            normalize_output_path(Path::new("/tmp/app-backup.sql"), true),
            PathBuf::from("/tmp/app-backup.sql.gz")
        );
        assert_eq!(
            normalize_output_path(Path::new("/tmp/app-backup.sql.gz"), true),
            PathBuf::from("/tmp/app-backup.sql.gz")
        );
        assert_eq!(
            normalize_output_path(Path::new("/tmp/app-backup.sql.gz"), false),
            PathBuf::from("/tmp/app-backup.sql")
        );
        assert_eq!(
            normalize_output_path(Path::new("/tmp/app-backup.gz"), false),
            PathBuf::from("/tmp/app-backup.sql")
        );
        assert_eq!(suggested_file_name("app", true), "app-backup.sql.gz");
        assert_eq!(suggested_file_name("app", false), "app-backup.sql");
    }

    #[test]
    fn a_gzip_sink_round_trips_through_the_import_decoder() {
        let mut sink = GzipSink::new();
        let mut compressed = sink.push(b"CREATE TABLE t (id int);\n").unwrap();
        compressed.extend(sink.push(b"INSERT INTO t VALUES (7);\n").unwrap());
        compressed.extend(sink.finish().unwrap());

        let mut decoded = String::new();
        flate2::read::MultiGzDecoder::new(&compressed[..])
            .read_to_string(&mut decoded)
            .unwrap();
        assert_eq!(
            decoded,
            "CREATE TABLE t (id int);\nINSERT INTO t VALUES (7);\n"
        );
    }

    #[test]
    fn every_ssl_mode_reaches_the_dump_tool() {
        assert_eq!(pg_ssl_mode(SslMode::VerifyFull), "verify-full");
        assert_eq!(pg_ssl_mode(SslMode::Disable), "disable");
        assert_eq!(mysql_ssl_mode(SslMode::VerifyFull), "VERIFY_IDENTITY");
        assert_eq!(mysql_ssl_mode(SslMode::Disable), "DISABLED");
    }
}
