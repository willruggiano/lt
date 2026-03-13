use super::list::Issue;

const MAX_TITLE: usize = 40;

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

fn date(s: &str) -> &str {
    if s.len() >= 10 { &s[..10] } else { s }
}

pub fn print_table(issues: &[Issue]) {
    if issues.is_empty() {
        println!("No issues found.");
        return;
    }

    // Build display rows: (identifier, title, state, priority, assignee, team, created, updated)
    let rows: Vec<[String; 8]> = issues
        .iter()
        .map(|i| {
            [
                i.identifier.clone(),
                truncate(&i.title, MAX_TITLE),
                i.state.name.clone(),
                i.priority_label.clone(),
                i.assignee
                    .as_ref()
                    .map(|u| u.name.clone())
                    .unwrap_or_else(|| "-".to_string()),
                i.team.name.clone(),
                date(&i.created_at).to_string(),
                date(&i.updated_at).to_string(),
            ]
        })
        .collect();

    let headers = [
        "IDENTIFIER",
        "TITLE",
        "STATE",
        "PRIORITY",
        "ASSIGNEE",
        "TEAM",
        "CREATED",
        "UPDATED",
    ];

    // Compute column widths: max of header and all row values.
    let mut widths = [0usize; 8];
    for (i, h) in headers.iter().enumerate() {
        widths[i] = h.len();
    }
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            if cell.len() > widths[i] {
                widths[i] = cell.len();
            }
        }
    }

    let print_row = |cells: &[&str; 8]| {
        let parts: Vec<String> = cells
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{:<width$}", c, width = widths[i]))
            .collect();
        println!("{}", parts.join("  "));
    };

    print_row(&headers);

    // Separator
    let sep: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    let sep_refs: Vec<&str> = sep.iter().map(|s| s.as_str()).collect();
    let sep_arr: [&str; 8] = sep_refs.try_into().unwrap();
    print_row(&sep_arr);

    for row in &rows {
        let refs: [&str; 8] = [
            &row[0], &row[1], &row[2], &row[3],
            &row[4], &row[5], &row[6], &row[7],
        ];
        print_row(&refs);
    }
}
