//! The storage-side issue query spec and its generated sort vocabulary.
//!
//! `IssueQuery` is the clap-free filter/sort specification the DB query layer,
//! the TUI, and the sync thread all carry. `lt-cli` lowers its clap `IssueQuery`
//! into it.

use anyhow::{Result, anyhow};

// SortField (label/from_key/next) -- generated from [[sort_field]] entries in
// build/search_filter_fields.toml by build.rs (bd-2w5).
include!(concat!(env!("OUT_DIR"), "/sort_field.rs"));

// build_sort(&SortField, desc) -> serde_json::Value -- generated (bd-2w5).
include!(concat!(env!("OUT_DIR"), "/sort_build.rs"));

/// A filter/sort specification for the issues list.
#[derive(Clone, Debug)]
pub struct IssueQuery {
    pub team: Option<String>,
    pub assignee: Option<String>,
    pub no_assignee: bool,
    pub state: Option<String>,
    pub priority: Option<String>,
    pub created_after: Option<String>,
    pub created_before: Option<String>,
    pub updated_after: Option<String>,
    pub updated_before: Option<String>,
    pub sort: SortField,
    pub desc: bool,
    pub title: Option<String>,
    pub limit: u32,
}

impl Default for IssueQuery {
    fn default() -> Self {
        Self {
            team: None,
            assignee: None,
            no_assignee: false,
            state: None,
            priority: None,
            created_after: None,
            created_before: None,
            updated_after: None,
            updated_before: None,
            sort: SortField::Updated,
            desc: true,
            title: None,
            limit: 50,
        }
    }
}

/// Validate and normalise a `YYYY-MM-DD` date into an RFC3339 start-of-day
/// timestamp for SQL/GraphQL comparison.
pub fn parse_date(s: &str, field: &str) -> Result<String> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3
        || parts[0].len() != 4
        || parts[1].len() != 2
        || parts[2].len() != 2
        || !parts.iter().all(|p| p.chars().all(|c| c.is_ascii_digit()))
    {
        return Err(anyhow!("--{field}: date must be YYYY-MM-DD, got {s:?}"));
    }
    Ok(format!("{s}T00:00:00Z"))
}

#[cfg(test)]
mod tests {
    use super::parse_date;

    #[test]
    fn parse_date_accepts_iso_date() {
        assert_eq!(
            parse_date("2026-06-29", "created-after").unwrap(),
            "2026-06-29T00:00:00Z"
        );
    }

    #[test]
    fn parse_date_rejects_malformed() {
        assert!(parse_date("2026-06", "f").is_err());
        assert!(parse_date("26-6-9", "f").is_err());
        assert!(parse_date("2026-6-29", "f").is_err());
        assert!(parse_date("2026-0a-29", "f").is_err());
    }
}
