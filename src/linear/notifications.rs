use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use super::client::graphql_query;
use super::types::PageInfo;

const NOTIFICATIONS_QUERY: &str = r#"
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
"#;

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
pub fn fetch_notifications(
    token: &str,
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

        let data: NotificationsData = graphql_query(token, NOTIFICATIONS_QUERY, variables)?;

        let conn = data.notifications;
        all.extend(conn.nodes);

        // Stop if we have reached the total cap.
        if let Some(max) = max_total {
            if all.len() >= max {
                all.truncate(max);
                break;
            }
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

pub fn fetch_notifications_from_config(
    page_size: usize,
    max_total: Option<usize>,
) -> Result<Vec<Notification>> {
    let token = crate::config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;
    fetch_notifications(&token.access_token, page_size, max_total)
}
