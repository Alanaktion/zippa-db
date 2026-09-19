//! Writing SQL by hand: quoting names, quoting literals, and placing bind
//! parameters so every engine accepts them.
//!
//! Used wherever the app generates a statement — the table view's paging,
//! filtering, and writes — never for the user's own editor buffer, which is
//! run verbatim.

use super::config::Engine;

/// Quote `name` when it would not survive being pasted into a statement bare.
///
/// Plain lower-case identifiers are left alone so the generated SQL reads the
/// way someone would type it.
pub fn quote_identifier(name: &str, engine: Engine) -> String {
    let bare = !name.is_empty()
        && !name.starts_with(|character: char| character.is_ascii_digit())
        && name.chars().all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        });

    if bare {
        return name.to_string();
    }

    match engine {
        Engine::MySql => format!("`{}`", name.replace('`', "``")),
        Engine::Postgres | Engine::Sqlite => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// Quote `value` as a SQL string literal.
///
/// Used only for the metadata queries that cannot take a bind parameter (a
/// `PRAGMA` table function, for one); user data goes through
/// [`Connection::execute`](super::Connection::execute).
pub(crate) fn quote_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Quote `value` as a SQL string literal for `engine`.
///
/// MySQL reads a backslash inside a string as the start of an escape sequence,
/// so a value holding one — a Windows path, a JSON document with `\n` in it —
/// has to have it doubled or the server reads a different value back. The other
/// two engines take a backslash literally.
pub(crate) fn quote_literal_for(engine: Engine, value: &str) -> String {
    let escaped = match engine {
        Engine::MySql => value.replace('\\', "\\\\").replace('\'', "''"),
        Engine::Postgres | Engine::Sqlite => value.replace('\'', "''"),
    };
    format!("'{escaped}'")
}

/// The placeholder for the `index`-th bind parameter, counting from one.
pub(crate) fn placeholder(engine: Engine, index: usize) -> String {
    match engine {
        Engine::Postgres => format!("${index}"),
        Engine::MySql | Engine::Sqlite => "?".to_string(),
    }
}

/// A placeholder that will be accepted where a `type_name` value belongs.
///
/// Every parameter is bound as text. MySQL and SQLite coerce that to the
/// column's type on their own; Postgres refuses it outright, so its
/// placeholder is cast. Type names come back from the driver (`INT4`,
/// `TIMESTAMPTZ`, `INT4[]`), and all of them are castable as written — bar a
/// user-defined type whose name is not lower case, which Postgres down-cases
/// and then fails to find.
pub(crate) fn typed_placeholder(engine: Engine, index: usize, type_name: &str) -> String {
    let placeholder = placeholder(engine, index);
    match engine {
        // A `BIT` shows as its digits, which MySQL would otherwise store as
        // the bytes of the digits themselves.
        Engine::MySql if type_name.eq_ignore_ascii_case("BIT") => {
            format!("cast(conv({placeholder}, 2, 10) as unsigned)")
        }
        Engine::Postgres if !type_name.is_empty() => {
            format!("cast({placeholder} as {})", cast_target(type_name))
        }
        _ => placeholder,
    }
}

/// The type a Postgres parameter is cast to on its way into a `type_name`
/// column.
///
/// Usually the column's own type, but a driver type name carries no length:
/// `CHAR` alone means `character(1)` and `BIT` alone means `bit(1)`, so casting
/// to either would quietly cut the value down to one character or one bit.
/// Both have a width-free relative that the column accepts on assignment and
/// compares against, so the cast goes through that instead and the column
/// itself decides the width.
fn cast_target(type_name: &str) -> &str {
    match type_name.to_ascii_uppercase().as_str() {
        "CHAR" | "BPCHAR" => "text",
        "BIT" | "VARBIT" => "varbit",
        _ => type_name,
    }
}

/// The type a value is cast to when SQL needs it as text, per engine.
pub(crate) fn text_type(engine: Engine) -> &'static str {
    match engine {
        // MySQL has no `text` in a cast; `char` is its stand-in for one.
        Engine::MySql => "char",
        Engine::Postgres | Engine::Sqlite => "text",
    }
}

/// Keyword calls a cell accepts as a literal value, the way it already
/// accepts `NULL`: recognized up to case, all three engines evaluate every
/// one of them the same way.
const KEYWORD_LITERALS: &[&str] = &["NOW()", "CURRENT_TIMESTAMP", "CURRENT_DATE", "CURRENT_TIME"];

/// `value`, if it is one of [`KEYWORD_LITERALS`] up to case — the canonical
/// spelling to paste into the statement as written, rather than bind as a
/// parameter.
///
/// A bound parameter is always text, so a driver would quote `NOW()` and the
/// server would either store the four characters or refuse them outright,
/// rather than evaluate the call.
pub(crate) fn keyword_literal(value: &str) -> Option<&'static str> {
    let trimmed = value.trim();
    KEYWORD_LITERALS
        .iter()
        .find(|keyword| keyword.eq_ignore_ascii_case(trimmed))
        .copied()
}
