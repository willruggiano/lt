//! Typed GraphQL inputs via cynic `InputObject`.
//!
//! The mutation variables enqueued into the outbox are built from these structs
//! so the wire payload is schema-checked at compile time. `Field<T>` gives the
//! one nullable field that needs it (`assigneeId`) a three-valued encoding so
//! "clear the assignee" (`null`) is distinct from "leave it unchanged"
//! (omitted).

use serde::{Serialize, Serializer};

use super::schema;

/// Three-valued optional for a nullable input field: `Absent` omits the field,
/// `Null` sends `null` (clear), `Value` sends the value. A bare `Option<T>`
/// cannot express all three (cynic ships no three-valued optional), so this
/// newtype closes the gap, wired through `skip_serializing_if` for `Absent`.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Field<T> {
    #[default]
    Absent,
    Null,
    Value(T),
}

impl<T> Field<T> {
    /// Whether the field should be omitted from the serialized input.
    pub fn is_absent(&self) -> bool {
        matches!(self, Field::Absent)
    }
}

impl<T: Serialize> Serialize for Field<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // `Absent` is paired with skip_serializing_if and never reached, but
            // serialize it as null defensively rather than panic.
            Field::Absent | Field::Null => serializer.serialize_none(),
            Field::Value(v) => v.serialize(serializer),
        }
    }
}

/// Deserializes to `Value`/`Null` only -- an absent key never reaches this
/// impl. Pair with `#[serde(default)]` on the containing field so a missing
/// key falls back to `Field::Absent` (the `Default` impl above) instead: this
/// is what makes the outbox's stored `Field<T>` variables round-trip through
/// JSON without collapsing "unchanged" (absent) and "clear" (null) together.
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Field<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(v) => Field::Value(v),
            None => Field::Null,
        })
    }
}

// Lets `Field<T>` stand in for a nullable scalar in a cynic `InputObject`: the
// derive aligns the field to `Option<Field<T>>` and asserts
// `Option<Field<T>>: IsScalar<Option<Marker>>`, which reduces to
// `Field<T>: IsScalar<Marker>`.
impl<T, U> cynic::schema::IsScalar<U> for Field<T>
where
    T: cynic::schema::IsScalar<U>,
{
    type SchemaType = T::SchemaType;
}

/// Partial update for `issueUpdate`. Every field is optional; omitted fields are
/// left unchanged. `assignee_id` is three-valued so it can be cleared.
#[derive(cynic::InputObject, Debug, Clone, Default, serde::Deserialize, PartialEq)]
#[cynic(graphql_type = "IssueUpdateInput", rename_all = "camelCase")]
#[serde(rename_all = "camelCase", default)]
pub struct IssueUpdateInput {
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub state_id: Option<String>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[cynic(skip_serializing_if = "Field::is_absent")]
    pub assignee_id: Field<String>,
}

/// New-issue payload for `issueCreate`. `team_id` is required; the rest are
/// omitted when absent.
#[derive(cynic::InputObject, Debug, Clone, serde::Deserialize, PartialEq)]
#[cynic(graphql_type = "IssueCreateInput", rename_all = "camelCase")]
#[serde(rename_all = "camelCase")]
pub struct IssueCreateInput {
    pub title: String,
    pub team_id: String,
    #[cynic(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub description: Option<String>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub state_id: Option<String>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub priority: Option<i32>,
    #[cynic(skip_serializing_if = "Option::is_none")]
    #[serde(default)]
    pub assignee_id: Option<String>,
}

/// New-comment payload for `commentCreate`.
#[derive(cynic::InputObject, Debug, Clone, serde::Deserialize, PartialEq)]
#[cynic(graphql_type = "CommentCreateInput", rename_all = "camelCase")]
#[serde(rename_all = "camelCase")]
pub struct CommentCreateInput {
    pub issue_id: String,
    pub body: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn issue_update_input_three_valued_assignee() {
        let clear = IssueUpdateInput {
            assignee_id: Field::Null,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&clear).unwrap(),
            json!({ "assigneeId": null })
        );

        let set = IssueUpdateInput {
            state_id: Some("s1".to_string()),
            priority: Some(2),
            assignee_id: Field::Value("u1".to_string()),
        };
        assert_eq!(
            serde_json::to_value(&set).unwrap(),
            json!({ "stateId": "s1", "priority": 2, "assigneeId": "u1" })
        );

        let untouched = IssueUpdateInput::default();
        assert_eq!(serde_json::to_value(&untouched).unwrap(), json!({}));
    }

    #[test]
    fn issue_create_input_omits_absent_optionals() {
        let input = IssueCreateInput {
            title: "New".to_string(),
            team_id: "t1".to_string(),
            description: None,
            state_id: Some("s1".to_string()),
            priority: None,
            assignee_id: None,
        };
        assert_eq!(
            serde_json::to_value(&input).unwrap(),
            json!({ "title": "New", "teamId": "t1", "stateId": "s1" })
        );
    }

    #[test]
    fn comment_create_input_serializes_required_fields() {
        let input = CommentCreateInput {
            issue_id: "i1".to_string(),
            body: "hi".to_string(),
        };
        assert_eq!(
            serde_json::to_value(&input).unwrap(),
            json!({ "issueId": "i1", "body": "hi" })
        );
    }

    // The outbox stores mutation variables as JSON text and deserializes them
    // back into these typed inputs at replay time; these round-trips cover the
    // Field<T> three-valued states that motivated the manual Deserialize impl.

    #[test]
    fn issue_update_input_round_trips_absent_field() {
        let input = IssueUpdateInput::default();
        let round_tripped: IssueUpdateInput =
            serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap();
        assert_eq!(round_tripped, input);
        assert!(round_tripped.assignee_id.is_absent());
    }

    #[test]
    fn issue_update_input_round_trips_null_field() {
        let input = IssueUpdateInput {
            assignee_id: Field::Null,
            ..Default::default()
        };
        let round_tripped: IssueUpdateInput =
            serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap();
        assert_eq!(round_tripped, input);
        assert_eq!(round_tripped.assignee_id, Field::Null);
    }

    #[test]
    fn issue_update_input_round_trips_value_field() {
        let input = IssueUpdateInput {
            state_id: Some("s1".to_string()),
            priority: Some(2),
            assignee_id: Field::Value("u1".to_string()),
        };
        let round_tripped: IssueUpdateInput =
            serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap();
        assert_eq!(round_tripped, input);
    }

    #[test]
    fn issue_create_input_round_trips_with_absent_optionals() {
        let input = IssueCreateInput {
            title: "New".to_string(),
            team_id: "t1".to_string(),
            description: None,
            state_id: None,
            priority: None,
            assignee_id: None,
        };
        let round_tripped: IssueCreateInput =
            serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap();
        assert_eq!(round_tripped, input);
    }

    #[test]
    fn comment_create_input_round_trips() {
        let input = CommentCreateInput {
            issue_id: "i1".to_string(),
            body: "hi".to_string(),
        };
        let round_tripped: CommentCreateInput =
            serde_json::from_value(serde_json::to_value(&input).unwrap()).unwrap();
        assert_eq!(round_tripped, input);
    }
}
