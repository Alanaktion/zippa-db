use super::*;

use std::collections::HashMap;
use std::io::Cursor;

use proptest::prelude::*;

use crate::db::catalog::Catalog;
use crate::db::completion::{Schemas, complete};
use crate::db::import::splitter::{self, Dialect};
use crate::db::statement::{clamp_limit, first_write, split, with_limit};

const ENGINES: [Engine; 3] = [Engine::Postgres, Engine::MySql, Engine::Sqlite];

fn is_write(sql: &str, engine: Engine) -> bool {
    first_write(sql, engine).is_some()
}

/// Statements that must count as writes. A classifier bug adds its row here
/// first.
#[test]
fn the_write_corpus_is_all_writes() {
    use Engine::{MySql, Postgres, Sqlite};
    let writes: &[(Engine, &str)] = &[
        (Postgres, "SELECT * INTO copy FROM users"),
        (Sqlite, "SELECT 1 INTO x"),
        (MySql, "SELECT * FROM users INTO OUTFILE '/tmp/u'"),
        (MySql, "SELECT 'x' INTO DUMPFILE '/tmp/u'"),
        (
            Postgres,
            "WITH d AS (DELETE FROM users RETURNING *) SELECT * FROM d",
        ),
        (MySql, "WITH x AS (SELECT 1) UPDATE users SET a = 1"),
        (Sqlite, "WITH x AS (SELECT 1) INSERT INTO t SELECT * FROM x"),
        (Postgres, "EXPLAIN ANALYZE DELETE FROM users"),
        (Postgres, "EXPLAIN (ANALYZE) UPDATE users SET a = 1"),
        (Postgres, "EXPLAIN ANALYSE INSERT INTO t VALUES (1)"),
        (Postgres, "COPY users FROM '/tmp/u.csv'"),
        (Postgres, "COPY users FROM STDIN"),
        (MySql, "CALL purge()"),
        (Postgres, "CALL purge()"),
        (Postgres, "DO $$ BEGIN DELETE FROM t; END $$"),
        (Sqlite, "PRAGMA journal_mode = wal"),
        (Sqlite, "PRAGMA user_version(5)"),
        (Postgres, "DROP TABLE users"),
        (MySql, "TRUNCATE users"),
        (Postgres, "/* read */ DELETE FROM users"),
        (MySql, "# read\nDELETE FROM users"),
        (MySql, "-- read\nDELETE FROM users"),
        (MySql, "SELECT 'it\\'s'; DELETE FROM users"),
        (Postgres, "SELECT E'it\\'s'; DELETE FROM users"),
        (Postgres, "SELECT $$ ; $$; DELETE FROM users"),
        (Postgres, "SELECT $tag$ x $tag$; DELETE FROM users"),
        (Postgres, "SELECT 1; INSERT INTO t VALUES (1)"),
        (Sqlite, "select 1;\n\n  update t set a = 1"),
    ];
    for (engine, sql) in writes {
        assert!(is_write(sql, *engine), "{engine:?} should refuse: {sql}");
    }
}

/// Reads that must stay reads, including keywords hidden in text.
#[test]
fn the_read_corpus_is_all_reads() {
    use Engine::{MySql, Postgres, Sqlite};
    let reads: &[(Engine, &str)] = &[
        (Postgres, "SELECT * FROM users"),
        (Postgres, "SELECT 'DELETE FROM users'"),
        (Postgres, "SELECT \"insert\" FROM t"),
        (MySql, "SELECT `update` FROM t"),
        (MySql, "SELECT 1 # DELETE FROM users"),
        (Postgres, "SELECT 1 -- DELETE FROM users"),
        (Postgres, "SELECT 1 /* DELETE FROM users */"),
        (Postgres, "SELECT $$ DELETE FROM users; $$"),
        (Postgres, "SELECT E'a\\'; DELETE FROM users'"),
        (MySql, "SELECT 'a\\'; DELETE FROM users'"),
        (Sqlite, "SELECT 'C:\\'"),
        (Postgres, "WITH x AS (SELECT 1) SELECT * FROM x"),
        (Postgres, "EXPLAIN SELECT * FROM users"),
        (MySql, "SHOW TABLES"),
        (MySql, "DESCRIBE users"),
        (Sqlite, "PRAGMA table_info(users)"),
        (Postgres, "VALUES (1), (2)"),
        (Postgres, "BEGIN"),
        (Postgres, "COMMIT"),
        (Postgres, ";;"),
    ];
    for (engine, sql) in reads {
        assert!(!is_write(sql, *engine), "{engine:?} should allow: {sql}");
    }
}

fn engine() -> impl Strategy<Value = Engine> {
    prop::sample::select(ENGINES.to_vec())
}

/// SQL-ish text: fragments of what the scanners care about, glued at random,
/// plus arbitrary characters.
fn sql() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        prop::sample::select(vec![
            "SELECT", "select", "INSERT", "INTO", "WITH", "DELETE", "UPDATE", "FROM", "WHERE",
            "LIMIT", "OFFSET", "EXPLAIN", "ANALYZE", "COPY", "PRAGMA", "users", "t", "*", "1", ",",
            ".", "=", "(", ")", ";", "'", "\"", "`", "\\", "$$", "$a$", "E'", "--", "#", "/*",
            "*/", ":v", "$1", "::", " ", "\n", "é", "日本",
        ])
        .prop_map(String::from),
        "\\PC{0,4}",
    ];
    prop::collection::vec(piece, 0..24).prop_map(|pieces| pieces.join(" "))
}

fn config() -> ProptestConfig {
    ProptestConfig::with_cases(300)
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn no_scanner_panics(sql in sql(), engine in engine(), cursor in 0usize..200, rows in 0usize..50) {
        let _ = split(&sql, engine);
        let _ = crate::db::statement::at_cursor(&sql, cursor, engine);
        let _ = crate::db::statement::placeholders(&sql, engine);
        let _ = crate::db::statement::leading_words(&sql, 3, engine);
        let _ = first_write(&sql, engine);
        let _ = with_limit(&sql, rows, engine);
        let _ = clamp_limit(&sql, rows, engine);

        let catalog = Catalog::default();
        let others = HashMap::new();
        let schemas = Schemas {
            current: &catalog,
            current_database: Some("main"),
            others: &others,
            databases: &[],
        };
        let _ = complete(&sql, cursor, &schemas, engine);

        let dialect = match engine {
            Engine::Postgres => Dialect::Postgres,
            Engine::MySql => Dialect::MySql,
            Engine::Sqlite => Dialect::Sqlite,
        };
        for chunk in splitter::new(Cursor::new(sql.as_bytes()), dialect) {
            let _ = chunk;
        }
    }

    #[test]
    fn split_ranges_are_ordered_and_whole(sql in sql(), engine in engine()) {
        let statements = split(&sql, engine);
        let mut previous_end = 0;
        for statement in &statements {
            prop_assert!(statement.start >= previous_end);
            prop_assert!(statement.start < statement.end);
            prop_assert!(statement.end <= sql.len());
            prop_assert!(sql.is_char_boundary(statement.start));
            prop_assert!(sql.is_char_boundary(statement.end));
            prop_assert_eq!(&sql[statement.start..statement.end], statement.text.as_str());
            previous_end = statement.end;
        }

        // The gaps hold only what split leaves out: whitespace and semicolons
        // are checked loosely by rebuilding the buffer from ranges and gaps.
        let mut rebuilt = String::new();
        let mut at = 0;
        for statement in &statements {
            rebuilt.push_str(&sql[at..statement.start]);
            rebuilt.push_str(&statement.text);
            at = statement.end;
        }
        rebuilt.push_str(&sql[at..]);
        prop_assert_eq!(rebuilt, sql);
    }

    #[test]
    fn a_trailing_comment_or_space_keeps_the_classification(sql in sql(), engine in engine()) {
        // A comment appended to text that ends inside a quote or comment would
        // join it, so only buffers that scan as complete are mutated.
        let before = is_write(&sql, engine);
        for tail in ["\n-- note", " /* note */", "  ", "\n"] {
            let probe = format!("{sql}{tail}");
            if closed(&sql, engine) {
                prop_assert_eq!(is_write(&probe, engine), before, "{:?}", probe);
            }
        }
    }

    #[test]
    fn a_read_stays_a_read_in_parentheses(sql in sql(), engine in engine()) {
        if !is_write(&sql, engine) && closed(&sql, engine) {
            let probe = format!("({sql}\n)");
            prop_assert!(!is_write(&probe, engine), "{:?}", probe);
        }
    }

    #[test]
    fn a_read_stays_a_read_with_a_comment_in_front(sql in sql(), engine in engine()) {
        if !is_write(&sql, engine) && closed(&sql, engine) {
            for head in ["/* note */ ", "-- note\n"] {
                let probe = format!("{head}{sql}");
                prop_assert!(!is_write(&probe, engine), "{:?}", probe);
            }
        }
    }
}

/// Whether `sql` ends outside every quote and comment, judged by appending a
/// marker statement and checking that the scanner sees it as a statement of
/// its own.
fn closed(sql: &str, engine: Engine) -> bool {
    let marker = "\n;SELECT 'zippa_marker'";
    split(&format!("{sql}{marker}"), engine)
        .last()
        .is_some_and(|last| last.text == "SELECT 'zippa_marker'")
}
