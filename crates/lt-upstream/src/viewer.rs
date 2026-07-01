//! Fetch the authenticated user's identity (viewer) from the Linear API.

use anyhow::Result;
use lt_types::types::Viewer;
use lt_types::viewer::ViewerQuery;
use serde_json::Value;

use super::client::{GraphqlTransport, query_as};

pub fn fetch(transport: &dyn GraphqlTransport) -> Result<Viewer> {
    // The viewer query takes no variables; cynic builds the string in lt-types.
    let data: ViewerQuery = query_as(transport, &lt_types::viewer::query(), Value::Null)?;
    Ok(Viewer {
        id: data.viewer.id.into_inner(),
        name: data.viewer.name,
        org_name: data.viewer.organization.name,
        org_url_key: data.viewer.organization.url_key,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::client::FakeTransport;

    #[test]
    fn fetch_viewer_maps_nested_fields() {
        let transport = FakeTransport::new(vec![json!({
            "viewer": {
                "id": "u1",
                "name": "Ada",
                "email": "ada@acme.test",
                "organization": { "name": "Acme", "urlKey": "acme" }
            }
        })]);
        let viewer = fetch(&transport).unwrap();
        assert_eq!(viewer.id, "u1");
        assert_eq!(viewer.name, "Ada");
        assert_eq!(viewer.org_name, "Acme");
        assert_eq!(viewer.org_url_key, "acme");
    }
}
