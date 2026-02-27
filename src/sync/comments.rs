//! Sync comments for a single issue from the Linear API into the local DB.
//!
//! The Linear GraphQL API returns comment IDs alongside bodies when queried
//! through the issue detail endpoint.  This module re-uses the existing
//! `fetch_issue_detail` infrastructure to obtain comments and persists them
//! into the `issue_comments` table.

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

use crate::db;
use crate::linear::client::graphql_query;
use crate::linear::types::PageInfo;

const COMMENTS_QUERY: &str = r#"
query IssueComments($id: String!, $after: String) {
  issue(id: $id) {
    comments(first: 100, after: $after) {
      nodes {
        id
        body
        createdAt
        updatedAt
        user { name }
      }
      pageInfo { hasNextPage endCursor }
    }
  }
}
"#;

#[derive(Deserialize)]
struct CommentUser {
    name: String,
}

#[derive(Deserialize)]
struct ApiComment {
    id: String,
    body: String,
    #[serde(rename = "createdAt")]
    created_at: String,
    #[serde(rename = "updatedAt")]
    updated_at: String,
    user: Option<CommentUser>,
}

#[derive(Deserialize)]
struct CommentConnection {
    nodes: Vec<ApiComment>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Deserialize)]
struct IssueWithComments {
    comments: CommentConnection,
}

#[derive(Deserialize)]
struct IssueCommentsData {
    issue: Option<IssueWithComments>,
}

fn api_to_db(c: &ApiComment, issue_id: &str) -> db::Comment {
    db::Comment {
        id: c.id.clone(),
        issue_id: issue_id.to_string(),
        body: c.body.clone(),
        author_name: c.user.as_ref().map(|u| u.name.clone()),
        created_at: c.created_at.clone(),
        updated_at: c.updated_at.clone(),
        synced_at: String::new(), // filled by upsert_comments
    }
}

/// Fetch all comments for `issue_id` from the Linear API and upsert them into
/// the local `issue_comments` table.
///
/// All existing comments for the issue are replaced with the freshly fetched
/// set to keep the DB consistent with Linear.
pub fn sync_comments(conn: &rusqlite::Connection, token: &str, issue_id: &str) -> Result<()> {
    let mut all_comments: Vec<db::Comment> = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let variables = json!({
            "id": issue_id,
            "after": cursor,
        });

        let data: IssueCommentsData = graphql_query(token, COMMENTS_QUERY, variables)?;
        let issue = match data.issue {
            Some(i) => i,
            None => break, // issue not found; nothing to sync
        };

        let conn_data = issue.comments;
        for c in &conn_data.nodes {
            all_comments.push(api_to_db(c, issue_id));
        }

        if !conn_data.page_info.has_next_page {
            break;
        }
        cursor = conn_data.page_info.end_cursor;
    }

    // Replace the existing comments for this issue with the fresh set.
    db::delete_comments_for_issue(conn, issue_id)?;
    db::upsert_comments(conn, &all_comments)?;
    Ok(())
}
