//! What the agent may run, decided in code rather than in the prompt.
//!
//! Three things gate a write and none implies another: the connection's
//! [`AgentAccess`] (does the tool exist), its [`SafetyMode`] (does the
//! connection allow writes at all), and its [`WriteApproval`] (does a person
//! say yes first). [`write_decision`] is the one place they meet, pure so the
//! whole matrix is tested without a database.

use crate::db::statement;
use crate::db::{AgentAccess, Engine, SafetyMode, WriteApproval};

/// What happens to a write the model asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteDecision {
    /// Refused outright; the text is what the model is told.
    Refuse(String),
    /// Shown to the user, who must approve this one statement.
    Ask,
    /// Runs now.
    Run,
}

/// Combine the three gates. A write runs only when every one lets it.
///
/// `Auto` approval runs a write unasked, even on a `ConfirmWrites` or
/// `AutoApply` connection: the choice was made in the agent's own setting.
/// Without it a write always asks, `AutoApply` included — that mode's premise
/// is that the user typed the change, and the model's SQL was not typed by
/// anyone.
pub fn write_decision(
    access: AgentAccess,
    safety: SafetyMode,
    approval: WriteApproval,
) -> WriteDecision {
    if !access.writes_rows() {
        return WriteDecision::Refuse(format!(
            "writes are not available at the {} access level",
            access.label()
        ));
    }
    if safety.is_read_only() {
        return WriteDecision::Refuse(
            "this connection is read-only, so nothing was changed".into(),
        );
    }
    match approval {
        WriteApproval::Never => {
            WriteDecision::Refuse("writes are turned off for the agent on this connection".into())
        }
        WriteApproval::Ask => WriteDecision::Ask,
        WriteApproval::Auto => WriteDecision::Run,
    }
}

/// Words that start a statement the agent never runs, at any access level:
/// they reach the file system or the server process rather than the data.
const FORBIDDEN: [&str; 9] = [
    "ATTACH", "DETACH", "COPY", "LOAD", "KILL", "SHUTDOWN", "INSTALL", "VACUUM", "\\",
];

/// Why a statement is not acceptable to a tool, or what it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Read,
    /// `INSERT` / `UPDATE` / `DELETE`.
    Dml,
    /// DDL, grants, user management — anything else that writes.
    Other,
}

/// One statement or nothing: text with a stray semicolon is still one.
pub fn single(sql: &str, engine: Engine) -> Result<String, String> {
    let mut statements = statement::split(sql, engine);
    match statements.len() {
        0 => Err("the statement is empty".into()),
        1 => Ok(statements.remove(0).text),
        _ => Err("send one statement at a time".into()),
    }
}

/// Sort a single statement into read, DML, or other write.
///
/// Reads must be a plain `SELECT` or `WITH … SELECT`: transaction control
/// counts as a read to [`statement::first_write`], and the agent runs without
/// a transaction, so `BEGIN` is refused here as neither.
pub fn classify(sql: &str, engine: Engine) -> Result<Kind, String> {
    let words = statement::leading_words(sql, 1, engine);
    let first = words.first().map(String::as_str).unwrap_or_default();
    if FORBIDDEN.contains(&first) {
        return Err(format!("{first} is not available to the agent"));
    }
    match statement::first_write(sql, engine) {
        None if first == "SELECT" || first == "WITH" => Ok(Kind::Read),
        None => Err(format!("{first} is not a query the agent can run")),
        Some(_) if matches!(first, "INSERT" | "UPDATE" | "DELETE") => Ok(Kind::Dml),
        Some(_) => Ok(Kind::Other),
    }
}

/// Whether `kind` may run at `access`.
pub fn allowed(kind: &Kind, access: AgentAccess) -> Result<(), String> {
    let ok = match kind {
        Kind::Read => access.reads_rows(),
        Kind::Dml => access.writes_rows(),
        Kind::Other => access.changes_schema(),
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "that statement needs more than the {} access level",
            access.label()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENGINE: Engine = Engine::Postgres;

    #[test]
    fn write_decision_matrix() {
        for access in AgentAccess::ALL {
            for safety in SafetyMode::ALL {
                for approval in WriteApproval::ALL {
                    let got = write_decision(access, safety, approval);
                    let expected = if !access.writes_rows()
                        || safety.is_read_only()
                        || approval == WriteApproval::Never
                    {
                        None
                    } else if approval == WriteApproval::Ask {
                        Some(WriteDecision::Ask)
                    } else {
                        Some(WriteDecision::Run)
                    };
                    match expected {
                        None => assert!(
                            matches!(got, WriteDecision::Refuse(_)),
                            "{access:?} {safety:?} {approval:?}"
                        ),
                        Some(want) => assert_eq!(got, want, "{access:?} {safety:?} {approval:?}"),
                    }
                }
            }
        }
    }

    #[test]
    fn auto_apply_connections_still_ask() {
        assert_eq!(
            write_decision(AgentAccess::Crud, SafetyMode::AutoApply, WriteApproval::Ask),
            WriteDecision::Ask
        );
    }

    #[test]
    fn plain_reads_are_reads() {
        assert_eq!(classify("select 1", ENGINE), Ok(Kind::Read));
        assert_eq!(
            classify("with a as (select 1) select * from a", ENGINE),
            Ok(Kind::Read)
        );
    }

    #[test]
    fn writes_hidden_from_a_read_are_writes() {
        assert_ne!(
            classify("select * into copy_of from t", ENGINE),
            Ok(Kind::Read)
        );
        assert_ne!(
            classify(
                "with d as (delete from t returning *) select * from d",
                ENGINE
            ),
            Ok(Kind::Read)
        );
        assert_ne!(
            classify("explain analyze delete from t", ENGINE),
            Ok(Kind::Read)
        );
        // A comment cannot turn a write into a read, nor a read into a write.
        assert_eq!(classify("/* delete */ select 1", ENGINE), Ok(Kind::Read));
        assert_eq!(classify("-- select\ndelete from t", ENGINE), Ok(Kind::Dml));
    }

    #[test]
    fn several_statements_are_refused() {
        assert!(single("select 1; delete from t", ENGINE).is_err());
        assert!(single("  ;  ", ENGINE).is_err());
        assert_eq!(
            single("select 1;", ENGINE).unwrap().trim_end_matches(';'),
            "select 1"
        );
    }

    #[test]
    fn transaction_control_and_file_access_are_refused() {
        assert!(classify("begin", ENGINE).is_err());
        assert!(classify("copy t to '/tmp/x'", ENGINE).is_err());
        assert!(classify("attach 'x.db' as x", Engine::Sqlite).is_err());
        assert!(classify("kill 4", Engine::MySql).is_err());
    }

    #[test]
    fn access_levels_gate_each_kind() {
        assert!(allowed(&Kind::Read, AgentAccess::Schema).is_err());
        assert!(allowed(&Kind::Read, AgentAccess::ReadOnlySql).is_ok());
        assert!(allowed(&Kind::Dml, AgentAccess::ReadOnlySql).is_err());
        assert!(allowed(&Kind::Dml, AgentAccess::Crud).is_ok());
        assert!(allowed(&Kind::Other, AgentAccess::Crud).is_err());
        assert!(allowed(&Kind::Other, AgentAccess::Full).is_ok());
        assert!(allowed(&Kind::Read, AgentAccess::None).is_err());
    }
}
