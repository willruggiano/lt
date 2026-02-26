use anyhow::{Result, anyhow};
use serde::Deserialize;
use serde_json::json;
use std::io::{self, BufRead, Write};

use crate::config;
use crate::linear::client::graphql_query;
use crate::linear::mutations::{
    CreateIssueInput, Team, WorkflowState, create_issue, fetch_teams, fetch_workflow_states,
};

const VIEWER_QUERY: &str = r#"
query Viewer {
  viewer {
    id
    name
    email
  }
}
"#;

const TEAM_MEMBERS_QUERY: &str = r#"
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
"#;

#[derive(Deserialize, Debug, Clone)]
struct Viewer {
    pub id: String,
    pub name: String,
    pub email: String,
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

fn read_line(prompt: &str) -> Result<String> {
    print!("{}", prompt);
    io::stdout().flush()?;
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

fn pick_team<'a>(teams: &'a [Team], hint: Option<&str>) -> Result<&'a Team> {
    if let Some(h) = hint {
        let lower = h.to_lowercase();
        if let Some(t) = teams.iter().find(|t| t.name.to_lowercase() == lower) {
            return Ok(t);
        }
        if let Ok(n) = h.parse::<usize>() {
            if n >= 1 && n <= teams.len() {
                return Ok(&teams[n - 1]);
            }
        }
        return Err(anyhow!("no team matching '{}'", h));
    }

    println!("Teams:");
    for (i, t) in teams.iter().enumerate() {
        println!("  {}. {}", i + 1, t.name);
    }

    loop {
        let input = read_line("Team (number or name): ")?;
        if input.is_empty() {
            println!("Team is required.");
            continue;
        }
        let lower = input.to_lowercase();
        if let Some(t) = teams.iter().find(|t| t.name.to_lowercase() == lower) {
            return Ok(t);
        }
        if let Ok(n) = input.parse::<usize>() {
            if n >= 1 && n <= teams.len() {
                return Ok(&teams[n - 1]);
            }
        }
        println!(
            "Invalid selection. Enter a number (1-{}) or team name.",
            teams.len()
        );
    }
}

fn prompt_title(hint: Option<&str>) -> Result<String> {
    if let Some(t) = hint {
        if !t.trim().is_empty() {
            return Ok(t.to_string());
        }
    }
    loop {
        let input = read_line("Title: ")?;
        if !input.trim().is_empty() {
            return Ok(input.trim().to_string());
        }
        println!("Title is required.");
    }
}

fn prompt_description(hint: Option<&str>) -> Result<Option<String>> {
    if let Some(d) = hint {
        return Ok(Some(d.to_string()));
    }
    let input =
        read_line("Description (optional, press Enter to skip, type 'e' to open editor): ")?;
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

fn prompt_priority(hint: Option<&str>) -> Result<u8> {
    if let Some(h) = hint {
        return parse_priority(h)
            .ok_or_else(|| anyhow!("invalid priority '{}'; use none/urgent/high/normal/low", h));
    }
    loop {
        let input = read_line("Priority [none/urgent/high/normal/low] (default: none): ")?;
        if input.trim().is_empty() {
            return Ok(0);
        }
        if let Some(p) = parse_priority(&input) {
            return Ok(p);
        }
        println!("Invalid priority. Use none, urgent, high, normal, or low.");
    }
}

fn pick_state<'a>(
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
        if let Ok(n) = h.parse::<usize>() {
            if n >= 1 && n <= states.len() {
                return Ok(Some(&states[n - 1]));
            }
        }
        return Err(anyhow!("no state matching '{}'", h));
    }

    let default_name = default_state.map(|s| s.name.as_str()).unwrap_or("(first)");

    println!("Workflow states:");
    for (i, s) in states.iter().enumerate() {
        let marker = if Some(s.id.as_str()) == default_state.map(|d| d.id.as_str()) {
            " *"
        } else {
            ""
        };
        println!("  {}. {}{}", i + 1, s.name, marker);
    }

    let input = read_line(&format!(
        "State (number or name, default: {}): ",
        default_name
    ))?;

    if input.trim().is_empty() {
        return Ok(default_state);
    }

    let lower = input.trim().to_lowercase();
    if let Some(s) = states.iter().find(|s| s.name.to_lowercase() == lower) {
        return Ok(Some(s));
    }
    if let Ok(n) = input.trim().parse::<usize>() {
        if n >= 1 && n <= states.len() {
            return Ok(Some(&states[n - 1]));
        }
    }

    println!("Invalid state, using default: {}", default_name);
    Ok(default_state)
}

fn pick_assignee(
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
        return Err(anyhow!("no member matching '{}'", h));
    }

    println!("Assignee (optional):");
    println!("  0. Unassigned");
    println!("  me. Assign to me ({})", viewer.name);
    for (i, m) in members.iter().enumerate() {
        println!("  {}. {}", i + 1, m.name);
    }

    let input = read_line("Assignee (number, 'me', or Enter for unassigned): ")?;
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
    if let Ok(n) = trimmed.parse::<usize>() {
        if n >= 1 && n <= members.len() {
            return Ok(Some(members[n - 1].id.clone()));
        }
    }

    println!("Invalid selection, defaulting to unassigned.");
    Ok(None)
}

pub fn run(args: NewIssueArgs) -> Result<()> {
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
    let team = pick_team(&teams, args.team.as_deref())?;
    let team_id = team.id.clone();
    let team_name = team.name.clone();

    // Step 2: Title
    let title = prompt_title(args.title.as_deref())?;

    // Step 3: Description
    let description = prompt_description(args.description.as_deref())?;

    // Step 4: Priority
    let priority = prompt_priority(args.priority.as_deref())?;

    // Step 5: State -- fetch workflow states for the chosen team
    let states = fetch_workflow_states(&token, &team_id)?;
    let state_id = if states.is_empty() {
        None
    } else {
        pick_state(&states, args.state.as_deref())?.map(|s| s.id.clone())
    };

    // Step 6: Assignee -- fetch team members
    let members_data: TeamDetailData =
        graphql_query(&token, TEAM_MEMBERS_QUERY, json!({ "teamId": team_id }))?;
    let members = members_data.team.members.nodes;
    let assignee_id = pick_assignee(&members, &viewer, args.assignee.as_deref())?;

    // Confirm summary before creating
    println!();
    println!("--- Issue summary ---");
    println!("  Team:        {}", team_name);
    println!("  Title:       {}", title);
    if let Some(ref d) = description {
        let preview: String = d.chars().take(60).collect();
        let ellipsis = if d.len() > 60 { "..." } else { "" };
        println!("  Description: {}{}", preview, ellipsis);
    } else {
        println!("  Description: (none)");
    }
    println!("  Priority:    {}", priority_label(priority));
    if let Some(ref sid) = state_id {
        let sname = states
            .iter()
            .find(|s| &s.id == sid)
            .map(|s| s.name.as_str())
            .unwrap_or(sid.as_str());
        println!("  State:       {}", sname);
    } else {
        println!("  State:       (default)");
    }
    if let Some(ref aid) = assignee_id {
        let aname = if aid == &viewer.id {
            viewer.name.clone()
        } else {
            members
                .iter()
                .find(|m| &m.id == aid)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| aid.clone())
        };
        println!("  Assignee:    {}", aname);
    } else {
        println!("  Assignee:    (unassigned)");
    }
    println!("---------------------");

    let confirm = read_line("Create issue? [Y/n]: ")?;
    if !confirm.trim().is_empty() && confirm.trim().to_lowercase() != "y" {
        println!("Aborted.");
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
    println!("Created: {} - {}", issue.identifier, issue.title);
    println!("URL:     https://linear.app/issue/{}", issue.identifier);

    Ok(())
}
