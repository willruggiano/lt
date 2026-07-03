use std::io::{self, BufRead, Write};

use anyhow::{Result, anyhow};
use lt_runtime::issues::NewIssueSession;
use lt_types::inputs::IssueCreateInput;
use lt_types::types::{Team, User as Member, WorkflowState, priority_u8_to_label};
use lt_types::viewer::User as Viewer;

#[derive(Debug, Clone)]
pub struct NewIssueArgs {
    pub team: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub priority: Option<String>,
    pub state: Option<String>,
    pub assignee: Option<String>,
}

fn read_line(out: &mut dyn Write, prompt: &str) -> Result<String> {
    write!(out, "{prompt}")?;
    out.flush()?;
    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    Ok(line
        .trim_end_matches('\n')
        .trim_end_matches('\r')
        .to_string())
}

fn parse_priority(s: &str) -> Option<u8> {
    match s.trim().to_lowercase().as_str() {
        "none" | "0" => Some(0),
        "urgent" | "1" => Some(1),
        "high" | "2" => Some(2),
        "normal" | "medium" | "3" => Some(3),
        "low" | "4" => Some(4),
        _ => None,
    }
}

fn pick_team<'a>(out: &mut dyn Write, teams: &'a [Team], hint: Option<&str>) -> Result<&'a Team> {
    if let Some(h) = hint {
        let lower = h.to_lowercase();
        if let Some(t) = teams.iter().find(|t| t.name.to_lowercase() == lower) {
            return Ok(t);
        }
        if let Ok(n) = h.parse::<usize>()
            && n >= 1
            && n <= teams.len()
        {
            return Ok(&teams[n - 1]);
        }
        return Err(anyhow!("no team matching '{h}'"));
    }

    writeln!(out, "Teams:")?;
    for (i, t) in teams.iter().enumerate() {
        writeln!(out, "  {}. {}", i + 1, t.name)?;
    }

    loop {
        let input = read_line(out, "Team (number or name): ")?;
        if input.is_empty() {
            writeln!(out, "Team is required.")?;
            continue;
        }
        let lower = input.to_lowercase();
        if let Some(t) = teams.iter().find(|t| t.name.to_lowercase() == lower) {
            return Ok(t);
        }
        if let Ok(n) = input.parse::<usize>()
            && n >= 1
            && n <= teams.len()
        {
            return Ok(&teams[n - 1]);
        }
        writeln!(
            out,
            "Invalid selection. Enter a number (1-{}) or team name.",
            teams.len()
        )?;
    }
}

fn prompt_title(out: &mut dyn Write, hint: Option<&str>) -> Result<String> {
    if let Some(t) = hint
        && !t.trim().is_empty()
    {
        return Ok(t.to_string());
    }
    loop {
        let input = read_line(out, "Title: ")?;
        if !input.trim().is_empty() {
            return Ok(input.trim().to_string());
        }
        writeln!(out, "Title is required.")?;
    }
}

fn prompt_description(out: &mut dyn Write, hint: Option<&str>) -> Result<Option<String>> {
    if let Some(d) = hint {
        return Ok(Some(d.to_string()));
    }
    let input = read_line(
        out,
        "Description (optional, press Enter to skip, type 'e' to open editor): ",
    )?;
    if input.trim().is_empty() {
        return Ok(None);
    }
    if input.trim() == "e" {
        return open_editor_for_description();
    }
    Ok(Some(input.trim().to_string()))
}

fn open_editor_for_description() -> Result<Option<String>> {
    let editor = std::env::var("EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "vi".to_string());

    let tmp = std::env::temp_dir().join(format!("lt-issue-desc-{}.md", std::process::id()));
    std::fs::write(&tmp, "")?;

    let status = std::process::Command::new(&editor).arg(&tmp).status()?;

    if !status.success() {
        return Ok(None);
    }

    let content = std::fs::read_to_string(&tmp)?;
    // Best-effort cleanup: the content is already read, so a failure here
    // just leaves a stray temp file behind.
    if let Err(e) = std::fs::remove_file(&tmp) {
        tracing::warn!(error = %e, path = %tmp.display(), "failed to remove temp description file");
    }

    let trimmed = content.trim().to_string();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed))
    }
}

fn prompt_priority(out: &mut dyn Write, hint: Option<&str>) -> Result<u8> {
    if let Some(h) = hint {
        return parse_priority(h)
            .ok_or_else(|| anyhow!("invalid priority '{h}'; use none/urgent/high/normal/low"));
    }
    loop {
        let input = read_line(
            out,
            "Priority [none/urgent/high/normal/low] (default: none): ",
        )?;
        if input.trim().is_empty() {
            return Ok(0);
        }
        if let Some(p) = parse_priority(&input) {
            return Ok(p);
        }
        writeln!(
            out,
            "Invalid priority. Use none, urgent, high, normal, or low."
        )?;
    }
}

fn pick_state<'a>(
    out: &mut dyn Write,
    states: &'a [WorkflowState],
    hint: Option<&str>,
) -> Result<Option<&'a WorkflowState>> {
    if let Some(h) = hint {
        let lower = h.to_lowercase();
        if let Some(s) = states.iter().find(|s| s.name.to_lowercase() == lower) {
            return Ok(Some(s));
        }
        if let Ok(n) = h.parse::<usize>()
            && n >= 1
            && n <= states.len()
        {
            return Ok(Some(&states[n - 1]));
        }
        return Err(anyhow!("no state matching '{h}'"));
    }

    writeln!(out, "Workflow states:")?;
    for (i, s) in states.iter().enumerate() {
        writeln!(out, "  {}. {}", i + 1, s.name)?;
    }

    // Empty input leaves the state unset, so Linear applies the team default.
    let input = read_line(out, "State (number or name, Enter to skip): ")?;

    if input.trim().is_empty() {
        return Ok(None);
    }

    let lower = input.trim().to_lowercase();
    if let Some(s) = states.iter().find(|s| s.name.to_lowercase() == lower) {
        return Ok(Some(s));
    }
    if let Ok(n) = input.trim().parse::<usize>()
        && n >= 1
        && n <= states.len()
    {
        return Ok(Some(&states[n - 1]));
    }

    writeln!(out, "Invalid state, skipping (team default applies).")?;
    Ok(None)
}

fn pick_assignee(
    out: &mut dyn Write,
    members: &[Member],
    viewer: &Viewer,
    hint: Option<&str>,
) -> Result<Option<String>> {
    if let Some(h) = hint {
        let lower = h.to_lowercase();
        if lower == "me" {
            return Ok(Some(viewer.id.inner().to_string()));
        }
        if lower == "none" || lower == "unassigned" {
            return Ok(None);
        }
        // match by name
        if let Some(m) = members.iter().find(|m| m.name.to_lowercase() == lower) {
            return Ok(Some(m.id.inner().to_string()));
        }
        return Err(anyhow!("no member matching '{h}'"));
    }

    writeln!(out, "Assignee (optional):")?;
    writeln!(out, "  0. Unassigned")?;
    writeln!(out, "  me. Assign to me ({})", viewer.name)?;
    for (i, m) in members.iter().enumerate() {
        writeln!(out, "  {}. {}", i + 1, m.name)?;
    }

    let input = read_line(out, "Assignee (number, 'me', or Enter for unassigned): ")?;
    let trimmed = input.trim();

    if trimmed.is_empty() || trimmed == "0" {
        return Ok(None);
    }
    if trimmed.to_lowercase() == "me" {
        return Ok(Some(viewer.id.inner().to_string()));
    }

    let lower = trimmed.to_lowercase();
    if let Some(m) = members.iter().find(|m| m.name.to_lowercase() == lower) {
        return Ok(Some(m.id.inner().to_string()));
    }
    if let Ok(n) = trimmed.parse::<usize>()
        && n >= 1
        && n <= members.len()
    {
        return Ok(Some(members[n - 1].id.inner().to_string()));
    }

    writeln!(out, "Invalid selection, defaulting to unassigned.")?;
    Ok(None)
}

/// Borrowed view of the values shown in the pre-creation confirmation summary.
struct IssueSummary<'a> {
    team_name: &'a str,
    title: &'a str,
    description: Option<&'a str>,
    priority: u8,
    state_id: Option<&'a str>,
    states: &'a [WorkflowState],
    assignee_id: Option<&'a str>,
    viewer: &'a Viewer,
    members: &'a [Member],
}

fn print_summary(out: &mut dyn Write, summary: &IssueSummary) -> Result<()> {
    writeln!(out)?;
    writeln!(out, "--- Issue summary ---")?;
    writeln!(out, "  Team:        {}", summary.team_name)?;
    writeln!(out, "  Title:       {}", summary.title)?;
    if let Some(d) = summary.description {
        let preview: String = d.chars().take(60).collect();
        let ellipsis = if d.len() > 60 { "..." } else { "" };
        writeln!(out, "  Description: {preview}{ellipsis}")?;
    } else {
        writeln!(out, "  Description: (none)")?;
    }
    writeln!(
        out,
        "  Priority:    {}",
        priority_u8_to_label(summary.priority)
    )?;
    if let Some(sid) = summary.state_id {
        let sname = summary
            .states
            .iter()
            .find(|s| s.id.inner() == sid)
            .map_or(sid, |s| s.name.as_str());
        writeln!(out, "  State:       {sname}")?;
    } else {
        writeln!(out, "  State:       (default)")?;
    }
    if let Some(aid) = summary.assignee_id {
        let aname = if aid == summary.viewer.id.inner() {
            summary.viewer.name.clone()
        } else {
            summary
                .members
                .iter()
                .find(|m| m.id.inner() == aid)
                .map_or_else(|| aid.to_string(), |m| m.name.clone())
        };
        writeln!(out, "  Assignee:    {aname}")?;
    } else {
        writeln!(out, "  Assignee:    (unassigned)")?;
    }
    writeln!(out, "---------------------")?;
    Ok(())
}

pub fn run(out: &mut dyn Write, args: &NewIssueArgs) -> Result<()> {
    // Open the session: builds the transport and fetches the viewer (for the
    // "me" shortcut) up front.
    let session = NewIssueSession::open()?;
    let viewer = session.viewer.clone();

    // Step 1: Team
    let teams = session.teams()?;
    if teams.is_empty() {
        return Err(anyhow!("no teams found in your Linear organization"));
    }
    let team = pick_team(out, &teams, args.team.as_deref())?;
    let team_id = team.id.inner().to_string();
    let team_name = team.name.clone();

    // Step 2: Title
    let title = prompt_title(out, args.title.as_deref())?;

    // Step 3: Description
    let description = prompt_description(out, args.description.as_deref())?;

    // Step 4: Priority
    let priority = prompt_priority(out, args.priority.as_deref())?;

    // Step 5: State -- fetch workflow states for the chosen team
    let states = session.workflow_states(&team_id)?;
    let state_id = if states.is_empty() {
        None
    } else {
        pick_state(out, &states, args.state.as_deref())?.map(|s| s.id.inner().to_string())
    };

    // Step 6: Assignee -- fetch team members
    let members = session.team_members(&team_id)?;
    let assignee_id = pick_assignee(out, &members, &viewer, args.assignee.as_deref())?;

    // Confirm summary before creating
    print_summary(
        out,
        &IssueSummary {
            team_name: &team_name,
            title: &title,
            description: description.as_deref(),
            priority,
            state_id: state_id.as_deref(),
            states: &states,
            assignee_id: assignee_id.as_deref(),
            viewer: &viewer,
            members: &members,
        },
    )?;

    let confirm = read_line(out, "Create issue? [Y/n]: ")?;
    if !confirm.trim().is_empty() && confirm.trim().to_lowercase() != "y" {
        writeln!(out, "Aborted.")?;
        return Ok(());
    }

    let input = IssueCreateInput {
        title,
        team_id,
        description,
        state_id,
        priority: if priority == 0 {
            None
        } else {
            Some(i32::from(priority))
        },
        assignee_id,
    };

    let issue = session.create(&input)?;
    writeln!(out, "Created: {} - {}", issue.identifier, issue.title)?;
    writeln!(
        out,
        "URL:     https://linear.app/{}/issue/{}",
        viewer.organization.url_key, issue.identifier
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn team(id: &str, name: &str) -> Team {
        Team {
            id: id.into(),
            name: name.to_string(),
        }
    }

    fn state(id: &str, name: &str) -> WorkflowState {
        WorkflowState {
            id: id.into(),
            name: name.to_string(),
        }
    }

    fn member(id: &str, name: &str) -> Member {
        Member {
            id: id.into(),
            name: name.to_string(),
        }
    }

    fn viewer() -> Viewer {
        Viewer {
            id: "viewer-id".into(),
            name: "Vic Viewer".to_string(),
            organization: lt_types::viewer::Organization {
                name: "Acme".to_string(),
                url_key: "acme".to_string(),
            },
        }
    }

    #[test]
    fn parse_priority_maps_labels_and_numbers() {
        for (input, want) in [
            ("none", 0),
            ("0", 0),
            ("urgent", 1),
            ("1", 1),
            ("high", 2),
            ("2", 2),
            ("normal", 3),
            ("medium", 3),
            ("3", 3),
            ("low", 4),
            ("4", 4),
        ] {
            assert_eq!(parse_priority(input), Some(want), "for {input:?}");
        }
        // Case-insensitive and whitespace-trimmed.
        assert_eq!(parse_priority("  HIGH "), Some(2));
        assert_eq!(parse_priority("bogus"), None);
    }

    #[test]
    fn pick_team_hint_matches_name_case_insensitively() {
        let teams = [team("t1", "Engineering"), team("t2", "Design")];
        let mut out = Vec::new();
        let got = pick_team(&mut out, &teams, Some("engineering")).unwrap();
        assert_eq!(got.id.inner(), "t1");
        // The hint path does not print the menu.
        assert!(out.is_empty());
    }

    #[test]
    fn pick_team_hint_matches_number() {
        let teams = [team("t1", "Engineering"), team("t2", "Design")];
        let mut out = Vec::new();
        let got = pick_team(&mut out, &teams, Some("2")).unwrap();
        assert_eq!(got.id.inner(), "t2");
    }

    #[test]
    fn pick_team_hint_no_match_errors() {
        let teams = [team("t1", "Engineering")];
        let mut out = Vec::new();
        assert!(pick_team(&mut out, &teams, Some("nope")).is_err());
        // Out-of-range numbers are not accepted.
        assert!(pick_team(&mut out, &teams, Some("5")).is_err());
    }

    #[test]
    fn pick_state_hint_matches_name_and_number() {
        let states = [state("s1", "Backlog"), state("s2", "Todo")];
        let mut out = Vec::new();
        assert_eq!(
            pick_state(&mut out, &states, Some("todo"))
                .unwrap()
                .unwrap()
                .id
                .inner(),
            "s2"
        );
        assert_eq!(
            pick_state(&mut out, &states, Some("1"))
                .unwrap()
                .unwrap()
                .id
                .inner(),
            "s1"
        );
        assert!(pick_state(&mut out, &states, Some("nope")).is_err());
    }

    #[test]
    fn pick_assignee_hint_resolves_special_and_matches() {
        let members = [member("m1", "Alice")];
        let v = viewer();
        let mut out = Vec::new();

        assert_eq!(
            pick_assignee(&mut out, &members, &v, Some("me")).unwrap(),
            Some("viewer-id".to_string())
        );
        assert_eq!(
            pick_assignee(&mut out, &members, &v, Some("none")).unwrap(),
            None
        );
        assert_eq!(
            pick_assignee(&mut out, &members, &v, Some("unassigned")).unwrap(),
            None
        );
        assert_eq!(
            pick_assignee(&mut out, &members, &v, Some("alice")).unwrap(),
            Some("m1".to_string())
        );
        assert!(pick_assignee(&mut out, &members, &v, Some("ghost")).is_err());
    }

    #[test]
    fn prompt_helpers_short_circuit_on_hints() {
        let mut out = Vec::new();
        assert_eq!(prompt_title(&mut out, Some("Fix bug")).unwrap(), "Fix bug");
        assert_eq!(
            prompt_description(&mut out, Some("details")).unwrap(),
            Some("details".to_string())
        );
        assert_eq!(prompt_priority(&mut out, Some("high")).unwrap(), 2);
        assert!(prompt_priority(&mut out, Some("bogus")).is_err());
    }

    #[test]
    fn print_summary_renders_all_fields() {
        let states = [state("s1", "Todo")];
        let members = [member("m1", "Alice")];
        let v = viewer();
        let mut out = Vec::new();
        print_summary(
            &mut out,
            &IssueSummary {
                team_name: "Engineering",
                title: "Fix bug",
                description: Some("a short description"),
                priority: 2,
                state_id: Some("s1"),
                states: &states,
                assignee_id: Some("m1"),
                viewer: &v,
                members: &members,
            },
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Team:        Engineering"));
        assert!(text.contains("Title:       Fix bug"));
        assert!(text.contains("Description: a short description"));
        assert!(text.contains("Priority:    High"));
        assert!(text.contains("State:       Todo"));
        assert!(text.contains("Assignee:    Alice"));
    }

    #[test]
    fn print_summary_handles_defaults_and_truncation() {
        let v = viewer();
        let long = "x".repeat(80);
        let mut out = Vec::new();
        print_summary(
            &mut out,
            &IssueSummary {
                team_name: "Engineering",
                title: "t",
                description: Some(&long),
                priority: 0,
                // Unknown state id falls back to printing the raw id.
                state_id: Some("unknown"),
                states: &[],
                // Viewer self-assignment renders the viewer name.
                assignee_id: Some("viewer-id"),
                viewer: &v,
                members: &[],
            },
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        // 60-char preview plus ellipsis.
        assert!(text.contains(&format!("Description: {}...", "x".repeat(60))));
        assert!(text.contains("Priority:    No priority"));
        assert!(text.contains("State:       unknown"));
        assert!(text.contains("Assignee:    Vic Viewer"));
    }

    #[test]
    fn print_summary_renders_unassigned_and_no_description() {
        let v = viewer();
        let mut out = Vec::new();
        print_summary(
            &mut out,
            &IssueSummary {
                team_name: "Engineering",
                title: "t",
                description: None,
                priority: 1,
                state_id: None,
                states: &[],
                assignee_id: None,
                viewer: &v,
                members: &[],
            },
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Description: (none)"));
        assert!(text.contains("State:       (default)"));
        assert!(text.contains("Assignee:    (unassigned)"));
    }
}
