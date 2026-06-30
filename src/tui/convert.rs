use super::Issue;

/// Convert a `crate::db::Comment` row to the API comment type shown in the
/// detail pane.
pub(crate) fn db_comment_to_api(c: crate::db::Comment) -> crate::linear::types::Comment {
    crate::linear::types::Comment {
        body: c.body,
        created_at: c.created_at,
        user: c
            .author_name
            .map(|name| crate::linear::types::CommentUser { name }),
    }
}

/// Convert a `crate::db::Issue` row to a `crate::issues::list::Issue` for TUI display.
pub(crate) fn db_issue_to_list_issue(src: crate::db::Issue) -> Issue {
    Issue {
        id: src.id,
        identifier: src.identifier,
        title: src.title,
        priority_label: src.priority_label.clone(),
        priority: priority_label_to_u8(&src.priority_label),
        state: crate::issues::list::State {
            id: String::new(),
            name: src.state_name,
        },
        assignee: src.assignee_name.map(|n| crate::issues::list::User {
            id: String::new(),
            name: n,
        }),
        team: crate::issues::list::Team {
            id: src.team_key.unwrap_or_default(),
            name: src.team_name,
        },
        created_at: src.created_at,
        updated_at: src.updated_at,
        description: src.description,
        labels: crate::issues::list::LabelConnection {
            nodes: src
                .labels
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|n| crate::issues::list::LabelNode {
                    name: n.to_string(),
                })
                .collect(),
        },
        project: src.project_name.map(|n| crate::issues::list::Project {
            id: String::new(),
            name: n,
        }),
        cycle: src.cycle_name.map(|n| crate::issues::list::Cycle {
            id: String::new(),
            name: Some(n),
        }),
        creator: src.creator_name.map(|n| crate::issues::list::User {
            id: String::new(),
            name: n,
        }),
        parent: src.parent_id.map(|id| crate::issues::list::Parent {
            id,
            identifier: src.parent_identifier.unwrap_or_default(),
        }),
    }
}

pub(crate) fn priority_label_to_u8(label: &str) -> u8 {
    match label.to_lowercase().as_str() {
        "urgent" => 1,
        "high" => 2,
        "normal" | "medium" => 3,
        "low" => 4,
        _ => 0,
    }
}
