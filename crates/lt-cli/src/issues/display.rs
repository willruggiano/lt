use std::io::Write;

use anyhow::Result;
use lt_runtime::text;
use lt_types::scalars::DateTime;
use lt_types::types::Issue;

const MAX_TITLE: usize = 40;

/// Render a wire timestamp as its `YYYY-MM-DD` date part.
fn date(dt: &DateTime) -> String {
    dt.0.format("%Y-%m-%d").to_string()
}

/// Print one padded row, each cell left-aligned to its column width.
fn print_row(out: &mut dyn Write, cells: &[&str], widths: &[usize]) -> Result<()> {
    let parts: Vec<String> = cells
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
        .collect();
    writeln!(out, "{}", parts.join("  "))?;
    Ok(())
}

/// Print the header, separator, and data rows for an 8-column table.
fn print_table_rows(out: &mut dyn Write, headers: &[&str; 8], rows: &[[String; 8]]) -> Result<()> {
    let mut widths = [0usize; 8];
    for (i, h) in headers.iter().enumerate() {
        widths[i] = h.len();
    }
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    print_row(out, headers, &widths)?;

    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    let sep_refs: Vec<&str> = sep.iter().map(std::string::String::as_str).collect();
    print_row(out, &sep_refs, &widths)?;

    for row in rows {
        let refs: Vec<&str> = row.iter().map(String::as_str).collect();
        print_row(out, &refs, &widths)?;
    }

    Ok(())
}

const HEADERS: [&str; 8] = [
    "IDENTIFIER",
    "TITLE",
    "STATE",
    "PRIORITY",
    "ASSIGNEE",
    "TEAM",
    "CREATED",
    "UPDATED",
];

/// Print a table of issues, with an optional trailing `note` (e.g. cache age).
/// Pass `""` for no note.
pub fn print_table(out: &mut dyn Write, issues: &[Issue], note: &str) -> Result<()> {
    if issues.is_empty() {
        writeln!(out, "No issues found.")?;
        return Ok(());
    }

    // Build display rows: (identifier, title, state, priority, assignee, team, created, updated)
    let rows: Vec<[String; 8]> = issues
        .iter()
        .map(|i| {
            [
                i.identifier.clone(),
                text::truncate(&i.title, MAX_TITLE),
                i.state.name.clone(),
                i.priority_label.clone(),
                i.assignee
                    .as_ref()
                    .map_or_else(|| "-".to_string(), |u| u.name.clone()),
                i.team.name.clone(),
                date(&i.created_at),
                date(&i.updated_at),
            ]
        })
        .collect();

    print_table_rows(out, &HEADERS, &rows)?;

    if !note.is_empty() {
        writeln!(out, "\n{note}")?;
    }

    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use lt_types::types::{IssueLabel, IssueLabelConnection, Team, User, WorkflowState};

    use super::*;

    /// A minimal issue fragment for display tests: `id`/`identifier`/`title`
    /// vary, everything else is a fixed baseline. Shared with the inbox
    /// display tests, which render the same `Issue` fragment via
    /// `Notification::issue`.
    pub(crate) fn sample_issue(id: &str, identifier: &str, title: &str) -> Issue {
        Issue {
            id: id.into(),
            identifier: identifier.into(),
            title: title.into(),
            priority_label: "Urgent".into(),
            priority: lt_types::scalars::Priority(1),
            state: WorkflowState {
                id: "".into(),
                name: "In Progress".into(),
                position: None,
            },
            assignee: Some(User {
                id: "".into(),
                name: "Ada Lovelace".into(),
            }),
            team: Team {
                id: "ENG".into(),
                name: "Engineering".into(),
            },
            description: None,
            labels: IssueLabelConnection { nodes: Vec::new() },
            project: None,
            cycle: None,
            creator: None,
            parent: None,
            created_at: DateTime("2026-01-02T03:04:05Z".parse().unwrap()),
            updated_at: DateTime("2026-01-06T07:08:09Z".parse().unwrap()),
        }
    }

    fn cached_to_string(issues: &[Issue], note: &str) -> String {
        let mut buf = Vec::new();
        print_table(&mut buf, issues, note).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn live_to_string(issues: &[Issue]) -> String {
        let mut buf = Vec::new();
        print_table(&mut buf, issues, "").unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn empty_says_no_issues() {
        assert_eq!(cached_to_string(&[], "(cached)"), "No issues found.\n");
        assert_eq!(live_to_string(&[]), "No issues found.\n");
    }

    #[test]
    fn live_table_renders_columns() {
        // Two hand-built rows exercise the list::Issue -> row mapping, an
        // unassigned issue ("-"), and the long-title truncation at MAX_TITLE.
        let mut second = sample_issue(
            "2",
            "ENG-2",
            "A title that is definitely longer than forty characters wide",
        );
        second.priority_label = "No priority".into();
        second.priority = lt_types::scalars::Priority(0);
        second.state.name = "Backlog".into();
        second.assignee = None;
        second.labels = IssueLabelConnection {
            nodes: vec![IssueLabel {
                id: "l1".into(),
                name: "bug".into(),
            }],
        };
        second.created_at = DateTime("2026-01-01T00:00:00Z".parse().unwrap());
        second.updated_at = DateTime("2026-01-01T00:00:00Z".parse().unwrap());

        let issues = vec![sample_issue("1", "ENG-1", "Short title"), second];
        insta::assert_snapshot!(live_to_string(&issues));
    }

    #[cfg(feature = "sim")]
    #[test]
    fn cached_table_snapshot_from_sim() {
        let dataset = lt_runtime::sim::generate(0, 8);
        insta::assert_snapshot!(cached_to_string(&dataset.issues, "(cached)"));
    }
}
