use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::types::GraphqlResponse;

const GRAPHQL_URL: &str = "https://api.linear.app/graphql";

pub fn graphql_query<T: DeserializeOwned>(token: &str, query: &str, variables: Value) -> Result<T> {
    let body = serde_json::json!({
        "query": query,
        "variables": variables,
    });

    let response = ureq::post(GRAPHQL_URL)
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", "application/json")
        .send_json(&body)
        .context("querying Linear GraphQL API")?;

    let parsed: GraphqlResponse<T> = response.into_json().context("parsing GraphQL response")?;

    if let Some(errors) = parsed.errors {
        let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
        return Err(anyhow!("GraphQL errors: {}", msgs.join("; ")));
    }

    parsed
        .data
        .ok_or_else(|| anyhow!("GraphQL response contained no data"))
}
