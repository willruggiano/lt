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
    fn priority_from_str_rejects_unknown() {
        let err = "bogus".parse::<Priority>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "expected none/urgent/high/normal/medium/low or 0-4, got \"bogus\""
        );
    }
}
