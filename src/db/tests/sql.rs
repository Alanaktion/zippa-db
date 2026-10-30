//! Quoting, placeholders, and casts for generated statements.

use super::*;

#[test]
fn identifiers_are_quoted_only_when_they_have_to_be() {
    assert_eq!(quote_identifier("users", Engine::Postgres), "users");
    assert_eq!(quote_identifier("user_roles", Engine::MySql), "user_roles");

    // Upper case, spaces, and leading digits all need quoting.
    assert_eq!(quote_identifier("Users", Engine::Postgres), "\"Users\"");
    assert_eq!(
        quote_identifier("order items", Engine::MySql),
        "`order items`"
    );
    assert_eq!(quote_identifier("2fa", Engine::Sqlite), "\"2fa\"");

    // Embedded quotes are doubled rather than escaped.
    assert_eq!(
        quote_identifier("we\"ird", Engine::Postgres),
        "\"we\"\"ird\""
    );
    assert_eq!(quote_identifier("we`ird", Engine::MySql), "`we``ird`");
}

#[test]
fn only_postgres_casts_its_placeholders() {
    assert_eq!(
        typed_placeholder(Engine::Postgres, 2, "INT4"),
        "cast($2 as INT4)"
    );
    assert_eq!(typed_placeholder(Engine::MySql, 2, "INT"), "?");
    assert_eq!(typed_placeholder(Engine::Sqlite, 2, "INTEGER"), "?");
    // A column the driver could not name is left uncast rather than guessed at.
    assert_eq!(typed_placeholder(Engine::Postgres, 1, ""), "$1");
}

#[test]
fn keyword_literals_are_recognized_up_to_case_and_whitespace() {
    assert_eq!(keyword_literal("now()"), Some("NOW()"));
    assert_eq!(
        keyword_literal(" Current_Timestamp "),
        Some("CURRENT_TIMESTAMP")
    );
    assert_eq!(keyword_literal("current_date"), Some("CURRENT_DATE"));
    assert_eq!(keyword_literal("current_time"), Some("CURRENT_TIME"));
    // Text that merely contains a keyword is still just text.
    assert_eq!(keyword_literal("now"), None);
    assert_eq!(keyword_literal("it's now()"), None);
    assert_eq!(keyword_literal(""), None);
}

#[test]
fn a_width_free_cast_stands_in_for_one_that_would_truncate() {
    // `cast($1 as CHAR)` is `character(1)`, and `cast($1 as BIT)` is `bit(1)`,
    // so neither may be used for a column wider than that.
    assert_eq!(
        typed_placeholder(Engine::Postgres, 1, "CHAR"),
        "cast($1 as text)"
    );
    assert_eq!(
        typed_placeholder(Engine::Postgres, 2, "BIT"),
        "cast($2 as varbit)"
    );
    assert_eq!(
        typed_placeholder(Engine::Postgres, 3, "VARBIT"),
        "cast($3 as varbit)"
    );
    // Everything else is cast to the column's own type.
    assert_eq!(
        typed_placeholder(Engine::Postgres, 1, "TIMESTAMPTZ"),
        "cast($1 as TIMESTAMPTZ)"
    );
    assert_eq!(
        typed_placeholder(Engine::Postgres, 1, "INT4[]"),
        "cast($1 as INT4[])"
    );
    assert_eq!(typed_placeholder(Engine::Postgres, 1, ""), "$1");
}

#[test]
fn mysql_bit_digits_are_converted_rather_than_stored_as_text() {
    assert_eq!(
        typed_placeholder(Engine::MySql, 1, "BIT"),
        "cast(conv(?, 2, 10) as unsigned)"
    );
    assert_eq!(typed_placeholder(Engine::MySql, 1, "DATETIME"), "?");
    assert_eq!(typed_placeholder(Engine::Sqlite, 1, "DATETIME"), "?");
}

#[test]
fn mysql_metadata_escapes_a_backslash_the_way_the_server_reads_it() {
    // MySQL reads a backslash inside a string as an escape, so a table name
    // holding one has to be doubled or the `information_schema` query names a
    // different table. Every metadata builder that pastes a name must do it.
    let name = r"odd\name";
    let queries = [
        crate::db::mysql::primary_key_sql(name),
        crate::db::mysql::columns_sql(name),
        crate::db::mysql::indexes_sql(name),
        crate::db::mysql::foreign_keys_sql(name),
    ];
    for sql in queries {
        assert!(
            sql.contains(r"'odd\\name'"),
            "the name should be escaped for MySQL: {sql}"
        );
    }
}
