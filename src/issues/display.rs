use std::io::Write;

use anyhow::Result;

use super::list::Issue;
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
