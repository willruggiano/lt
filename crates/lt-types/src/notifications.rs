//! The notifications query, modelled as cynic `QueryFragment`s. These are the
//! shared "currency" types; the fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::scalars::DateTime;
use crate::schema;

#[derive(cynic::QueryVariables)]
pub struct NotificationsVariables {
    pub first: Option<i32>,
    pub after: Option<String>,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Query", variables = "NotificationsVariables")]
pub struct NotificationsQuery {
    #[arguments(first: $first, after: $after)]
    pub notifications: NotificationConnection,
}

/// The built notifications query string. Kept here so cynic stays confined to
/// `lt-types` (same contract as `viewer::query`).
#[must_use]
pub fn query() -> String {
    NotificationsQuery::build(NotificationsVariables {
        first: None,
        after: None,
    })
    .query
}

#[derive(cynic::QueryFragment)]
pub struct NotificationConnection {
    pub nodes: Vec<Notification>,
    pub page_info: PageInfo,
}

#[derive(cynic::QueryFragment)]
pub struct PageInfo {
    pub has_next_page: bool,
    pub end_cursor: Option<String>,
}

/// One notification node: issue notifications carry their issue; every other
/// concrete type decodes through the interface-level fallback.
#[derive(cynic::InlineFragments)]
#[cynic(graphql_type = "Notification")]
pub enum Notification {
    IssueNotification(IssueNotification),
    #[cynic(fallback)]
    Other(BaseNotification),
}

#[derive(cynic::QueryFragment)]
pub struct IssueNotification {
    pub id: cynic::Id,
    #[cynic(rename = "type")]
    pub type_: String,
    pub read_at: Option<DateTime>,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    pub actor: Option<NotificationActor>,
    pub issue: NotificationIssue,
}

/// The interface-level selection shared by every notification type.
#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Notification")]
pub struct BaseNotification {
    pub id: cynic::Id,
    #[cynic(rename = "type")]
    pub type_: String,
    pub read_at: Option<DateTime>,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    pub actor: Option<NotificationActor>,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "User")]
pub struct NotificationActor {
    pub name: String,
}

#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Issue")]
pub struct NotificationIssue {
    pub identifier: String,
    pub title: String,
}

#[cfg(test)]
mod tests {
    use super::query;

    #[test]
    fn query_selects_issue_notification_inline_fragment() {
        let built = query();
        assert!(built.contains("__typename"));
        assert!(built.contains("... on IssueNotification"));
    }
}
