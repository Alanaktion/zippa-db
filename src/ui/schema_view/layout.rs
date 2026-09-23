//! Column widths shared by the structure tab's three section tables, so a
//! heading and every row under it line up.

use gpui_kit::prelude::*;
use gpui_kit::{Pixels, px};

/// How a structure table sizes a column. The same value is given to the
/// heading row and to every body row, so the two stay in line.
#[derive(Clone, Copy)]
pub(super) enum ColWidth {
    /// A fixed width, for short, predictable fields such as a key marker or a
    /// flag.
    Fixed(f32),
    /// Grows to fill the space left over, with a floor so the text stays
    /// readable when the pane is narrow.
    Flex(f32),
}

impl ColWidth {
    /// The narrowest the column will be: its width when fixed, its floor when
    /// it grows.
    pub(super) const fn floor(self) -> f32 {
        match self {
            ColWidth::Fixed(width) | ColWidth::Flex(width) => width,
        }
    }
}

/// Size a heading or body cell as its column's [`ColWidth`].
pub(super) fn sized<E: Styled>(cell: E, width: ColWidth) -> E {
    match width {
        ColWidth::Fixed(width) => cell.flex_none().w(px(width)).min_w(px(width)),
        ColWidth::Flex(min) => cell.flex_1().min_w(px(min)),
    }
}

/// The narrowest a table can be and still show all of `columns`, counting the
/// small-cell padding on both sides of each. The page scrolls sideways past
/// this instead of letting the table clip a column away.
pub(super) fn table_min_width(columns: &[ColWidth]) -> Pixels {
    px(columns.iter().map(|column| column.floor() + 8.).sum())
}

/// Columns table: which column holds the primary key, then the fields the user
/// can change, then the row's own action.
pub(super) const COL_KEY: ColWidth = ColWidth::Fixed(48.);
pub(super) const COL_NAME: ColWidth = ColWidth::Flex(140.);
pub(super) const COL_TYPE: ColWidth = ColWidth::Fixed(150.);
pub(super) const COL_NULLABLE: ColWidth = ColWidth::Fixed(80.);
pub(super) const COL_DEFAULT: ColWidth = ColWidth::Flex(120.);

/// Trailing action cell, shared by every section.
pub(super) const COL_ACTIONS: ColWidth = ColWidth::Fixed(110.);

/// Indexes table.
pub(super) const COL_INDEX_NAME: ColWidth = ColWidth::Flex(150.);
pub(super) const COL_INDEX_KIND: ColWidth = ColWidth::Fixed(160.);
pub(super) const COL_INDEX_COLUMNS: ColWidth = ColWidth::Flex(180.);

/// Foreign keys table.
pub(super) const COL_FK_NAME: ColWidth = ColWidth::Flex(130.);
pub(super) const COL_FK_REFERENCE: ColWidth = ColWidth::Flex(220.);
pub(super) const COL_FK_ACTION: ColWidth = ColWidth::Fixed(110.);
