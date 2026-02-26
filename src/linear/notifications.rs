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

pub fn fetch_notifications(token: &str, first: usize) -> Result<Vec<Notification>> {
    let first = first.min(250);
    let mut all: Vec<Notification> = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let variables = json!({
            "first": first,
            "after": cursor,
        });

        let data: NotificationsData =
            graphql_query(token, NOTIFICATIONS_QUERY, variables)?;

        let conn = data.notifications;
        all.extend(conn.nodes);

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

pub fn fetch_notifications_from_config(first: usize) -> Result<Vec<Notification>> {
    let token = crate::config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;
    fetch_notifications(&token.access_token, first)
}
