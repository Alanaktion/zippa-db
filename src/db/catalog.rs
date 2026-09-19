//! A snapshot of everything in the database, and the search over it.
//!
//! The sidebar filter and the quick switcher only know table, view and routine
//! *names*, so "where is `email` stored?" cannot be answered without opening
//! tables. This module holds the snapshot that answers it — one entry per
//! table, view, column, index, routine and trigger — and the pure ranking
//! function the search dialog drives. Nothing here touches a database or a
//! window, so the matching is testable on its own.
//!
//! A [`Catalog`] is built once per session by [`Connection::catalog`], so a
//! keystroke never costs a round trip.
//!
//! [`Connection::catalog`]: super::Connection::catalog

use regex::{Regex, RegexBuilder};

use super::connection::{DatabaseObject, ObjectKind, StoredKind, StoredObject};

/// The most entries a catalog keeps. A database past this is truncated rather
/// than held whole; the session reports the number the server had.
pub const MAX_ENTRIES: usize = 200_000;

/// The most results one search hands back.
///
/// The list virtualizes, so the cap is about the work of ranking and building
/// rows on every keystroke, not about what can be scrolled through.
pub const MAX_HITS: usize = 500;

/// What one entry describes. Tables, views and routines are objects a tab can
/// open; columns, indexes and triggers belong to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CatalogKind {
    Table,
    View,
    Column,
    Index,
    Routine,
    Trigger,
}

impl CatalogKind {
    /// Every kind, in the order groups are listed.
    pub const ALL: [CatalogKind; 6] = [
        CatalogKind::Table,
        CatalogKind::View,
        CatalogKind::Column,
        CatalogKind::Index,
        CatalogKind::Routine,
        CatalogKind::Trigger,
    ];

    /// The word for the row: a screen reader and a reader who does not
    /// recognise the icon both need it.
    pub fn label(self) -> &'static str {
        match self {
            CatalogKind::Table => "Table",
            CatalogKind::View => "View",
            CatalogKind::Column => "Column",
            CatalogKind::Index => "Index",
            CatalogKind::Routine => "Routine",
            CatalogKind::Trigger => "Trigger",
        }
    }

    /// The word the `kind:` prefix accepts, and the chip's own id.
    pub fn keyword(self) -> &'static str {
        match self {
            CatalogKind::Table => "table",
            CatalogKind::View => "view",
            CatalogKind::Column => "column",
            CatalogKind::Index => "index",
            CatalogKind::Routine => "routine",
            CatalogKind::Trigger => "trigger",
        }
    }

    /// The kind a `kind:` prefix names, accepting the plural and the common
    /// abbreviations.
    fn named(word: &str) -> Option<Self> {
        match word.to_ascii_lowercase().as_str() {
            "table" | "tables" => Some(CatalogKind::Table),
            "view" | "views" => Some(CatalogKind::View),
            "column" | "columns" | "col" | "cols" => Some(CatalogKind::Column),
            "index" | "indexes" | "indices" => Some(CatalogKind::Index),
            "routine" | "routines" | "function" | "functions" | "procedure" | "procedures" => {
                Some(CatalogKind::Routine)
            }
            "trigger" | "triggers" => Some(CatalogKind::Trigger),
            _ => None,
        }
    }

    /// Groups the rows are listed under: the two openable object kinds share a
    /// heading, the way the sidebar lists them together.
    pub fn group(self) -> &'static str {
        match self {
            CatalogKind::Table | CatalogKind::View => "Tables & Views",
            CatalogKind::Column => "Columns",
            CatalogKind::Index => "Indexes",
            CatalogKind::Routine => "Routines",
            CatalogKind::Trigger => "Triggers",
        }
    }
}

/// One thing the catalog knows about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogEntry {
    pub kind: CatalogKind,
    /// The table or view the entry belongs to, or the table or view entry
    /// itself. `None` for a routine, which belongs to no table.
    pub object: Option<DatabaseObject>,
    /// The routine's own identity, when `kind` is [`CatalogKind::Routine`].
    pub routine: Option<StoredObject>,
    /// The entry's own name: a column, index, routine or trigger name, or a
    /// table's or view's.
    pub name: String,
    /// The muted second line: a column's type, an index's columns, a routine's
    /// arguments, a trigger's event.
    pub detail: String,
}

impl CatalogEntry {
    /// A table or view, which owns itself.
    pub(crate) fn object(object: DatabaseObject) -> Self {
        let kind = match object.kind {
            ObjectKind::Table => CatalogKind::Table,
            ObjectKind::View => CatalogKind::View,
        };
        Self {
            kind,
            name: object.name.clone(),
            object: Some(object),
            routine: None,
            detail: String::new(),
        }
    }

    /// A function, procedure or sequence.
    pub(crate) fn routine(routine: StoredObject) -> Self {
        let detail = routine.arguments.clone().unwrap_or_default();
        Self {
            kind: CatalogKind::Routine,
            name: routine.name.clone(),
            object: None,
            routine: Some(routine),
            detail,
        }
    }

    /// A column, index or trigger belonging to `object`.
    pub(crate) fn member(
        kind: CatalogKind,
        object: DatabaseObject,
        name: String,
        detail: String,
    ) -> Self {
        Self {
            kind,
            object: Some(object),
            routine: None,
            name,
            detail,
        }
    }

    /// The object a member belongs to, for the muted owner line. `None` for a
    /// table or view, whose own name is the row, and for a routine, which no
    /// table owns.
    pub fn owner_label(&self) -> Option<String> {
        match (self.kind, &self.object, &self.routine) {
            (CatalogKind::Table | CatalogKind::View, _, _) | (_, None, None) => None,
            (_, Some(object), _) => Some(object.label()),
            (_, None, Some(routine)) => routine.schema.clone(),
        }
    }

    /// The name as shown: a routine keeps its argument list, which tells
    /// overloads apart.
    pub fn label(&self) -> String {
        match &self.routine {
            Some(routine) => routine.label(),
            None => self.name.clone(),
        }
    }

    /// The kind word shown beside the name, which for a routine says which of
    /// the three it is rather than the generic "Routine".
    pub fn kind_label(&self) -> &'static str {
        match &self.routine {
            Some(routine) => match routine.kind {
                StoredKind::Function => "Function",
                StoredKind::Procedure => "Procedure",
                StoredKind::Sequence => "Sequence",
            },
            None => self.kind.label(),
        }
    }

    /// The table or view a member belongs to, which is what a column, index or
    /// trigger result opens.
    pub fn owner(&self) -> Option<&DatabaseObject> {
        self.object.as_ref()
    }

    /// The name a `table:` filter matches: the owning table for a member, or
    /// the table or view itself.
    fn owner_or_self(&self) -> String {
        self.owner_label().unwrap_or_else(|| self.label())
    }
}

/// The catalog as loaded, with the total the server reported so a capped one
/// can say so.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub entries: Vec<CatalogEntry>,
    /// The number of entries before the cap. Equal to `entries.len()` unless
    /// the database was over [`MAX_ENTRIES`].
    pub total: usize,
}

impl Catalog {
    /// The number hidden by the cap, when there is one.
    pub fn truncated(&self) -> Option<usize> {
        (self.total > self.entries.len()).then_some(self.total)
    }
}

/// A parsed search box: free text plus the `kind:` / `table:` / `type:`
/// prefixes.
///
/// The grammar is deliberately tiny and is spelled out in the dialog's
/// placeholder. Anything with an unknown prefix is ordinary text, so a query
/// like `http://x` is not swallowed by a prefix it does not have.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// The words that are not a recognised prefix, joined by a space.
    pub text: String,
    /// `kind:` filters, which are OR'd together when there is more than one.
    pub kinds: Vec<CatalogKind>,
    /// `table:` — matches the owning object's name.
    pub table: Option<String>,
    /// `type:` — matches the detail line, which carries a column's type.
    pub type_name: Option<String>,
}

impl Query {
    /// Split a search box's text into free text and prefixes.
    ///
    /// A prefix naming nothing (`kind:bogus`) stays in the free text, so it is
    /// searched for rather than silently dropping the results.
    pub fn parse(raw: &str) -> Self {
        let mut query = Query::default();
        let mut words: Vec<&str> = Vec::new();

        for token in raw.split_whitespace() {
            let mut consumed = false;
            if let Some((prefix, value)) = token.split_once(':')
                && !value.is_empty()
            {
                match prefix.to_ascii_lowercase().as_str() {
                    "kind" => {
                        if let Some(kind) = CatalogKind::named(value) {
                            query.kinds.push(kind);
                            consumed = true;
                        }
                    }
                    "table" => {
                        query.table = Some(value.to_string());
                        consumed = true;
                    }
                    "type" => {
                        query.type_name = Some(value.to_string());
                        consumed = true;
                    }
                    _ => {}
                }
            }
            if !consumed {
                words.push(token);
            }
        }

        query.text = words.join(" ");
        query
    }

    /// Whether the box holds nothing at all.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
            && self.kinds.is_empty()
            && self.table.is_none()
            && self.type_name.is_none()
    }
}

/// One result: an index into the entries searched, and how well it scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    pub index: usize,
    pub score: u32,
}

/// Rank `entries` against `query`, best first.
///
/// Scoring, highest first: an exact name, a name prefix, a match at a word
/// boundary, a name substring, and last a match in the owner or detail line.
/// Ties break towards tables and views, then by owning object and name, so the
/// order is stable however the entries arrived.
pub fn search(entries: &[CatalogEntry], query: &Query) -> Vec<Hit> {
    let needle = Needle::new(&query.text);
    let table = query.table.as_deref().map(Needle::new);
    let type_name = query.type_name.as_deref().map(Needle::new);

    let mut hits = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if !query.kinds.is_empty() && !query.kinds.contains(&entry.kind) {
            continue;
        }
        if let Some(table) = &table
            && !table.is_any()
            && !table.matches(&entry.owner_or_self())
        {
            continue;
        }
        if let Some(type_name) = &type_name
            && !type_name.is_any()
            && !type_name.matches(&entry.detail)
        {
            continue;
        }

        let Some(score) = score(entry, &needle) else {
            continue;
        };
        hits.push(Hit { index, score });
    }

    hits.sort_by(|a, b| {
        let left = &entries[a.index];
        let right = &entries[b.index];
        b.score
            .cmp(&a.score)
            .then_with(|| rank(left.kind).cmp(&rank(right.kind)))
            .then_with(|| left.owner_sort().cmp(&right.owner_sort()))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| a.index.cmp(&b.index))
    });
    hits.truncate(MAX_HITS);
    hits
}

/// How well one entry answers the free text, or `None` when it does not.
fn score(entry: &CatalogEntry, needle: &Needle) -> Option<u32> {
    if needle.is_any() {
        return Some(1);
    }

    let name = needle.name_score(&entry.name);
    if name > 0 {
        return Some(name * 2);
    }

    let owner = entry.owner_label().unwrap_or_default();
    (needle.matches(&owner) || needle.matches(&entry.detail)).then_some(1)
}

/// Sort order for kinds on equal scores: what a tab can open first, then what
/// describes a table.
fn rank(kind: CatalogKind) -> u8 {
    match kind {
        CatalogKind::Table => 0,
        CatalogKind::View => 1,
        CatalogKind::Routine => 2,
        CatalogKind::Column => 3,
        CatalogKind::Index => 4,
        CatalogKind::Trigger => 5,
    }
}

impl CatalogEntry {
    /// The owning object's name for a stable tie-break, schema-qualified.
    fn owner_sort(&self) -> String {
        self.owner_label().unwrap_or_default().to_lowercase()
    }
}

/// The text being searched for, compiled once per search.
///
/// The same convention as the sidebar filter: a case-insensitive regex, with a
/// pattern that will not parse falling back to matching the text literally —
/// while it is still being typed, `user(` is more likely half a word than a
/// broken pattern. An empty pattern matches everything.
enum Needle {
    Any,
    Pattern(Regex),
}

impl Needle {
    fn new(text: &str) -> Self {
        if text.is_empty() {
            return Needle::Any;
        }

        let case_insensitive = |pattern: &str| {
            RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .ok()
        };

        match case_insensitive(text).or_else(|| case_insensitive(&regex::escape(text))) {
            Some(pattern) => Needle::Pattern(pattern),
            // The escaped form always parses, so this is unreachable in
            // practice; treating it as "match nothing" keeps the function
            // total.
            None => Needle::Any,
        }
    }

    fn is_any(&self) -> bool {
        matches!(self, Needle::Any)
    }

    fn matches(&self, haystack: &str) -> bool {
        match self {
            Needle::Any => true,
            Needle::Pattern(pattern) => pattern.is_match(haystack),
        }
    }

    /// The name score: exact, prefix, word boundary, or substring.
    fn name_score(&self, name: &str) -> u32 {
        let Needle::Pattern(pattern) = self else {
            return 0;
        };
        let Some(found) = pattern.find(name) else {
            return 0;
        };

        if found.start() == 0 && found.end() == name.len() {
            4
        } else if found.start() == 0 {
            3
        } else if starts_word(name, found.start()) {
            2
        } else {
            1
        }
    }
}

/// Whether the match at `index` begins a word: at the start, or after
/// something that is not part of an identifier.
fn starts_word(text: &str, index: usize) -> bool {
    text[..index]
        .chars()
        .next_back()
        .is_none_or(|character| !character.is_alphanumeric() && character != '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(name: &str) -> CatalogEntry {
        CatalogEntry::object(DatabaseObject {
            schema: None,
            name: name.into(),
            kind: ObjectKind::Table,
        })
    }

    fn column(owner: &str, name: &str, type_name: &str) -> CatalogEntry {
        CatalogEntry::member(
            CatalogKind::Column,
            DatabaseObject {
                schema: None,
                name: owner.into(),
                kind: ObjectKind::Table,
            },
            name.into(),
            type_name.into(),
        )
    }

    fn index(owner: &str, name: &str, columns: &str) -> CatalogEntry {
        CatalogEntry::member(
            CatalogKind::Index,
            DatabaseObject {
                schema: None,
                name: owner.into(),
                kind: ObjectKind::Table,
            },
            name.into(),
            columns.into(),
        )
    }

    fn fixture() -> Vec<CatalogEntry> {
        vec![
            table("users"),
            table("orders"),
            column("users", "id", "uuid"),
            column("users", "email", "text"),
            column("orders", "user_id", "uuid"),
            index("users", "users_email_idx", "email"),
        ]
    }

    fn labels(entries: &[CatalogEntry], query: &str) -> Vec<String> {
        search(entries, &Query::parse(query))
            .into_iter()
            .map(|hit| entries[hit.index].label())
            .collect()
    }

    #[test]
    fn exact_names_outrank_prefixes_which_outrank_substrings() {
        let entries = vec![table("user"), table("users"), table("superuser")];
        assert_eq!(labels(&entries, "user"), ["user", "users", "superuser"]);
    }

    #[test]
    fn a_table_outranks_a_column_on_the_same_score() {
        let entries = vec![column("users", "email", "text"), table("email")];
        assert_eq!(labels(&entries, "email"), ["email", "email"]);
        let hits = search(&entries, &Query::parse("email"));
        // The table is listed first even though both are exact matches.
        assert_eq!(entries[hits[0].index].kind, CatalogKind::Table);
    }

    #[test]
    fn a_name_match_outranks_a_detail_match() {
        let entries = vec![
            column("orders", "total", "numeric"),
            column("orders", "amount", "total"),
        ];
        let hits = search(&entries, &Query::parse("total"));
        assert_eq!(entries[hits[0].index].name, "total");
    }

    #[test]
    fn a_member_is_found_by_its_owning_table() {
        let entries = fixture();
        assert!(labels(&entries, "email").contains(&"email".to_string()));
        // `type:` reaches the detail line, where a column's type lives.
        assert_eq!(labels(&entries, "type:uuid"), ["user_id", "id"]);
        // `table:` narrows to one owner, and keeps the table's own row.
        assert_eq!(
            labels(&entries, "table:users"),
            ["users", "email", "id", "users_email_idx"]
        );
    }

    #[test]
    fn kind_prefixes_are_recognized_in_both_numbers() {
        let entries = fixture();
        assert_eq!(labels(&entries, "kind:index"), ["users_email_idx"]);
        assert_eq!(labels(&entries, "kind:columns"), ["user_id", "email", "id"]);
        assert_eq!(labels(&entries, "kind:table"), ["orders", "users"]);
    }

    #[test]
    fn an_unknown_prefix_is_searched_for_as_text() {
        let entries = vec![table("http://example.com")];
        assert_eq!(
            labels(&entries, "http://example.com"),
            ["http://example.com"]
        );
        assert!(labels(&entries, "kind:bogus").is_empty());
    }

    #[test]
    fn the_text_is_a_case_insensitive_regex_with_a_literal_fallback() {
        let entries = vec![table("Orders"), table("order_items")];
        assert_eq!(labels(&entries, "^order"), ["Orders", "order_items"]);
        // A pattern still being typed falls back to the literal text.
        assert!(labels(&entries, "order(").is_empty());
        assert_eq!(labels(&entries, "Order_Items"), ["order_items"]);
    }

    #[test]
    fn an_empty_query_keeps_everything() {
        let entries = fixture();
        assert_eq!(search(&entries, &Query::parse("")).len(), entries.len());
    }

    #[test]
    fn queries_parse_prefixes_out_of_the_free_text() {
        let query = Query::parse("kind:column type:uuid email");
        assert_eq!(query.kinds, [CatalogKind::Column]);
        assert_eq!(query.type_name.as_deref(), Some("uuid"));
        assert_eq!(query.text, "email");
        assert!(!query.is_empty());
        assert!(Query::parse("   ").is_empty());
    }

    #[test]
    fn the_catalog_caps_what_it_keeps_and_says_how_many_there_were() {
        let entries: Vec<CatalogEntry> = (0..MAX_HITS + 10)
            .map(|index| table(&format!("t{index}")))
            .collect();
        let hits = search(&entries, &Query::parse(""));
        assert_eq!(hits.len(), MAX_HITS);

        let catalog = Catalog {
            entries: entries.clone(),
            total: entries.len(),
        };
        assert_eq!(catalog.truncated(), None);

        let capped = Catalog {
            entries: entries[..MAX_HITS].to_vec(),
            total: MAX_ENTRIES + 1,
        };
        assert_eq!(capped.truncated(), Some(MAX_ENTRIES + 1));
    }
}
