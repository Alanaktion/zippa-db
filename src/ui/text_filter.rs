//! The live text filter shared by the sidebar and the row panel.

use regex::{Regex, RegexBuilder};

/// Turn a filter box's text into a matcher.
///
/// The pattern is a case-insensitive regex; while it is still being typed it is
/// often not valid (`user(`), so an unparseable pattern falls back to matching
/// the text literally rather than showing nothing. An empty pattern is no
/// filter at all.
pub(crate) fn compile(pattern: &str) -> Option<Regex> {
    if pattern.is_empty() {
        return None;
    }

    let case_insensitive = |pattern: &str| {
        RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .ok()
    };

    case_insensitive(pattern).or_else(|| case_insensitive(&regex::escape(pattern)))
}
