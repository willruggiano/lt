//! Fetch the authenticated user's identity (viewer) from the Linear API.

use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

use super::client::{GraphqlTransport, query_as};

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

pub fn fetch_viewer(transport: &dyn GraphqlTransport) -> Result<Viewer> {
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

    let data: ViewerData = query_as(transport, VIEWER_QUERY, json!({}))?;
    Ok(Viewer {
        id: data.viewer.id,
        name: data.viewer.name,
        org_name: data.viewer.organization.name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear::client::FakeTransport;

    #[test]
    fn fetch_viewer_maps_nested_fields() {
        let transport = FakeTransport::new(vec![json!({
            "viewer": { "id": "u1", "name": "Ada", "organization": { "name": "Acme" } }
        })]);
        let viewer = fetch_viewer(&transport).unwrap();
        assert_eq!(viewer.id, "u1");
        assert_eq!(viewer.name, "Ada");
        assert_eq!(viewer.org_name, "Acme");
    }
}
