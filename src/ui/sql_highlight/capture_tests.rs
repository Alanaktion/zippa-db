use tree_sitter::{Language, Parser, Query, QueryCursor, StreamingIterator};

fn captures(dialect: &str, source: &str) -> Vec<(String, String)> {
    let (language, highlights) = match dialect {
        "postgres" => (
            zippa_db_tree_sitter::LANGUAGE_POSTGRES,
            include_str!("highlights/postgres.scm"),
        ),
        "mysql" => (
            zippa_db_tree_sitter::LANGUAGE_MYSQL,
            include_str!("highlights/mysql.scm"),
        ),
        "sqlite" => (
            zippa_db_tree_sitter::LANGUAGE_SQLITE,
            include_str!("highlights/sqlite.scm"),
        ),
        _ => unreachable!(),
    };
    let language = Language::new(language);
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{dialect}: {}",
        tree.root_node().to_sexp()
    );
    let query = Query::new(&language, highlights).unwrap();
    for name in query.capture_names() {
        assert!(
            !matches!(
                *name,
                "conditional" | "storageclass" | "field" | "parameter" | "float"
            ),
            "{dialect}: unsupported capture {name}"
        );
    }
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut captures = Vec::new();
    while let Some(matched) = matches.next() {
        for capture in matched.captures {
            captures.push((
                query.capture_names()[capture.index as usize].to_owned(),
                source[capture.node.byte_range()].to_owned(),
            ));
        }
    }
    captures
}

#[test]
fn dialect_literal_and_conditional_captures() {
    for dialect in ["postgres", "mysql", "sqlite"] {
        let captures = captures(
            dialect,
            "SELECT 42, 3.14, -7, +8, 'hello', 'true', 'false', TRUE, FALSE, true, false, CASE WHEN TRUE THEN 1 ELSE 0 END;",
        );
        for (name, text) in [
            ("number", "42"),
            ("number", "3.14"),
            ("number", "-7"),
            ("number", "+8"),
            ("string", "'hello'"),
            ("string", "'true'"),
            ("string", "'false'"),
            ("boolean", "TRUE"),
            ("boolean", "FALSE"),
            ("boolean", "true"),
            ("boolean", "false"),
            ("keyword", "CASE"),
            ("keyword", "WHEN"),
            ("keyword", "THEN"),
            ("keyword", "ELSE"),
        ] {
            assert!(
                captures.contains(&(name.to_owned(), text.to_owned())),
                "{dialect}: missing {name} for {text}: {captures:?}"
            );
        }
        for (name, text) in &captures {
            assert!(
                !(name == "string"
                    && matches!(text.to_ascii_lowercase().as_str(), "true" | "false")),
                "{dialect}: boolean captured as string"
            );
            // The generic (literal) @string rule matches every literal, so
            // without the number exclusions an unquoted number also carries
            // a string capture alongside its number one.
            assert!(
                !(name == "string" && text.parse::<f64>().is_ok()),
                "{dialect}: unquoted number {text} captured as string"
            );
        }
    }
}
