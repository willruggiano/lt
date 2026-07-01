#![allow(dead_code)]

use anyhow::{Result, anyhow};
use lt_types::types::PageInfo;
use serde::Deserialize;
use serde_json::json;

use super::client::{GraphqlTransport, HttpTransport, query_as};

const NOTIFICATIONS_QUERY: &str = r"
query Notifications($first: Int, $after: String) {
  notifications(first: $first, after: $after) {
    nodes {
      id
      type
      readAt
      createdAt
      updatedAt
      ... on IssueNotification {
        issue { identifier title state { name } priority team { name } }
        actor { name }
      }
    }
    pageInfo { hasNextPage endCursor }
  }
}
";

#[derive(Deserialize, Debug, Clone)]
pub struct NotificationIssueState {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct NotificationIssueTeam {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct NotificationIssue {
    pub identifier: String,
    pub title: String,
    pub state: NotificationIssueState,
    pub priority: Option<i64>,
    pub team: NotificationIssueTeam,
}

#[derive(Deserialize, Debug, Clone)]
pub struct NotificationActor {
    pub name: String,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Notification {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(rename = "readAt")]
    pub read_at: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    pub issue: Option<NotificationIssue>,
    pub actor: Option<NotificationActor>,
}

#[derive(Deserialize)]
struct NotificationConnection {
    nodes: Vec<Notification>,
    #[serde(rename = "pageInfo")]
    page_info: PageInfo,
}

#[derive(Deserialize)]
struct NotificationsData {
    notifications: NotificationConnection,
}

/// Fetch notifications from the Linear API.
///
/// `page_size` is the number of items to request per GraphQL page (capped at 250).
/// `max_total` is the maximum number of items to return across all pages.
/// When `max_total` is `None` the function fetches every available page.
pub fn fetch(
    transport: &dyn GraphqlTransport,
    page_size: usize,
    max_total: Option<usize>,
) -> Result<Vec<Notification>> {
    let page_size = page_size.min(250);
    let mut all: Vec<Notification> = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        // Never request more items per page than we still need.
        let fetch_count = if let Some(max) = max_total {
            let remaining = max.saturating_sub(all.len());
            if remaining == 0 {
                break;
            }
            page_size.min(remaining)
        } else {
            page_size
        };

        let variables = json!({
            "first": fetch_count,
            "after": cursor,
        });

        let data: NotificationsData = query_as(transport, NOTIFICATIONS_QUERY, variables)?;

        let conn = data.notifications;
        all.extend(conn.nodes);

        // Stop if we have reached the total cap.
        if let Some(max) = max_total
            && all.len() >= max
        {
            all.truncate(max);
            break;
        }

        if !conn.page_info.has_next_page {
            break;
        }
        cursor = conn.page_info.end_cursor;
        if cursor.is_none() {
            break;
        }
    }

    Ok(all)
}

pub fn fetch_from_config(page_size: usize, max_total: Option<usize>) -> Result<Vec<Notification>> {
    let token = lt_config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;
    fetch(
        &HttpTransport::new(token.access_token),
        page_size,
        max_total,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::FakeTransport;

    fn node(id: &str) -> serde_json::Value {
        json!({
            "id": id,
            "type": "issueAssignedToYou",
            "readAt": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "issue": null,
            "actor": null
        })
    }

    fn page(nodes: &[&str], has_next: bool, end: Option<&str>) -> serde_json::Value {
        let nodes: Vec<_> = nodes.iter().map(|id| node(id)).collect();
        json!({ "notifications": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": has_next, "endCursor": end }
        }})
    }

    #[test]
    fn paginates_until_last_page() {
        let transport = FakeTransport::new(vec![
            page(&["n1"], true, Some("c1")),
            page(&["n2"], false, None),
        ]);
        let got = fetch(&transport, 250, None).unwrap();
        assert_eq!(
            got.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
            ["n1", "n2"]
        );
        // The second request carries the first page's end cursor.
        assert_eq!(transport.variables(1)["after"], json!("c1"));
    }

    #[test]
    fn max_total_truncates_and_stops_early() {
        let transport = FakeTransport::new(vec![page(&["n1", "n2", "n3"], true, Some("c1"))]);
        let got = fetch(&transport, 250, Some(2)).unwrap();
        assert_eq!(got.len(), 2);
        // The cap is reached on the first page, so no second request is made.
        assert_eq!(transport.calls.borrow().len(), 1);
    }

    #[test]
    fn page_size_is_capped_at_250() {
        let transport = FakeTransport::new(vec![page(&["n1"], false, None)]);
        fetch(&transport, 1000, None).unwrap();
        assert_eq!(transport.variables(0)["first"], json!(250));
    }
}
