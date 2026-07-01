use std::io::Write;

use anyhow::{Result, anyhow};
use serde::Deserialize;

use crate::client::{HttpTransport, query_as};

const VIEWER_STATUS_QUERY: &str = "{ viewer { id name email organization { name urlKey } } }";

#[derive(Deserialize)]
struct ViewerData {
    viewer: Viewer,
}

#[derive(Deserialize)]
struct Viewer {
    id: String,
    name: String,
    email: String,
    organization: Organization,
}

#[derive(Deserialize)]
struct Organization {
    name: String,
    #[serde(rename = "urlKey")]
    url_key: String,
}

pub fn run(out: &mut dyn Write) -> Result<()> {
    let token = lt_config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;

    let transport = HttpTransport::new(token.access_token);
    let data: ViewerData = query_as(&transport, VIEWER_STATUS_QUERY, serde_json::json!({}))?;
    let viewer = data.viewer;

    writeln!(out, "user:         {} <{}>", viewer.name, viewer.email)?;
    writeln!(out, "id:           {}", viewer.id)?;
    writeln!(
        out,
        "organization: {} ({})",
        viewer.organization.name, viewer.organization.url_key
    )?;

    Ok(())
}
