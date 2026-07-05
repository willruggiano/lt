//! The generated sort vocabulary shared by the DB query layer, the TUI, and
//! the sync thread.

use anyhow::{Result, anyhow};

// SortField (label/from_key/next) -- generated from [[sort_field]] entries in
// build/search_filter_fields.toml by build.rs (bd-2w5).
include!(concat!(env!("OUT_DIR"), "/sort_field.rs"));

// build_sort(&SortField, desc) -> serde_json::Value -- generated (bd-2w5).
include!(concat!(env!("OUT_DIR"), "/sort_build.rs"));

impl std::str::FromStr for SortField {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::from_key(s).ok_or_else(|| format!("invalid sort field: {s}"))
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
