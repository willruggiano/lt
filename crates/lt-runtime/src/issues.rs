//! Issue command orchestration: the live list fetch, the synchronous create,
//! and the new-issue picker reads (teams, states, members, viewer). The API
//! edge lives in `lt-upstream`; these entry points construct a transport and
//! hand back plain data so `lt-cli` never names `HttpTransport`/`query_as`.

use anyhow::{Result, anyhow};
use lt_types::inputs::IssueCreateInput;
use lt_types::sync_dto::{Team, WorkflowState};
use lt_upstream::client::{HttpTransport, query_as};
use lt_upstream::issues::CreatedIssue;
pub use lt_upstream::issues::fetch;
use serde::Deserialize;
use serde_json::json;

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

/// The Linear organization (workspace) as seen by the new-issue flow.
#[derive(Deserialize, Debug, Clone)]
pub struct Organization {
    #[serde(rename = "urlKey")]
    pub url_key: String,
}

/// The viewer as seen by the new-issue flow: identity plus the org url-key used
/// to render the created issue's URL.
#[derive(Deserialize, Debug, Clone)]
pub struct NewIssueViewer {
    pub id: String,
    pub name: String,
    pub email: String,
    pub organization: Organization,
}

impl NewIssueViewer {
    /// The org's url-key (the `linear.app/<key>` path segment).
    pub fn org_url_key(&self) -> &str {
        &self.organization.url_key
    }
}

#[derive(Deserialize)]
struct ViewerData {
    viewer: NewIssueViewer,
}

/// A team member for the new-issue assignee picker (carries email for matching).
#[derive(Deserialize, Debug, Clone)]
pub struct NewIssueMember {
    pub id: String,
    pub name: String,
    pub email: String,
}

#[derive(Deserialize)]
struct MemberConnection {
    nodes: Vec<NewIssueMember>,
}

#[derive(Deserialize)]
struct TeamDetail {
    members: MemberConnection,
}

#[derive(Deserialize)]
struct TeamDetailData {
    team: TeamDetail,
}

/// Build a transport from the stored token, erroring when not logged in.
fn transport_from_config() -> Result<HttpTransport> {
    let token = lt_config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;
    Ok(HttpTransport::new(token.access_token))
}

/// A ready-to-drive new-issue session: a transport plus the viewer identity.
pub struct NewIssueSession {
    transport: HttpTransport,
    pub viewer: NewIssueViewer,
}

impl NewIssueSession {
    /// Open a session: build the transport and fetch the viewer up front.
    pub fn open() -> Result<Self> {
        let transport = transport_from_config()?;
        let viewer_data: ViewerData = query_as(&transport, VIEWER_QUERY, json!({}))?;
        Ok(Self {
            transport,
            viewer: viewer_data.viewer,
        })
    }

    /// List the teams the viewer can file issues against.
    pub fn teams(&self) -> Result<Vec<Team>> {
        lt_upstream::teams::fetch(&self.transport)
    }

    /// List a team's workflow states.
    pub fn workflow_states(&self, team_id: &str) -> Result<Vec<WorkflowState>> {
        lt_upstream::states::fetch(&self.transport, team_id)
    }

    /// List a team's members (with email, for the assignee picker).
    pub fn team_members(&self, team_id: &str) -> Result<Vec<NewIssueMember>> {
        let data: TeamDetailData = query_as(
            &self.transport,
            TEAM_MEMBERS_QUERY,
            json!({ "teamId": team_id }),
        )?;
        Ok(data.team.members.nodes)
    }

    /// Create an issue synchronously.
    pub fn create(&self, input: &IssueCreateInput) -> Result<CreatedIssue> {
        lt_upstream::issues::create(&self.transport, input)
    }
}
