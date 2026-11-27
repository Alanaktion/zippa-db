//! Per-engine SQL grammars for the query editor's syntax highlighting.
//!
//! Highlighting comes from gpui-kit's `LanguageRegistry`, which ships one
//! permissive SQL grammar (tree-sitter-sequel) under the name `"sql"`.
//! Zippa-db registers one grammar per engine from zippa-db-tree-sitter —
//! the narrow fork of tree-sitter-sql-polyglot kept for this project
//! (base plus MySQL, MariaDB, PostgreSQL, SQLite) — instead, and the query
//! editor picks its language from the connection's engine, so dialect
//! syntax highlights — and parses — as that dialect. The built-in `"sql"`
//! stays registered as the fallback for editors that do not know their
//! engine.
//!
//! The highlights query for each dialect is vendored in `highlights/`,
//! generated from the fork's sources with its `scripts/bundle-highlights.js`:
//! the ANSI base query plus the dialect's additions, with every pattern
//! naming a node the dialect's parser pruned dropped. The fork ships the
//! same bundles as its `HIGHLIGHTS_QUERY_*` constants, but those are the
//! raw bundles: they name captures gpui-kit rejects (`@field`, ...), so
//! the queries stay vendored and normalized instead of using the constants
//! directly. To regenerate, check out the fork, run
//! `node scripts/bundle-highlights.js postgres mysql sqlite` there, then
//! run `python3 script/normalize-sql-highlights.py /path/to/fork` from
//! this repository to install the queries with Rust regex syntax and
//! gpui-kit capture names.

use gpui_kit::component::highlighter::{GrammarConfig, LanguageRegistry};

use crate::db::Engine;

/// The registry name of the grammar for an engine.
pub fn language_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "postgres",
        Engine::MySql => "mysql",
        Engine::Sqlite => "sqlite",
    }
}

/// Register the three dialect grammars with gpui-kit's language registry.
///
/// Idempotent — registering twice just overwrites the same entries — so
/// tests can call this too. Call once at startup, before any editor is
/// created.
pub fn register_dialect_grammars() {
    let registry = LanguageRegistry::singleton();
    registry.register(
        "postgres",
        &grammar(
            "postgres",
            tree_sitter::Language::new(zippa_db_tree_sitter::LANGUAGE_POSTGRES),
            include_str!("highlights/postgres.scm"),
        ),
    );
    registry.register(
        "mysql",
        &grammar(
            "mysql",
            tree_sitter::Language::new(zippa_db_tree_sitter::LANGUAGE_MYSQL),
            include_str!("highlights/mysql.scm"),
        ),
    );
    registry.register(
        "sqlite",
        &grammar(
            "sqlite",
            tree_sitter::Language::new(zippa_db_tree_sitter::LANGUAGE_SQLITE),
            include_str!("highlights/sqlite.scm"),
        ),
    );
}

/// One dialect's registry entry: its grammar plus its bundled highlights
/// query.
fn grammar(name: &str, language: tree_sitter::Language, highlights: &str) -> GrammarConfig {
    GrammarConfig::new(name, language, Vec::new(), highlights, "", "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registered_language(name: &str) -> tree_sitter::Language {
        register_dialect_grammars();
        LanguageRegistry::singleton()
            .language(name)
            .and_then(|config| config.language)
            .expect("dialect registered with a grammar")
    }

    fn parse(language: &tree_sitter::Language, source: &str) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(language)
            .expect("dialect language loads");
        parser.parse(source, None).expect("source parses")
    }

    #[test]
    fn dialects_are_registered_with_grammars() {
        register_dialect_grammars();
        let registry = LanguageRegistry::singleton();
        for name in ["postgres", "mysql", "sqlite"] {
            let config = registry.language(name);
            assert!(
                config.is_some_and(|config| config.has_grammar()),
                "{name} registered"
            );
        }
        // The built-in permissive grammar stays as the fallback.
        assert!(registry.language("sql").is_some());
    }

    #[test]
    fn language_name_covers_every_engine() {
        assert_eq!(language_name(Engine::Postgres), "postgres");
        assert_eq!(language_name(Engine::MySql), "mysql");
        assert_eq!(language_name(Engine::Sqlite), "sqlite");
    }

    #[test]
    fn dialect_highlights_queries_compile() {
        register_dialect_grammars();
        let registry = LanguageRegistry::singleton();
        for name in ["postgres", "mysql", "sqlite"] {
            let language = registered_language(name);
            let config = registry.language(name).expect("dialect registered");
            if let Err(error) = tree_sitter::Query::new(&language, &config.highlights) {
                panic!("{name} highlights query failed: {error}");
            }
        }
    }

    fn assert_parses(language: &tree_sitter::Language, source: &str, label: &str) {
        let tree = parse(language, source);
        assert!(
            !tree.root_node().has_error(),
            "{label} has an error node:\n{}",
            tree.root_node().to_sexp()
        );
    }

    #[test]
    fn dialects_parse_their_own_syntax_without_error() {
        // Postgres operators and casts, which the permissive grammar also
        // accepts but the dialect grammar must keep.
        let postgres = registered_language("postgres");
        assert_parses(
            &postgres,
            "SELECT data #> '{a,0}' FROM t WHERE x = 'y'::text;",
            "postgres #> and ::",
        );

        // MySQL backticks and LIMIT offset, count, plus a `#` comment,
        // which the fork's MySQL dialect accepts (upstream polyglot #29).
        let mysql = registered_language("mysql");
        assert_parses(
            &mysql,
            "SELECT `a`, `b` FROM `t` WHERE x = 1 LIMIT 1, 10; # a comment",
            "mysql backticks, LIMIT offset, count, and # comment",
        );

        // SQLite pragmas: the no-value form, a numeric value, and a keyword
        // value (`PRAGMA foreign_keys = ON`), which the fork accepts
        // (upstream polyglot #30, fixed in the fork).
        let sqlite = registered_language("sqlite");
        assert_parses(
            &sqlite,
            "PRAGMA foreign_keys; PRAGMA cache_size = 100; PRAGMA foreign_keys = ON; SELECT sql FROM sqlite_master WHERE type = 'table';",
            "sqlite PRAGMA",
        );
    }
}

#[cfg(test)]
mod capture_tests;
