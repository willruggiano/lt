//! The detail pane's composed query (ENG-27's data contract): one document
//! over `Query.issue(id:)` selecting the full [`Issue`] fragment alongside
//! its `comments` and `children` connections, so the pane is a single
//! operation rather than a client-side join
//! (docs/design/operation-seam-adr.md, "Decision 3").

use cynic::QueryBuilder;

use crate::comments::{Comment, CommentConnection};
use crate::graphql::GraphqlOperation;
use crate::issues::IssueConnection;
use crate::schema;
use crate::types::Issue;

#[derive(cynic::QueryVariables, Clone)]
pub struct IssueDetailVariables {
    pub id: String,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query", variables = "IssueDetailVariables")]
pub struct IssueDetailQuery {
    #[arguments(id: $id)]
    pub issue: IssueDetailFragment,
}

/// One issue's full fragment (spread) plus its comment thread and children,
/// all selected on the same `Issue` object.
#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Issue")]
pub struct IssueDetailFragment {
    #[cynic(spread)]
    pub base: Issue,
    #[arguments(first: 100)]
    pub comments: CommentConnection,
    /// Never fetched upstream before this operation existed (only
    /// reconstructed locally); a first-page fetch is an upgrade, not a
    /// regression -- `docs/design/operation-seam-adr.md` Task 4.
    #[arguments(first: 250)]
    pub children: IssueConnection,
}

/// The detail pane's whole data contract. `None` when the id is locally
/// absent (a stale cache after an upstream delete): the honest shape for a
/// pane opened from a listed issue whose row has since disappeared, rather
/// than panicking or fabricating an empty issue.
pub struct IssueDetailData {
    pub issue: Issue,
    pub comments: Vec<Comment>,
    pub children: Vec<Issue>,
}

impl GraphqlOperation for IssueDetailQuery {
    type Variables = IssueDetailVariables;
    type Output = Option<IssueDetailData>;
    const NAME: &'static str = "issueDetail";

    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables> {
        Self::build(variables)
    }

    fn extract(self) -> anyhow::Result<Self::Output> {
        Ok(Some(IssueDetailData {
            issue: self.issue.base,
            comments: self.issue.comments.nodes,
            children: self.issue.children.nodes,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issues::sample_issue_node;

    #[test]
    fn query_declares_expected_variable_and_connections() {
        let built = IssueDetailQuery::operation(IssueDetailVariables { id: String::new() }).query;
        assert!(built.contains("$id: String!"));
        assert!(built.contains("comments"));
        assert!(built.contains("children"));
    }

    #[test]
    fn extract_maps_issue_comments_and_children() {
        let data = serde_json::json!({
            "issue": {
                "id": "1", "identifier": "ENG-1", "title": "t",
                "priorityLabel": "High", "priority": 2,
                "state": { "id": "s", "name": "Todo", "position": 1.0 },
                "assignee": null,
                "team": { "id": "ENG", "name": "Engineering" },
                "description": null,
                "labels": { "nodes": [] },
                "project": null, "cycle": null, "creator": null, "parent": null,
                "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
                "comments": {
                    "nodes": [{
                        "id": "c1", "body": "hi",
                        "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-01T00:00:00Z",
                        "user": { "id": "u1", "name": "Ada" },
                        "issueId": "1"
                    }],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                },
                "children": {
                    "nodes": [sample_issue_node("2")],
                    "pageInfo": { "hasNextPage": false, "endCursor": null }
                }
            }
        });
        let out = serde_json::from_value::<IssueDetailQuery>(data)
            .unwrap()
            .extract()
            .unwrap()
            .unwrap();
        assert_eq!(out.issue.identifier, "ENG-1");
        assert_eq!(out.comments.len(), 1);
        assert_eq!(out.comments[0].body, "hi");
        assert_eq!(out.children.len(), 1);
        assert_eq!(out.children[0].identifier, "ENG-2");
    }
}
