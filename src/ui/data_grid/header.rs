//! What a column header says about its column beyond the name: a key mark
//! for the primary key, a link mark for a foreign key, and a tooltip with the
//! type, nullability, and default.
//!
//! A query result knows only the driver's type; a table tab also reads the
//! table's own columns and keys and hands them over as [`ColumnInfo`]. The
//! words are built here, without a window, so the header, its accessible
//! name, and the row panel's field labels say the same thing.

use std::collections::HashMap;

/// What the table view knows about one column from the table's schema.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnInfo {
    /// Part of the primary key.
    pub primary_key: bool,
    /// The `table.column` a single-column foreign key points at.
    pub references: Option<String>,
    /// `None` when the schema read did not say.
    pub nullable: Option<bool>,
    /// The default expression, as the server reports it.
    pub default: Option<String>,
    /// The declared type (`character varying(255)`), which says more than
    /// the driver's (`VARCHAR`). The driver's is used when this is empty.
    pub declared_type: String,
}

/// Column name to what is known of it, for a table tab's grid.
pub type ColumnInfos = HashMap<String, ColumnInfo>;

/// One header's words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderText {
    /// The accessible name: the column name and its keys, in words, so the
    /// marks are not something only a sighted user gets.
    pub label: String,
    /// One line per fact: type, nullability, default, keys.
    pub tooltip: String,
}

/// The words for column `name`, whose driver type is `type_name`.
pub fn header_text(name: &str, type_name: &str, info: Option<&ColumnInfo>) -> HeaderText {
    let mut label = name.to_string();
    let type_shown = info
        .map(|info| info.declared_type.trim())
        .filter(|declared| !declared.is_empty())
        .unwrap_or(type_name.trim());
    let mut lines = Vec::new();
    match type_shown {
        "" => lines.push(name.to_string()),
        shown => lines.push(format!("{name}: {shown}")),
    }

    if let Some(info) = info {
        if info.primary_key {
            label.push_str(", primary key");
            lines.push("Primary key".to_string());
        }
        if let Some(target) = &info.references {
            label.push_str(&format!(", foreign key to {target}"));
            lines.push(format!("References {target}"));
        }
        match info.nullable {
            Some(false) => lines.push("NOT NULL".to_string()),
            Some(true) => lines.push("Nullable".to_string()),
            None => {}
        }
        if let Some(default) = info.default.as_deref().filter(|d| !d.trim().is_empty()) {
            lines.push(format!("Default: {default}"));
        }
    }

    HeaderText {
        label,
        tooltip: lines.join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_result_header_names_its_type() {
        let text = header_text("total", "NUMERIC", None);
        assert_eq!(text.label, "total");
        assert_eq!(text.tooltip, "total: NUMERIC");
    }

    #[test]
    fn a_table_header_says_its_keys_nullability_and_default() {
        let id = ColumnInfo {
            primary_key: true,
            nullable: Some(false),
            default: Some("nextval('races_id_seq'::regclass)".into()),
            declared_type: "integer".into(),
            ..ColumnInfo::default()
        };
        let text = header_text("id", "INT4", Some(&id));
        assert_eq!(text.label, "id, primary key");
        assert_eq!(
            text.tooltip,
            "id: integer\nPrimary key\nNOT NULL\nDefault: nextval('races_id_seq'::regclass)"
        );

        let race = ColumnInfo {
            references: Some("races.id".into()),
            nullable: Some(true),
            ..ColumnInfo::default()
        };
        let text = header_text("race_id", "INT4", Some(&race));
        assert_eq!(text.label, "race_id, foreign key to races.id");
        assert_eq!(text.tooltip, "race_id: INT4\nReferences races.id\nNullable");
    }
}
