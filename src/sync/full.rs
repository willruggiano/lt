use anyhow::Result;
use chrono::Utc;

use crate::db;
use crate::issues::list::fetch;
use crate::issues::{IssueArgs, SortField};

/// Convert a list::Issue into a db::Issue.
fn to_db_issue(src: &crate::issues::list::Issue) -> db::Issue {
    db::Issue {
        id: src.id.clone(),
        identifier: src.identifier.clone(),
        title: src.title.clone(),
        priority_label: src.priority_label.clone(),
        state_name: src.state.name.clone(),
        assignee_name: src.assignee.as_ref().map(|u| u.name.clone()),
        team_name: src.team.name.clone(),
        team_key: Some(src.team.id.clone()),
        created_at: src.created_at.clone(),
        updated_at: src.updated_at.clone(),
        synced_at: String::new(), // filled by upsert_issues
    }
}

/// Fetch every page from the Linear API and upsert into SQLite.
/// Sets sync_meta key='last_synced_at' to the current UTC timestamp on success.
pub fn run() -> Result<()> {
    let conn = db::open_db()?;

    // Use a default IssueArgs with no filters and max page size.
    let args = IssueArgs {
        limit: 250,
        sort: SortField::Updated,
        desc: true,
        team: None,
        assignee: None,
        no_assignee: false,
        state: None,
        priority: None,
        created_after: None,
        created_before: None,
        updated_after: None,
        updated_before: None,
        title: None,
        live: false,
    };

    let mut cursor: Option<String> = None;
    loop {
        let after = cursor.as_deref();
        let (issues, has_next, end_cursor) = fetch(&args, after)?;
        let count = issues.len();

        if count > 0 {
            let db_issues: Vec<db::Issue> = issues.iter().map(to_db_issue).collect();
            db::upsert_issues(&conn, &db_issues)?;
        }

        if !has_next {
            break;
        }
        cursor = end_cursor;
    }

    let now = Utc::now().to_rfc3339();
    db::set_meta(&conn, "last_synced_at", &now)?;

    Ok(())
}
