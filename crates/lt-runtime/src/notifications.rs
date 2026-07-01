//! Notification command entry point. The API fetch and `Notification` types
//! live in `lt-upstream`; the runtime re-exports them so `lt-cli` renders the
//! inbox without naming `lt-upstream`.

pub use lt_upstream::notifications::{
    Notification, NotificationActor, NotificationIssue, NotificationIssueState,
    NotificationIssueTeam, fetch_from_config,
};
