//! Fetch the authenticated user's identity (viewer) from the Linear API.

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

use super::client::graphql_query;

const VIEWER_QUERY: &str = r"
query Viewer {
  viewer {
    id
    name
    organization {
      name
    }
  }
}
";

/// The authenticated user's identity.
pub struct Viewer {
    pub id: String,
    pub name: String,
    /// Linear organization (workspace) name.
    pub org_name: String,
}

pub fn fetch_viewer(token: &str) -> Result<Viewer> {
    #[derive(Deserialize)]
    struct OrgNode {
        name: String,
    }
    #[derive(Deserialize)]
    struct ViewerNode {
        id: String,
        name: String,
        organization: OrgNode,
    }
    #[derive(Deserialize)]
    struct ViewerData {
        viewer: ViewerNode,
    }

    let data: ViewerData = graphql_query(token, VIEWER_QUERY, json!({}))?;
    Ok(Viewer {
        id: data.viewer.id,
        name: data.viewer.name,
        org_name: data.viewer.organization.name,
    })
}
