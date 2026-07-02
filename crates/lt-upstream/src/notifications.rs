//! Fetch notifications from the Linear API. The `Notification` fragment type
//! (and its accessors) live in `lt-types`; this module is just the paginated
//! fetch.

use anyhow::{Result, anyhow};
pub use lt_types::notifications::Notification;
use lt_types::notifications::{NotificationsQuery, NotificationsVariables};

use super::client::{GraphqlTransport, HttpTransport, execute};

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

        let variables = NotificationsVariables {
            first: Some(i32::try_from(fetch_count).unwrap_or(250)),
            after: cursor,
        };

        let page = execute::<NotificationsQuery>(transport, variables)?;
        all.extend(page.nodes);

        // Stop if we have reached the total cap.
        if let Some(max) = max_total
            && all.len() >= max
        {
            all.truncate(max);
            break;
        }

        if !page.info.has_next_page {
            break;
        }
        cursor = page.info.end_cursor;
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
    use serde_json::json;

    use super::*;
    use crate::client::FakeTransport;
    use crate::issues::sample_issue_node;

    fn node(id: &str) -> serde_json::Value {
        json!({
            "__typename": "ProjectNotification",
            "id": id,
            "category": "assignments",
            "readAt": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
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
            got.iter().map(|n| n.id().inner()).collect::<Vec<_>>(),
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

    #[test]
    fn issue_notification_maps_issue_and_actor() {
        let node = json!({
            "__typename": "IssueNotification",
            "id": "n1",
            "category": "assignments",
            "readAt": null,
            "createdAt": "2026-01-01T00:00:00Z",
            "updatedAt": "2026-01-01T00:00:00Z",
            "actor": { "id": "u1", "name": "Ada Lovelace" },
            "issue": sample_issue_node("1")
        });
        let transport = FakeTransport::new(vec![json!({ "notifications": {
            "nodes": [node],
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        }})]);
        let got = fetch(&transport, 250, None).unwrap();
        assert_eq!(got.len(), 1);
        let n = &got[0];
        let issue = n.issue().unwrap();
        assert_eq!(issue.identifier, "ENG-1");
        assert_eq!(issue.title, "t");
        assert_eq!(n.actor().unwrap().name, "Ada Lovelace");
    }
}
