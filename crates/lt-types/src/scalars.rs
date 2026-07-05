//! Custom GraphQL scalars, modelled as newtypes so cynic can decode them
//! directly (see the ADR: `docs/design/linear-api-types-codegen.md`).

use crate::schema;

/// An ISO-8601 timestamp as returned by the Linear API, decoded straight into
/// `chrono` (rather than kept as a raw `String`) so every consumer gets a
/// real, comparable/formattable timestamp instead of re-parsing text.
#[derive(cynic::Scalar, Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime(pub chrono::DateTime<chrono::Utc>);

impl std::str::FromStr for DateTime {
    type Err = chrono::ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse::<chrono::DateTime<chrono::Utc>>().map(Self)
    }
}

impl DateTime {
    /// Render back to the millisecond-precision RFC3339 text form used for
    /// storage: the single format every text column and test fixture parses
    /// against, so ordering by that column matches chronological ordering.
    #[must_use]
    pub fn to_rfc3339_millis(self) -> String {
        self.0.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    /// Format as a relative age string like '5m ago', '2h ago', '3d ago'.
    /// `now` is the reference wall-clock time; callers pass the real value,
    /// tests a fixed one.
    #[must_use]
    pub fn relative_age(&self, now: chrono::DateTime<chrono::Utc>) -> String {
        let diff = (now - self.0).num_seconds().max(0);
        if diff < 60 {
            format!("{diff}s ago")
        } else if diff < 3600 {
            format!("{}m ago", diff / 60)
        } else if diff < 86400 {
            format!("{}h ago", diff / 3600)
        } else {
            format!("{}d ago", diff / 86400)
        }
    }

    /// Render as its `YYYY-MM-DD` date part.
    #[must_use]
    pub fn date(&self) -> String {
        self.0.format("%Y-%m-%d").to_string()
    }
}

/// An issue's `priority: Float!`, decoded straight into `u8` (Linear's
/// priority levels are always small non-negative integers: 0-4). `u8` cannot
/// implement cynic's `IsScalar` directly (orphan rule -- `f64`'s marker type
/// lives in the generated schema module), so this newtype closes the gap.
/// `#[serde(transparent)]` keeps the wire encoding identical to a bare `u8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct Priority(pub u8);

impl cynic::schema::IsScalar<f64> for Priority {
    type SchemaType = f64;
}

impl std::str::FromStr for Priority {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "none" | "0" => Ok(Self(0)),
            "urgent" | "1" => Ok(Self(1)),
            "high" | "2" => Ok(Self(2)),
            "normal" | "medium" | "3" => Ok(Self(3)),
            "low" | "4" => Ok(Self(4)),
            _ => Err(anyhow::anyhow!(
                "expected none/urgent/high/normal/medium/low or 0-4, got {s:?}"
            )),
        }
    }
}

/// The Linear API's `priorityLabel` string for each level, indexed by `.0`
/// (out-of-range levels cannot occur: `priorityLabel` is the wire source of
/// truth this scalar is decoded alongside).
const LABELS: [&str; 5] = ["No priority", "Urgent", "High", "Medium", "Low"];

impl Priority {
    /// The `priorityLabel` string this level matches on the wire, used to
    /// filter the local `priority_label` column by an equivalent level.
    #[must_use]
    pub fn label(self) -> &'static str {
        LABELS
            .get(usize::from(self.0))
            .copied()
            .unwrap_or(LABELS[0])
    }

    /// Parse a `priorityLabel` string back to its level. Lossy: any
    /// unrecognised label (including "No priority") collapses to 0, so this
    /// is a parse, not a `From`.
    #[must_use]
    pub fn from_label(label: &str) -> Self {
        match label.to_lowercase().as_str() {
            "urgent" => Self(1),
            "high" => Self(2),
            "medium" => Self(3),
            "low" => Self(4),
            _ => Self(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_from_str_accepts_names_and_numbers() {
        assert_eq!("none".parse::<Priority>().unwrap().0, 0);
        assert_eq!("0".parse::<Priority>().unwrap().0, 0);
        assert_eq!("urgent".parse::<Priority>().unwrap().0, 1);
        assert_eq!("1".parse::<Priority>().unwrap().0, 1);
        assert_eq!("high".parse::<Priority>().unwrap().0, 2);
        assert_eq!("2".parse::<Priority>().unwrap().0, 2);
        assert_eq!("normal".parse::<Priority>().unwrap().0, 3);
        assert_eq!("medium".parse::<Priority>().unwrap().0, 3);
        assert_eq!("3".parse::<Priority>().unwrap().0, 3);
        assert_eq!("low".parse::<Priority>().unwrap().0, 4);
        assert_eq!("4".parse::<Priority>().unwrap().0, 4);
        // Case-insensitive.
        assert_eq!("URGENT".parse::<Priority>().unwrap().0, 1);
    }

    #[test]
    fn priority_label_round_trips_through_from_label() {
        for level in 0..=4u8 {
            assert_eq!(Priority::from_label(Priority(level).label()).0, level);
        }
    }

    #[test]
    fn priority_from_label_is_lossy_for_unrecognised_labels() {
        assert_eq!(Priority::from_label("No priority").0, 0);
        assert_eq!(Priority::from_label("bogus").0, 0);
    }

    #[test]
    fn priority_from_str_rejects_unknown() {
        let err = "bogus".parse::<Priority>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected none/urgent/high/normal/medium/low or 0-4, got \"bogus\""
        );
    }

    #[test]
    fn relative_age_formatting() {
        // Fixed "now" so the age is deterministic.
        // 2020-01-01T00:00:00Z is 0, 2020-01-02T00:00:00Z is one day later.
        let now = "2020-01-02T01:00:00Z".parse::<DateTime>().unwrap().0;
        assert_eq!(
            "2020-01-01T00:00:00Z"
                .parse::<DateTime>()
                .unwrap()
                .relative_age(now),
            "1d ago"
        );
        assert_eq!(
            "2020-01-02T00:00:00Z"
                .parse::<DateTime>()
                .unwrap()
                .relative_age(now),
            "1h ago"
        );
        assert_eq!(
            "2020-01-02T00:59:30Z"
                .parse::<DateTime>()
                .unwrap()
                .relative_age(now),
            "30s ago"
        );
    }

    #[test]
    fn date_formats_as_year_month_day() {
        let dt = "2026-01-09T23:00:00Z".parse::<DateTime>().unwrap();
        assert_eq!(dt.date(), "2026-01-09");
    }
}
