//! Laying a single value out for a human to read.
//!
//! The grid holds display strings rather than the values themselves, so this
//! decides how one of them is shown in full — JSON over several lines, a
//! stand-in saying it was never read back — and whether a cell can show it at
//! all or it wants a window of its own.

use crate::db::query::{self, Cell};

/// Lay a cell out for the viewer: JSON is indented, everything else is shown
/// as it stands.
///
/// The grid holds display strings rather than the values themselves, so a
/// value the driver could only describe says so instead of pretending.
pub fn format_value(value: &Cell, type_name: &str) -> String {
    let Some(text) = value else {
        return "NULL".to_string();
    };

    if query::is_placeholder(value) {
        return format!(
            "{text}\n\nThis value was not read back from the server, so there is \
             nothing to show here."
        );
    }

    // A column typed as JSON is laid out even when the value is a bare
    // string; anything else has to look like JSON before it is reformatted.
    // An array is written in braces of its own and is never JSON.
    let upper = type_name.to_ascii_uppercase();
    let json = !upper.ends_with("[]")
        && (upper.starts_with("JSON") || text.trim_start().starts_with(['{', '[']));
    if json && let Some(pretty) = pretty_json(text) {
        return pretty;
    }

    text.clone()
}

/// Values longer than this are read in a window rather than a grid cell.
const LONG_VALUE: usize = 200;

/// Whether a value wants a window of its own rather than the one-line editor.
///
/// Several lines, JSON, something the driver only described, or simply too
/// much text to read through a cell.
pub fn needs_a_window(value: &Cell, type_name: &str) -> bool {
    if query::is_placeholder(value) || query::is_binary_type(type_name) {
        return true;
    }

    let Some(text) = value else {
        return false;
    };

    text.contains('\n')
        || text.chars().count() > LONG_VALUE
        || (type_name.to_ascii_uppercase().starts_with("JSON")
            || text.trim_start().starts_with(['{', '[']))
            && pretty_json(text).is_some()
}

/// `text` laid out over several lines, if it is JSON at all.
fn pretty_json(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_laid_out_for_reading() {
        // Keys keep the order the server sent them in.
        let value = Some(r#"{"b":1,"a":[1,2]}"#.to_string());
        assert_eq!(
            format_value(&value, "JSONB"),
            "{\n  \"b\": 1,\n  \"a\": [\n    1,\n    2\n  ]\n}"
        );

        // A column typed as JSON holding a bare string is still JSON.
        assert_eq!(format_value(&Some("\"hi\"".to_string()), "JSON"), "\"hi\"");
    }

    #[test]
    fn everything_else_is_left_as_it_is() {
        let text = "a long line of plain text".to_string();
        assert_eq!(format_value(&Some(text.clone()), "TEXT"), text);
        assert_eq!(format_value(&None, "TEXT"), "NULL");

        // Something that starts like JSON but is not stays as typed.
        let broken = "{not json".to_string();
        assert_eq!(format_value(&Some(broken.clone()), "TEXT"), broken);
    }

    #[test]
    fn a_value_that_was_never_read_says_so() {
        let described = format_value(&query::blob(b"abc"), "BLOB");
        assert!(described.starts_with("<3 bytes>"), "{described}");
        assert!(described.contains("not read back"), "{described}");
    }
}
