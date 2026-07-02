//! The notifications query, modelled as cynic `QueryFragment`s. These are the
//! shared "currency" types; the fetch lives in `lt-upstream`.

use cynic::QueryBuilder;

use crate::pagination::PageInfo;
use crate::scalars::DateTime;
use crate::schema;
use crate::types::{Issue, User};

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

/// One notification node: issue notifications carry their issue (the shared,
/// fully-selected [`Issue`] fragment -- this is what fragment reuse across
/// operations is for); every other concrete type decodes through the
/// interface-level fallback.
#[derive(cynic::InlineFragments)]
#[cynic(graphql_type = "Notification")]
pub enum Notification {
    // Boxed: `IssueNotification` embeds the full `Issue` fragment, making this
    // variant far larger than `Other` (clippy::large_enum_variant).
    IssueNotification(Box<IssueNotification>),
    #[cynic(fallback)]
    Other(BaseNotification),
}

impl Notification {
    pub fn id(&self) -> &cynic::Id {
        match self {
            Self::IssueNotification(n) => &n.id,
            Self::Other(n) => &n.id,
        }
    }

    pub fn category(&self) -> &NotificationCategory {
        match self {
            Self::IssueNotification(n) => &n.category,
            Self::Other(n) => &n.category,
        }
    }

    pub fn read_at(&self) -> Option<&DateTime> {
        match self {
            Self::IssueNotification(n) => n.read_at.as_ref(),
            Self::Other(n) => n.read_at.as_ref(),
        }
    }

    pub fn created_at(&self) -> &DateTime {
        match self {
            Self::IssueNotification(n) => &n.created_at,
            Self::Other(n) => &n.created_at,
        }
    }

    pub fn updated_at(&self) -> &DateTime {
        match self {
            Self::IssueNotification(n) => &n.updated_at,
            Self::Other(n) => &n.updated_at,
        }
    }

    pub fn actor(&self) -> Option<&User> {
        match self {
            Self::IssueNotification(n) => n.actor.as_ref(),
            Self::Other(n) => n.actor.as_ref(),
        }
    }

    pub fn issue(&self) -> Option<&Issue> {
        match self {
            Self::IssueNotification(n) => Some(&n.issue),
            Self::Other(_) => None,
        }
    }
}

#[derive(cynic::QueryFragment)]
pub struct IssueNotification {
    pub id: cynic::Id,
    pub category: NotificationCategory,
    pub read_at: Option<DateTime>,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    pub actor: Option<User>,
    pub issue: Issue,
}

/// The interface-level selection shared by every notification type.
#[derive(cynic::QueryFragment)]
#[cynic(graphql_type = "Notification")]
pub struct BaseNotification {
    pub id: cynic::Id,
    pub category: NotificationCategory,
    pub read_at: Option<DateTime>,
    pub created_at: DateTime,
    pub updated_at: DateTime,
    pub actor: Option<User>,
}

/// Linear's stable, public classification of a notification (`title` and
/// `subtitle` are `[Internal]`, so `category` is the only presentable
/// discriminator). `Other` is cynic's decode fallback: any category the
/// schema adds after this build still decodes instead of failing.
#[derive(cynic::Enum, Clone, Debug, PartialEq, Eq)]
#[cynic(graphql_type = "NotificationCategory", rename_all = "camelCase")]
pub enum NotificationCategory {
    AppsAndIntegrations,
    Assignments,
    Billing,
    CommentsAndReplies,
    Customers,
    DocumentChanges,
    Feed,
    Mentions,
    PostsAndUpdates,
    Reactions,
    Reminders,
    Reviews,
    StatusChanges,
    Subscriptions,
    System,
    Triage,
    #[cynic(fallback)]
    Other(String),
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
