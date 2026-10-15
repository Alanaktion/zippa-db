//! The editor's text edits as pure functions over a buffer and a selection,
//! so every one of them is unit-tested without a window. Each returns the
//! new buffer and where the selection should sit in it, or `None` when there
//! is nothing to change — so a no-op leaves the undo stack alone too.

use std::ops::Range;

/// every non-blank line already carries a comment, and commented as a whole
/// Pretty-print the selection, or the whole buffer when the selection is
/// empty. Pure, so the formatting is unit-tested without a window.
/// Returns `None` when the target is blank or already formatted, so the
/// buffer — and the undo stack — is left alone.
pub(super) fn format_sql(
    text: &str,
    selected: Range<usize>,
    dialect: sqlformat::Dialect,
) -> Option<(String, Range<usize>)> {
    let len = text.len();
    let (start, end) = if selected.is_empty() {
        (0, len)
    } else {
        (selected.start.min(len), selected.end.min(len))
    };
    let target = text.get(start..end)?;
    if target.trim().is_empty() {
        return None;
    }
    let formatted = sqlformat::format(
        target,
        &sqlformat::QueryParams::None,
        &sqlformat::FormatOptions {
            dialect,
            ..Default::default()
        },
    );
    if formatted == target {
        return None;
    }
    let mut new_text = text.to_string();
    new_text.replace_range(start..end, &formatted);
    Some((new_text, start..start + formatted.len()))
}

/// Toggle `-- ` line comments over the lines a selection touches — or the
/// caret's line — returning the new buffer and where the selection should sit
/// in it. `None` when there is nothing to change, so a blank caret line is a
/// no-op rather than an empty edit on the undo stack.
///
/// A blank line within the run is left alone; the run is uncommented only when
/// every non-blank line already carries a comment, and commented as a whole
/// otherwise. The comment goes in after the line's indentation, so the marker
/// lines up with the statement rather than the margin. Pure, so the buffer is
/// reshaped in unit tests without a window.
pub(super) fn toggle_comments(
    text: &str,
    selected: Range<usize>,
) -> Option<(String, Range<usize>)> {
    let len = text.len();
    let start = selected.start.min(len);
    let end = selected.end.min(len);
    // A selection that ends exactly at the start of a line does not take that
    // line with it, the way dragging down to the next line's margin does not.
    let last = if end > start && text[..end].ends_with('\n') {
        end - 1
    } else {
        end
    };

    // The lines the toggle covers, as byte ranges without their newline.
    let mut spans = Vec::new();
    let last_line_end = line_end(text, last);
    let mut cursor = line_start(text, start);
    loop {
        let end_of_line = line_end(text, cursor);
        spans.push((cursor, end_of_line));
        if end_of_line == last_line_end {
            break;
        }
        cursor = end_of_line + 1;
    }

    let blank = |line: &str| line.trim().is_empty();
    let commented = |line: &str| line.trim_start_matches([' ', '\t']).starts_with("--");
    let non_blank: Vec<&str> = spans
        .iter()
        .map(|&(s, e)| &text[s..e])
        .filter(|line| !blank(line))
        .collect();
    let uncomment = !non_blank.is_empty() && non_blank.iter().all(|line| commented(line));

    let mut out = String::with_capacity(len);
    // Where the text was changed and by how much, so a caret or selection in
    // the old buffer can be carried into the new one.
    let mut edits: Vec<(usize, isize)> = Vec::new();
    let mut prev = 0;
    for &(s, e) in &spans {
        out.push_str(&text[prev..s]);
        let line = &text[s..e];
        if blank(line) {
            out.push_str(line);
        } else {
            let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
            let at = s + indent;
            out.push_str(&line[..indent]);
            if uncomment {
                let rest = &line[indent..];
                // `-- ` is what this writes; a hand-written `--x` comes off
                // without taking the `x` with it.
                let marker = if rest.starts_with("-- ") { 3 } else { 2 };
                out.push_str(&rest[marker..]);
                edits.push((at, -(marker as isize)));
            } else {
                out.push_str("-- ");
                out.push_str(&line[indent..]);
                edits.push((at, 3));
            }
        }
        prev = e;
    }
    out.push_str(&text[prev..]);

    if out == text {
        return None;
    }

    let shift = |offset: usize| -> usize {
        let mut shifted = offset as isize;
        for &(at, delta) in &edits {
            if delta > 0 {
                // Strictly after the marker: an endpoint on the marker's own
                // line start keeps to the left of it, so a selection still
                // covers what it commented.
                if offset > at {
                    shifted += delta;
                }
            } else {
                let removed = (-delta) as usize;
                if offset >= at + removed {
                    shifted += delta;
                } else if offset > at {
                    // Inside the comment marker: land on the new line start.
                    shifted -= (offset - at) as isize;
                }
            }
        }
        shifted.clamp(0, out.len() as isize) as usize
    };
    let selection = shift(start)..shift(end);
    Some((out, selection))
}

/// The byte offset of the first character of the line `offset` is on.
fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map(|at| at + 1).unwrap_or(0)
}

/// The byte offset of the newline that ends the line `offset` is on, or the
/// end of the text when it is the last line.
fn line_end(text: &str, offset: usize) -> usize {
    text[offset..]
        .find('\n')
        .map(|at| offset + at)
        .unwrap_or(text.len())
}

/// Byte spans of every line in the buffer, newlines excluded. A trailing
/// newline leaves a final empty span, which is the empty line after it.
fn all_lines(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (at, byte) in text.bytes().enumerate() {
        if byte == b'\n' {
            spans.push((start, at));
            start = at + 1;
        }
    }
    spans.push((start, text.len()));
    spans
}

/// The first and last line indices a selection touches, inclusive. A
/// selection that ends exactly at a line's start does not take that line —
/// the way dragging down to the next line's margin does not.
fn touched_lines(spans: &[(usize, usize)], text: &str, selected: Range<usize>) -> (usize, usize) {
    let len = text.len();
    let start = selected.start.min(len);
    let end = selected.end.min(len);
    let last = if end > start && text[..end].ends_with('\n') {
        end - 1
    } else {
        end
    };
    let line_of = |offset: usize| spans.iter().rposition(|&(s, _)| s <= offset).unwrap_or(0);
    (line_of(start), line_of(last))
}

/// Rebuild the buffer from `order`, a permutation of line indices, joining
/// with newlines. Returns the text and each line's new byte offset, keyed by
/// line index.
fn join_lines(text: &str, spans: &[(usize, usize)], order: &[usize]) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut new_starts = vec![0usize; spans.len()];
    for (at, &line) in order.iter().enumerate() {
        if at > 0 {
            out.push('\n');
        }
        new_starts[line] = out.len();
        let (s, e) = spans[line];
        out.push_str(&text[s..e]);
    }
    (out, new_starts)
}

/// Shift a selection that rides along with a rigidly moved block of lines.
fn shift_selection(selected: Range<usize>, delta: isize, len: usize) -> Range<usize> {
    let shift = |offset: usize| (offset as isize + delta).clamp(0, len as isize) as usize;
    shift(selected.start)..shift(selected.end)
}

/// Delete the lines a selection touches, returning the new buffer and where
/// the caret lands. `None` when there is no line to delete.
pub(super) fn delete_lines(text: &str, selected: Range<usize>) -> Option<(String, Range<usize>)> {
    let spans = all_lines(text);
    let (first, last) = touched_lines(&spans, text, selected);
    let (start, _) = spans[first];
    let (_, end) = spans[last];
    // One newline goes with the lines: the one after them, or the one before
    // when they are the last lines.
    let (rm_start, rm_end) = if end < text.len() {
        (start, end + 1)
    } else if start > 0 {
        (start - 1, end)
    } else {
        (start, end)
    };
    if rm_start == rm_end {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..rm_start]);
    out.push_str(&text[rm_end..]);
    let caret = rm_start.min(out.len());
    Some((out, caret..caret))
}

/// Delete exactly the selected range. `None` for a bare caret.
pub(super) fn delete_range(text: &str, selected: Range<usize>) -> Option<(String, Range<usize>)> {
    if selected.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..selected.start]);
    out.push_str(&text[selected.end..]);
    Some((out, selected.start..selected.start))
}

/// Move the lines a selection touches up or down one line, the selection
/// riding along. `None` when the lines are already at that edge.
pub(super) fn move_lines(
    text: &str,
    selected: Range<usize>,
    up: bool,
) -> Option<(String, Range<usize>)> {
    let spans = all_lines(text);
    let (first, last) = touched_lines(&spans, text, selected.clone());
    let mut order: Vec<usize> = (0..spans.len()).collect();
    if up {
        if first == 0 {
            return None;
        }
        let block: Vec<usize> = order.drain(first..=last).collect();
        for (at, line) in block.into_iter().enumerate() {
            order.insert(first - 1 + at, line);
        }
    } else {
        if last + 1 >= spans.len() {
            return None;
        }
        let block: Vec<usize> = order.drain(first..=last).collect();
        for (at, line) in block.into_iter().enumerate() {
            order.insert(first + 1 + at, line);
        }
    }
    let (out, new_starts) = join_lines(text, &spans, &order);
    let delta = new_starts[first] as isize - spans[first].0 as isize;
    let len = out.len();
    Some((out, shift_selection(selected, delta, len)))
}

/// Duplicate the lines a selection touches above or below themselves, the
/// selection moving onto the new copy the way it does in VS Code.
pub(super) fn duplicate_lines(
    text: &str,
    selected: Range<usize>,
    up: bool,
) -> Option<(String, Range<usize>)> {
    let spans = all_lines(text);
    let (first, last) = touched_lines(&spans, text, selected.clone());
    let block: Vec<usize> = (first..=last).collect();
    let mut order: Vec<usize> = (0..spans.len()).collect();
    // Where the copy's first line lands in the rebuilt order. A line index
    // appears twice now, so the copy's start is tracked by position.
    let copy_at = if up { first } else { last + 1 };
    for (at, line) in block.iter().enumerate() {
        order.insert(copy_at + at, *line);
    }
    let mut out = String::with_capacity(text.len() + spans[last].1 - spans[first].0 + 1);
    let mut copy_start = 0;
    for (at, &line) in order.iter().enumerate() {
        if at > 0 {
            out.push('\n');
        }
        if at == copy_at {
            copy_start = out.len();
        }
        let (s, e) = spans[line];
        out.push_str(&text[s..e]);
    }
    let delta = copy_start as isize - spans[first].0 as isize;
    let len = out.len();
    Some((out, shift_selection(selected, delta, len)))
}

/// The text ⌘C copies for the caret's line: the line plus its newline, so
/// pasting it back yields a whole line.
pub(super) fn line_clip_text(text: &str, caret: usize) -> String {
    let spans = all_lines(text);
    let caret = caret.min(text.len());
    let index = spans.iter().rposition(|&(s, _)| s <= caret).unwrap_or(0);
    let (s, e) = spans[index];
    let mut out = text[s..e].to_string();
    if e < text.len() {
        out.push('\n');
    }
    out
}

/// The range ⌘L selects: the touched lines whole, newlines included. When
/// they are already exactly selected, it extends through the next line.
pub(super) fn select_line_range(text: &str, selected: Range<usize>) -> Range<usize> {
    let spans = all_lines(text);
    let (first, last) = touched_lines(&spans, text, selected.clone());
    let with_break = |index: usize| {
        let (_, e) = spans[index];
        if e < text.len() { e + 1 } else { e }
    };
    let full = spans[first].0..with_break(last);
    if !selected.is_empty() && selected == full && last + 1 < spans.len() {
        spans[first].0..with_break(last + 1)
    } else {
        full
    }
}

/// Where Home — or ⌘◀ — moves the caret: the first non-blank character,
/// unless already there, when it goes to the line's start.
pub(super) fn smart_home_offset(text: &str, caret: usize) -> usize {
    let caret = caret.min(text.len());
    let start = line_start(text, caret);
    let indent = text[start..].len() - text[start..].trim_start_matches([' ', '\t']).len();
    let first_text = start + indent;
    if caret == first_text {
        start
    } else {
        first_text
    }
}

/// Whether a byte is part of a word for occurrence matching: letters,
/// digits, and underscores.
fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// The word under the caret, if the caret touches one.
fn word_at(text: &str, caret: usize) -> Option<Range<usize>> {
    let bytes = text.as_bytes();
    let caret = caret.min(bytes.len());
    let mut start = caret;
    while start > 0 && is_word_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = caret;
    while end < bytes.len() && is_word_byte(bytes[end]) {
        end += 1;
    }
    if start == end { None } else { Some(start..end) }
}

/// The next occurrence of the selected text, wrapping around the buffer.
/// With no selection, the word under the caret is selected first. `None`
/// when there is no other occurrence.
pub(super) fn next_occurrence(text: &str, selected: Range<usize>) -> Option<Range<usize>> {
    let selected = if selected.is_empty() {
        word_at(text, selected.start)?
    } else {
        selected
    };
    let needle = &text[selected.clone()];
    if needle.is_empty() {
        return None;
    }
    let len = needle.len();
    let mut found = text[selected.end..]
        .find(needle)
        .map(|at| selected.end + at);
    if found.is_none() {
        found = text[..selected.end].find(needle);
    }
    let start = found?;
    let range = start..start + len;
    if range == selected { None } else { Some(range) }
}

/// The previous occurrence of the selected text, wrapping around the
/// buffer. With no selection, the word under the caret is selected first.
/// `None` when there is no other occurrence.
pub(super) fn previous_occurrence(text: &str, selected: Range<usize>) -> Option<Range<usize>> {
    let selected = if selected.is_empty() {
        word_at(text, selected.start)?
    } else {
        selected
    };
    let needle = &text[selected.clone()];
    if needle.is_empty() {
        return None;
    }
    let len = needle.len();
    let mut found = text[..selected.start].rfind(needle);
    if found.is_none() {
        found = text[selected.start..]
            .rfind(needle)
            .map(|at| selected.start + at);
    }
    let start = found?;
    let range = start..start + len;
    if range == selected { None } else { Some(range) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Toggle over `text` with `selected` as the buffer selection, which must
    /// change something.
    fn toggle(text: &str, selected: Range<usize>) -> (String, Range<usize>) {
        toggle_comments(text, selected).expect("there should be a change to make")
    }

    #[test]
    fn the_caret_line_is_commented() {
        assert_eq!(toggle("select 1", 3..3), ("-- select 1".to_string(), 6..6));
    }

    #[test]
    fn commenting_keeps_the_indentation() {
        assert_eq!(
            toggle("    select 1", 6..6),
            ("    -- select 1".to_string(), 9..9)
        );
    }

    #[test]
    fn a_commented_line_comes_back_off() {
        assert_eq!(toggle("-- select 1", 3..3), ("select 1".to_string(), 0..0));
        assert_eq!(
            toggle("  -- select 1", 5..5),
            ("  select 1".to_string(), 2..2)
        );
    }

    #[test]
    fn a_marker_with_no_space_comes_off_cleanly() {
        assert_eq!(toggle("--select 1", 2..2), ("select 1".to_string(), 0..0));
    }

    /// Format `text` with `selected` as the buffer selection, which must
    /// change something.
    fn format(text: &str, selected: Range<usize>) -> (String, Range<usize>) {
        format_sql(text, selected, sqlformat::Dialect::Generic)
            .expect("there should be a change to make")
    }

    #[test]
    fn an_empty_selection_formats_the_whole_buffer() {
        let (new, selection) = format("select a, b from t", 4..4);
        assert_eq!(new, "select\n  a,\n  b\nfrom\n  t");
        assert_eq!(selection, 0..new.len());
    }

    #[test]
    fn a_selection_formats_only_itself() {
        let (new, selection) = format("select 1;select a, b from t", 9..27);
        assert_eq!(new, "select 1;select\n  a,\n  b\nfrom\n  t");
        assert_eq!(selection, 9..new.len());
    }

    #[test]
    fn already_formatted_text_is_left_alone() {
        assert_eq!(
            format_sql("select\n  a\nfrom\n  t", 0..0, sqlformat::Dialect::Generic),
            None
        );
    }

    #[test]
    fn blank_text_is_left_alone() {
        assert_eq!(
            format_sql("  \n  ", 0..0, sqlformat::Dialect::Generic),
            None
        );
        assert_eq!(format_sql("", 0..0, sqlformat::Dialect::Generic), None);
    }

    #[test]
    fn a_selection_comments_every_line_it_touches() {
        let (new, selection) = toggle("select 1\nselect 2\nselect 3", 0..18);
        assert_eq!(new, "-- select 1\n-- select 2\nselect 3");
        assert_eq!(
            selection,
            0..24,
            "the selection should still cover the lines it commented"
        );
    }

    #[test]
    fn a_selection_ending_at_the_next_line_does_not_take_it() {
        let (new, _) = toggle("select 1\nselect 2", 0..9);
        assert_eq!(new, "-- select 1\nselect 2");
    }

    #[test]
    fn a_fully_commented_run_is_uncommented_together() {
        let text = "-- select 1\n  -- select 2";
        let (new, selection) = toggle(text, 0..text.len());
        assert_eq!(new, "select 1\n  select 2");
        assert_eq!(selection, 0..19);
    }

    #[test]
    fn a_mixed_run_is_commented_as_a_whole() {
        let text = "-- select 1\nselect 2";
        let (new, _) = toggle(text, 0..text.len());
        assert_eq!(new, "-- -- select 1\n-- select 2");
    }

    #[test]
    fn blank_lines_are_left_alone() {
        let text = "select 1\n\nselect 2";
        let (new, _) = toggle(text, 0..text.len());
        assert_eq!(new, "-- select 1\n\n-- select 2");
    }

    #[test]
    fn a_blank_caret_line_is_a_no_op() {
        assert_eq!(toggle_comments("   \nselect 1", 1..1), None);
    }

    #[test]
    fn the_selection_survives_the_comments_coming_off() {
        let text = "-- select 1\n-- select 2";
        let (new, selection) = toggle(text, 0..text.len());
        assert_eq!(new, "select 1\nselect 2");
        assert_eq!(selection, 0..17);
    }

    #[test]
    fn delete_lines_removes_the_caret_line_and_its_newline() {
        let (new, caret) = delete_lines("select 1\nselect 2\nselect 3", 3..3).unwrap();
        assert_eq!(new, "select 2\nselect 3");
        assert_eq!(caret, 0..0);
    }

    #[test]
    fn delete_lines_takes_the_newline_before_the_last_line() {
        let (new, caret) = delete_lines("select 1\nselect 2", 12..12).unwrap();
        assert_eq!(new, "select 1");
        assert_eq!(caret, 8..8);
    }

    #[test]
    fn delete_lines_removes_every_line_the_selection_touches() {
        let (new, caret) = delete_lines("a\nb\nc", 0..4).unwrap();
        assert_eq!(new, "c");
        assert_eq!(caret, 0..0);
    }

    #[test]
    fn delete_lines_stops_at_a_line_start() {
        // The selection covers "a\n" and ends where "b" starts: only "a" goes.
        let (new, _) = delete_lines("a\nb\nc", 0..2).unwrap();
        assert_eq!(new, "b\nc");
    }

    #[test]
    fn delete_lines_on_an_empty_buffer_is_a_no_op() {
        assert_eq!(delete_lines("", 0..0), None);
    }

    #[test]
    fn delete_range_removes_the_selection() {
        let (new, caret) = delete_range("select 1", 2..8).unwrap();
        assert_eq!(new, "se");
        assert_eq!(caret, 2..2);
    }

    #[test]
    fn delete_range_with_a_bare_caret_is_a_no_op() {
        assert_eq!(delete_range("select 1", 3..3), None);
    }

    #[test]
    fn move_lines_up_swaps_with_the_line_above() {
        let (new, selection) = move_lines("a\nb\nc", 2..2, true).unwrap();
        assert_eq!(new, "b\na\nc");
        assert_eq!(selection, 0..0, "the caret rides along with its line");
    }

    #[test]
    fn move_lines_down_swaps_with_the_line_below() {
        let (new, selection) = move_lines("a\nb\nc", 2..2, false).unwrap();
        assert_eq!(new, "a\nc\nb");
        assert_eq!(selection, 4..4);
    }

    #[test]
    fn move_lines_moves_a_whole_selection() {
        let (new, selection) = move_lines("a\nb\nc\nd", 2..5, true).unwrap();
        assert_eq!(new, "b\nc\na\nd");
        assert_eq!(selection, 0..3);
    }

    #[test]
    fn move_lines_at_the_edges_is_a_no_op() {
        assert_eq!(move_lines("a\nb", 0..0, true), None);
        assert_eq!(move_lines("a\nb", 2..2, false), None);
    }

    #[test]
    fn move_lines_down_past_a_trailing_newline() {
        let (new, selection) = move_lines("a\nb\n", 2..2, false).unwrap();
        assert_eq!(new, "a\n\nb");
        assert_eq!(selection, 3..3);
    }

    #[test]
    fn duplicate_lines_down_copies_below_and_moves_the_caret_onto_it() {
        let (new, selection) = duplicate_lines("a\nb\nc", 2..2, false).unwrap();
        assert_eq!(new, "a\nb\nb\nc");
        assert_eq!(selection, 4..4);
    }

    #[test]
    fn duplicate_lines_up_copies_above_and_moves_the_caret_onto_it() {
        let (new, selection) = duplicate_lines("a\nb\nc", 2..2, true).unwrap();
        assert_eq!(new, "a\nb\nb\nc");
        assert_eq!(selection, 2..2);
    }

    #[test]
    fn duplicate_lines_down_at_the_end_grows_the_buffer() {
        let (new, selection) = duplicate_lines("a\nb", 2..2, false).unwrap();
        assert_eq!(new, "a\nb\nb");
        assert_eq!(selection, 4..4);
    }

    #[test]
    fn duplicate_lines_copies_a_whole_selection() {
        let (new, selection) = duplicate_lines("a\nb\nc", 0..3, false).unwrap();
        assert_eq!(new, "a\nb\na\nb\nc");
        assert_eq!(selection, 4..7);
    }

    #[test]
    fn line_clip_text_carries_the_newline() {
        assert_eq!(line_clip_text("a\nb\nc", 2), "b\n");
        assert_eq!(line_clip_text("a\nb", 2), "b");
        assert_eq!(line_clip_text("a\nb\nc", 0), "a\n");
    }

    #[test]
    fn select_line_range_takes_the_line_and_its_newline() {
        assert_eq!(select_line_range("a\nb\nc", 2..2), 2..4);
        assert_eq!(select_line_range("a\nb", 2..2), 2..3);
    }

    #[test]
    fn select_line_range_expands_a_partial_selection() {
        assert_eq!(select_line_range("a\nb", 0..1), 0..2);
    }

    #[test]
    fn select_line_range_pressed_twice_reaches_the_next_line() {
        assert_eq!(select_line_range("a\nb\nc", 2..4), 2..5);
        assert_eq!(select_line_range("a\nb\nc\n", 2..4), 2..6);
    }

    #[test]
    fn smart_home_toggles_between_indent_and_line_start() {
        assert_eq!(smart_home_offset("    select 1", 6), 4);
        assert_eq!(smart_home_offset("    select 1", 4), 0);
        assert_eq!(smart_home_offset("    select 1", 0), 4);
        assert_eq!(smart_home_offset("    select 1", 2), 4);
    }

    #[test]
    fn smart_home_on_a_blank_line_stays_put() {
        assert_eq!(smart_home_offset("select 1", 3), 0);
        assert_eq!(smart_home_offset("select 1", 0), 0);
    }

    #[test]
    fn smart_home_counts_tabs_as_indentation() {
        assert_eq!(smart_home_offset("\t\tselect", 5), 2);
        assert_eq!(smart_home_offset("\t\tselect", 2), 0);
    }

    #[test]
    fn word_at_finds_the_word_touching_the_caret() {
        assert_eq!(word_at("foo bar", 1), Some(0..3));
        assert_eq!(word_at("foo bar", 0), Some(0..3));
        // At a word boundary the word to the left is taken, the way ⌘D does.
        assert_eq!(word_at("foo bar", 3), Some(0..3));
        assert_eq!(word_at("foo  bar", 4), None);
        assert_eq!(word_at("foo_bar", 4), Some(0..7));
    }

    #[test]
    fn next_occurrence_selects_the_word_first_then_advances() {
        assert_eq!(next_occurrence("foo bar foo", 1..1), Some(8..11));
        assert_eq!(next_occurrence("foo bar foo", 0..3), Some(8..11));
    }

    #[test]
    fn next_occurrence_wraps_around() {
        assert_eq!(next_occurrence("foo bar foo", 8..11), Some(0..3));
    }

    #[test]
    fn next_occurrence_with_a_lone_match_is_a_no_op() {
        assert_eq!(next_occurrence("foo", 0..3), None);
        assert_eq!(next_occurrence("foo", 1..1), None);
        assert_eq!(next_occurrence("a + b", 2..2), None);
    }

    #[test]
    fn occurrence_matching_is_case_sensitive() {
        assert_eq!(next_occurrence("Foo foo", 0..3), None);
    }

    #[test]
    fn previous_occurrence_goes_backwards_and_wraps() {
        assert_eq!(previous_occurrence("foo bar foo", 8..11), Some(0..3));
        assert_eq!(previous_occurrence("foo bar foo", 0..3), Some(8..11));
    }

    #[test]
    fn previous_occurrence_with_a_lone_match_is_a_no_op() {
        assert_eq!(previous_occurrence("foo", 0..3), None);
        assert_eq!(previous_occurrence("foo bar", 5..5), None);
    }
}
