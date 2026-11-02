//! Sizing a result's columns before it is drawn.
//!
//! The table needs a width per column up front, and the values are display
//! strings in a monospaced font, so a character count is enough to size one
//! without shaping any text.

use gpui_kit::{Pixels, px};

use crate::db::query::QueryResult;

/// Rows read when sizing a column. Values further down are rare enough that
/// paying for them on every result would cost more than the odd clipped cell.
const SAMPLE_ROWS: usize = 100;

/// Rough advance width of one character in the grid's font, which is monospaced
/// and rendered at `text_xs`. Measuring properly would mean shaping every
/// sampled value; this is within a few pixels and costs nothing.
const CHARACTER_WIDTH: f32 = 7.2;

/// Cell padding and borders that sit either side of the text.
const CELL_PADDING: f32 = 18.;

pub(super) const MIN_COLUMN_WIDTH: f32 = 56.;
const MAX_COLUMN_WIDTH: f32 = 420.;

/// Width of the placeholder shown for SQL `NULL`.
const NULL_WIDTH: usize = 4;

/// Size every column from its header and the first [`SAMPLE_ROWS`] values.
pub(super) fn measure_columns(result: &QueryResult) -> Vec<Pixels> {
    result
        .columns
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut characters = name.chars().count();

            for row in result.rows.iter().take(SAMPLE_ROWS) {
                let width = match row.get(index) {
                    Some(Some(value)) => value.chars().count(),
                    Some(None) => NULL_WIDTH,
                    None => 0,
                };
                characters = characters.max(width);
            }

            let width = characters as f32 * CHARACTER_WIDTH + CELL_PADDING;
            px(width.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH))
        })
        .collect()
}

/// Room one key or link mark takes in a header, gap included.
const MARK_WIDTH: f32 = 16.;

/// Widen any column whose header carries a key or link mark, so the marks do
/// not cut the name short. Only ever widens, so it can be applied again when
/// the schema read lands after the rows.
pub(super) fn widen_for_marks(
    widths: &mut [Pixels],
    columns: &[String],
    info: &super::ColumnInfos,
) {
    for (width, name) in widths.iter_mut().zip(columns) {
        let Some(info) = info.get(name) else {
            continue;
        };
        let marks = usize::from(info.primary_key) + usize::from(info.references.is_some());
        if marks == 0 {
            continue;
        }
        let header = name.chars().count() as f32 * CHARACTER_WIDTH
            + marks as f32 * MARK_WIDTH
            + CELL_PADDING;
        *width = (*width).max(px(header.clamp(MIN_COLUMN_WIDTH, MAX_COLUMN_WIDTH)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_marked_header_is_widened_for_its_marks() {
        let columns = vec!["race_id".to_string(), "runner".to_string()];
        let mut widths = vec![px(MIN_COLUMN_WIDTH), px(MIN_COLUMN_WIDTH)];
        let mut info = super::super::ColumnInfos::new();
        info.insert(
            "race_id".into(),
            super::super::ColumnInfo {
                references: Some("races.id".into()),
                ..Default::default()
            },
        );
        widen_for_marks(&mut widths, &columns, &info);
        let expected = 7. * CHARACTER_WIDTH + MARK_WIDTH + CELL_PADDING;
        assert_eq!(widths, [px(expected), px(MIN_COLUMN_WIDTH)]);
        // Applying it again changes nothing.
        widen_for_marks(&mut widths, &columns, &info);
        assert_eq!(widths, [px(expected), px(MIN_COLUMN_WIDTH)]);
    }

    fn result(columns: &[&str], rows: Vec<Vec<Option<&str>>>) -> QueryResult {
        QueryResult {
            columns: columns.iter().map(|name| name.to_string()).collect(),
            rows: rows
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|cell| cell.map(|value| value.to_string()))
                        .collect()
                })
                .collect(),
            ..QueryResult::default()
        }
    }

    #[test]
    fn narrow_columns_get_the_minimum_width() {
        let widths = measure_columns(&result(&["id"], vec![vec![Some("1")], vec![Some("2")]]));
        assert_eq!(widths, [px(MIN_COLUMN_WIDTH)]);
    }

    #[test]
    fn wide_values_are_capped() {
        let long = "x".repeat(500);
        let widths = measure_columns(&result(&["blob"], vec![vec![Some(long.as_str())]]));
        assert_eq!(widths, [px(MAX_COLUMN_WIDTH)]);
    }

    #[test]
    fn width_follows_the_longest_sampled_value() {
        let widths = measure_columns(&result(
            &["name"],
            vec![
                vec![Some("ada")],
                vec![Some("a rather longer value")],
                vec![None],
            ],
        ));

        let expected = "a rather longer value".len() as f32 * CHARACTER_WIDTH + CELL_PADDING;
        assert_eq!(widths, [px(expected)]);
    }

    #[test]
    fn the_header_widens_a_column_of_short_values() {
        let widths = measure_columns(&result(
            &["a_column_with_a_long_name"],
            vec![vec![Some("1")]],
        ));

        let expected = "a_column_with_a_long_name".len() as f32 * CHARACTER_WIDTH + CELL_PADDING;
        assert_eq!(widths, [px(expected)]);
    }

    #[test]
    fn only_the_first_rows_are_sampled() {
        let sampled = "a twenty char value.";
        let long = "x".repeat(300);
        let mut rows: Vec<Vec<Option<&str>>> = vec![vec![Some(sampled)]; SAMPLE_ROWS];
        rows.push(vec![Some(long.as_str())]);

        let widths = measure_columns(&result(&["value"], rows));
        assert_eq!(
            widths,
            [px(sampled.len() as f32 * CHARACTER_WIDTH + CELL_PADDING)],
            "a long value past the sample should not widen the column"
        );
    }
}
