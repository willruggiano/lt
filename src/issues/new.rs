use std::io::{self, BufRead, Write};

use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;

use crate::config;
use crate::linear::client::graphql_query;
use crate::linear::mutations::{
    CreateIssueInput, Team, WorkflowState, create_issue, fetch_teams, fetch_workflow_states,
};

const VIEWER_QUERY: &str = r"
query Viewer {
  viewer {
    id
    name
    email
    organization {
      urlKey
    }
  }
}
";

const TEAM_MEMBERS_QUERY: &str = r"
query TeamMembers($teamId: String!) {
  team(id: $teamId) {
    members {
      nodes {
        id
        name
        email
      }
    }
  }
}
";

#[derive(Deserialize, Debug, Clone)]
struct Organization {
    #[serde(rename = "urlKey")]
    pub url_key: String,
}

#[derive(Deserialize, Debug, Clone)]
struct Viewer {
    pub id: String,
    pub name: String,
    #[allow(dead_code)]
    pub email: String,
    pub organization: Organization,
}

#[derive(Deserialize)]
struct ViewerData {
    viewer: Viewer,
}

#[derive(Deserialize, Debug, Clone)]
struct Member {
    pub id: String,
    pub name: String,
    pub email: String,
}

#[derive(Deserialize)]
struct MemberConnection {
    nodes: Vec<Member>,
}

#[derive(Deserialize)]
struct TeamDetail {
    members: MemberConnection,
}

#[derive(Deserialize)]
struct TeamDetailData {
    team: TeamDetail,
}

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

fn priority_label(p: u8) -> &'static str {
    match p {
        1 => "urgent",
        2 => "high",
        3 => "normal",
        4 => "low",
        _ => "none",
    }
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
    let _ = std::fs::remove_file(&tmp);

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
    // Default: first unstarted state
    let default_state = states.iter().find(|s| s.type_ == "unstarted");

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

    let default_name = default_state.map_or("(first)", |s| s.name.as_str());

    writeln!(out, "Workflow states:")?;
    for (i, s) in states.iter().enumerate() {
        let marker = if Some(s.id.as_str()) == default_state.map(|d| d.id.as_str()) {
            " *"
        } else {
            ""
        };
        writeln!(out, "  {}. {}{}", i + 1, s.name, marker)?;
    }

    let input = read_line(
        out,
        &format!("State (number or name, default: {default_name}): "),
    )?;

    if input.trim().is_empty() {
        return Ok(default_state);
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

    writeln!(out, "Invalid state, using default: {default_name}")?;
    Ok(default_state)
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
            return Ok(Some(viewer.id.clone()));
        }
        if lower == "none" || lower == "unassigned" {
            return Ok(None);
        }
        // match by name or email
        if let Some(m) = members
            .iter()
            .find(|m| m.name.to_lowercase() == lower || m.email.to_lowercase() == lower)
        {
            return Ok(Some(m.id.clone()));
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
        return Ok(Some(viewer.id.clone()));
    }

    let lower = trimmed.to_lowercase();
    if let Some(m) = members
        .iter()
        .find(|m| m.name.to_lowercase() == lower || m.email.to_lowercase() == lower)
    {
        return Ok(Some(m.id.clone()));
    }
    if let Ok(n) = trimmed.parse::<usize>()
        && n >= 1
        && n <= members.len()
    {
        return Ok(Some(members[n - 1].id.clone()));
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
    writeln!(out, "  Priority:    {}", priority_label(summary.priority))?;
    if let Some(sid) = summary.state_id {
        let sname = summary
            .states
            .iter()
            .find(|s| s.id == sid)
            .map_or(sid, |s| s.name.as_str());
        writeln!(out, "  State:       {sname}")?;
    } else {
        writeln!(out, "  State:       (default)")?;
    }
    if let Some(aid) = summary.assignee_id {
        let aname = if aid == summary.viewer.id {
            summary.viewer.name.clone()
        } else {
            summary
                .members
                .iter()
                .find(|m| m.id == aid)
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
    let token = config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;
    let token = token.access_token;

    // Fetch viewer (for "me" shortcut)
    let viewer_data: ViewerData = graphql_query(&token, VIEWER_QUERY, json!({}))?;
    let viewer = viewer_data.viewer;

    // Step 1: Team
    let teams = fetch_teams(&token)?;
    if teams.is_empty() {
        return Err(anyhow!("no teams found in your Linear organization"));
    }
    let team = pick_team(out, &teams, args.team.as_deref())?;
    let team_id = team.id.clone();
    let team_name = team.name.clone();

    // Step 2: Title
    let title = prompt_title(out, args.title.as_deref())?;

    // Step 3: Description
    let description = prompt_description(out, args.description.as_deref())?;

    // Step 4: Priority
    let priority = prompt_priority(out, args.priority.as_deref())?;

    // Step 5: State -- fetch workflow states for the chosen team
    let states = fetch_workflow_states(&token, &team_id)?;
    let state_id = if states.is_empty() {
        None
    } else {
        pick_state(out, &states, args.state.as_deref())?.map(|s| s.id.clone())
    };

    // Step 6: Assignee -- fetch team members
    let members_data: TeamDetailData =
        graphql_query(&token, TEAM_MEMBERS_QUERY, json!({ "teamId": team_id }))?;
    let members = members_data.team.members.nodes;
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

    let input = CreateIssueInput {
        title,
        team_id,
        description,
        state_id,
        priority: if priority == 0 { None } else { Some(priority) },
        assignee_id,
    };

    let issue = create_issue(&token, input)?;
    writeln!(out, "Created: {} - {}", issue.identifier, issue.title)?;
    writeln!(
        out,
        "URL:     https://linear.app/{}/issue/{}",
        viewer.organization.url_key, issue.identifier
    )?;

    Ok(())
}
