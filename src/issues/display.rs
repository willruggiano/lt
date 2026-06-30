use std::io::Write;

use anyhow::Result;

use crate::linear::types::Issue;
use crate::{db, text};

const MAX_TITLE: usize = 40;

fn date(s: &str) -> &str {
    if s.len() >= 10 { &s[..10] } else { s }
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

/// Print a table of issues fetched from the local SQLite cache.
pub fn print_table_cached(out: &mut dyn Write, issues: &[db::Issue], note: &str) -> Result<()> {
    if issues.is_empty() {
        writeln!(out, "No issues found.")?;
        return Ok(());
    }

    let rows: Vec<[String; 8]> = issues
        .iter()
        .map(|i| {
            [
                i.identifier.clone(),
                text::truncate(&i.title, MAX_TITLE),
                i.state_name.clone(),
                i.priority_label.clone(),
                i.assignee_name.as_deref().unwrap_or("-").to_string(),
                i.team_name.clone(),
                date(&i.created_at).to_string(),
                date(&i.updated_at).to_string(),
            ]
        })
        .collect();

    print_table_rows(out, &HEADERS, &rows)?;

    if !note.is_empty() {
        writeln!(out, "\n{note}")?;
    }

    Ok(())
}

pub fn print_table(out: &mut dyn Write, issues: &[Issue]) -> Result<()> {
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
                date(&i.created_at).to_string(),
                date(&i.updated_at).to_string(),
            ]
        })
        .collect();

    print_table_rows(out, &HEADERS, &rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::types::{Label, LabelConnection, State, Team, User};

    fn cached_to_string(issues: &[db::Issue], note: &str) -> String {
        let mut buf = Vec::new();
        print_table_cached(&mut buf, issues, note).unwrap();
        String::from_utf8(buf).unwrap()
    }

    fn live_to_string(issues: &[Issue]) -> String {
        let mut buf = Vec::new();
        print_table(&mut buf, issues).unwrap();
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
        let issues = vec![
            Issue {
                id: "1".into(),
                identifier: "ENG-1".into(),
                title: "Short title".into(),
                priority_label: "Urgent".into(),
                priority: 1,
                state: State {
                    id: String::new(),
                    name: "In Progress".into(),
                },
                assignee: Some(User {
                    id: String::new(),
                    name: "Ada Lovelace".into(),
                }),
                team: Team {
                    id: "ENG".into(),
                    name: "Engineering".into(),
                },
                description: None,
                labels: LabelConnection { nodes: Vec::new() },
                project: None,
                cycle: None,
                creator: None,
                parent: None,
                created_at: "2026-01-02T03:04:05Z".into(),
                updated_at: "2026-01-06T07:08:09Z".into(),
            },
            Issue {
                id: "2".into(),
                identifier: "ENG-2".into(),
                title: "A title that is definitely longer than forty characters wide".into(),
                priority_label: "No priority".into(),
                priority: 0,
                state: State {
                    id: String::new(),
                    name: "Backlog".into(),
                },
                assignee: None,
                team: Team {
                    id: "ENG".into(),
                    name: "Engineering".into(),
                },
                description: None,
                labels: LabelConnection {
                    nodes: vec![Label {
                        id: "l1".into(),
                        name: "bug".into(),
                    }],
                },
                project: None,
                cycle: None,
                creator: None,
                parent: None,
                created_at: "2026-01-01T00:00:00Z".into(),
                updated_at: "2026-01-01T00:00:00Z".into(),
            },
        ];
        insta::assert_snapshot!(live_to_string(&issues));
    }

    #[cfg(feature = "sim")]
    #[test]
    fn cached_table_snapshot_from_sim() {
        let dataset = crate::sim::generate(0, 8);
        insta::assert_snapshot!(cached_to_string(&dataset.issues, "(cached)"));
    }
}
