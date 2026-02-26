use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

use crate::config;

#[derive(Deserialize)]
struct GraphqlResponse<T> {
    data: Option<T>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
}

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

pub fn run() -> Result<()> {
    let token = config::load_token()?
        .ok_or_else(|| anyhow!("not logged in -- run `lt auth login` first"))?;

    let body = serde_json::json!({
        "query": "{ viewer { id name email organization { name urlKey } } }"
    });

    let response = ureq::post("https://api.linear.app/graphql")
        .set("Authorization", &format!("Bearer {}", token.access_token))
        .set("Content-Type", "application/json")
        .send_json(&body)
        .context("querying Linear API")?;

    let parsed: GraphqlResponse<ViewerData> = response
        .into_json()
        .context("parsing API response")?;

    if let Some(errors) = parsed.errors {
        let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
        return Err(anyhow!("GraphQL errors: {}", msgs.join(", ")));
    }

    let viewer = parsed
        .data
        .ok_or_else(|| anyhow!("empty response from Linear API"))?
        .viewer;

    println!("user:         {} <{}>", viewer.name, viewer.email);
    println!("id:           {}", viewer.id);
    println!(
        "organization: {} ({})",
        viewer.organization.name, viewer.organization.url_key
    );

    Ok(())
}
