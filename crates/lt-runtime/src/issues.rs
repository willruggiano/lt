//! Issue command orchestration: the live list fetch, the synchronous create,
//! and the new-issue picker reads (teams, states, members, viewer). The API
//! edge lives in `lt-upstream`; these entry points construct a transport and
//! hand back plain data so `lt-cli` never names `HttpTransport`/`query_as`.

use anyhow::{Result, anyhow};
use lt_types::inputs::IssueCreateInput;
use lt_types::types::{Issue, Team, User, WorkflowState};
use lt_types::viewer;
use lt_upstream::client::HttpTransport;
pub use lt_upstream::issues::fetch;

/// Build a transport from the stored token, erroring when not logged in.
fn transport_from_config() -> Result<HttpTransport> {
    let token = lt_config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;
    Ok(HttpTransport::new(token.access_token))
}

/// A ready-to-drive new-issue session: a transport plus the viewer identity.
pub struct NewIssueSession {
    transport: HttpTransport,
    pub viewer: viewer::User,
}

impl NewIssueSession {
    /// Open a session: build the transport and fetch the viewer up front.
    pub fn open() -> Result<Self> {
        let transport = transport_from_config()?;
        let viewer = lt_upstream::viewer::fetch(&transport)?;
        Ok(Self { transport, viewer })
    }

    /// List the teams the viewer can file issues against.
    pub fn teams(&self) -> Result<Vec<Team>> {
        lt_upstream::teams::fetch(&self.transport)
    }

    /// List a team's workflow states.
    pub fn workflow_states(&self, team_id: &str) -> Result<Vec<WorkflowState>> {
        lt_upstream::states::fetch(&self.transport, team_id)
    }

    /// List a team's members.
    pub fn team_members(&self, team_id: &str) -> Result<Vec<User>> {
        lt_upstream::members::fetch(&self.transport, team_id)
    }

    /// Create an issue synchronously.
    pub fn create(&self, input: &IssueCreateInput) -> Result<Issue> {
        lt_upstream::issues::create(&self.transport, input)
    }
}
