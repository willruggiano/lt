//! The new-issue modal's composed query: the team list plus, when a team is
//! selected, that team's workflow states and members -- one document whose
//! team-scoped selection is conditionally included on the wire
//! (docs/design/operation-seam-adr.md, "Decision 3").
//!
//! cynic 3.13 supports per-field `@include`/`@skip` directives
//! (`#[directives(include(if: $var))]`; verified against
//! `cynic-codegen-3.13.2/src/fragment_derive/directives/{mod,output}.rs` and
//! its `spread_attr`/`flatten_attr` codegen snapshot tests). A GraphQL
//! variable passed to a non-null argument slot must itself be non-null
//! (there is no `default = ...` attribute on `cynic::QueryVariables` to give
//! it an unused default), so `team_id` stays a plain (always-provided,
//! empty-when-unused) `String`; `has_team` is the client-only gate.

use cynic::QueryBuilder;
use linear_schema::linear as schema;

use crate::graphql::GraphqlOperation;
use crate::members::UserConnection;
use crate::states::WorkflowStateConnection;
use crate::teams::TeamConnection;
use crate::types::{Team, User, WorkflowState};
use crate::viewer;

#[derive(cynic::QueryVariables, Clone)]
pub struct NewIssueVariables {
    #[cynic(rename = "hasTeam")]
    pub has_team: bool,
    #[cynic(rename = "teamId")]
    pub team_id: String,
}

impl NewIssueVariables {
    #[must_use]
    pub fn new(team_id: Option<String>) -> Self {
        Self {
            has_team: team_id.is_some(),
            team_id: team_id.unwrap_or_default(),
        }
    }
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query", variables = "NewIssueVariables")]
pub struct NewIssueQuery {
    pub teams: TeamConnection,
    #[arguments(id: $team_id)]
    #[directives(include(if: $has_team))]
    pub team: Option<TeamWithStatesAndMembers>,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Team")]
pub struct TeamWithStatesAndMembers {
    pub states: WorkflowStateConnection,
    pub members: UserConnection,
}

/// The new-issue modal's whole data contract. `viewer` is sourced from the
/// cache only (`lt-storage`'s `Read` impl reads the persisted viewer); the
/// composed wire document above does not re-fetch it, since viewer
/// persistence stays `ViewerQuery`'s sole concern -- this operation's own
/// refresh would otherwise have to clobber or ignore the organization
/// fields only `ViewerQuery` selects.
#[derive(Default)]
pub struct NewIssueData {
    pub teams: Vec<Team>,
    pub states: Vec<WorkflowState>,
    pub members: Vec<User>,
    pub viewer: Option<viewer::Viewer>,
}

impl GraphqlOperation for NewIssueQuery {
    type Variables = NewIssueVariables;
    type Output = NewIssueData;
    const NAME: &'static str = "newIssue";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }
}

impl TryFrom<NewIssueQuery> for NewIssueData {
    type Error = anyhow::Error;

    fn try_from(op: NewIssueQuery) -> anyhow::Result<Self> {
        let (states, members) = op.team.map_or_else(
            || (Vec::new(), Vec::new()),
            |t| (t.states.nodes, t.members.nodes),
        );
        Ok(NewIssueData {
            teams: op.teams.nodes,
            states,
            members,
            viewer: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_declares_the_include_directive_and_team_id_variable() {
        let built = NewIssueQuery::operation(NewIssueVariables::new(None)).query;
        assert!(built.contains("$hasTeam: Boolean!"));
        assert!(built.contains("$teamId: String!"));
        assert!(built.contains("@include(if: $hasTeam)"));
    }

    #[test]
    fn new_variables_sets_has_team_from_the_option() {
        let with_team = NewIssueVariables::new(Some("t1".to_string()));
        assert!(with_team.has_team);
        assert_eq!(with_team.team_id, "t1");

        let without_team = NewIssueVariables::new(None);
        assert!(!without_team.has_team);
        assert_eq!(without_team.team_id, "");
    }

    #[test]
    fn recompose_with_team_returns_states_and_members() {
        let data = serde_json::json!({
            "teams": { "nodes": [{ "id": "t1", "name": "Eng" }] },
            "team": {
                "states": { "nodes": [{ "id": "s1", "name": "Todo", "position": 1.0 }] },
                "members": { "nodes": [{ "id": "u1", "name": "Ada" }] }
            }
        });
        let out: NewIssueData = serde_json::from_value::<NewIssueQuery>(data)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(out.teams.len(), 1);
        assert_eq!(out.states.len(), 1);
        assert_eq!(out.members.len(), 1);
        assert!(out.viewer.is_none());
    }

    #[test]
    fn recompose_without_team_leaves_states_and_members_empty() {
        let data = serde_json::json!({
            "teams": { "nodes": [{ "id": "t1", "name": "Eng" }] }
        });
        let out: NewIssueData = serde_json::from_value::<NewIssueQuery>(data)
            .unwrap()
            .try_into()
            .unwrap();
        assert_eq!(out.teams.len(), 1);
        assert!(out.states.is_empty());
        assert!(out.members.is_empty());
    }
}
