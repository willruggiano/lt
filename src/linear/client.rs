use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::types::GraphqlResponse;

const GRAPHQL_URL: &str = "https://api.linear.app/graphql";

pub fn graphql_query<T: DeserializeOwned>(token: &str, query: &str, variables: Value) -> Result<T> {
    // Build the body by moving `variables` in (the json! macro would only
    // borrow it, leaving the by-value parameter unconsumed).
    let mut body = serde_json::Map::with_capacity(2);
    body.insert("query".to_owned(), Value::from(query));
    body.insert("variables".to_owned(), variables);
    let body = Value::Object(body);

    let mut response = ureq::post(GRAPHQL_URL)
        .header("Authorization", &format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send_json(&body)
        .context("querying Linear GraphQL API")?;

    let parsed: GraphqlResponse<T> = response
        .body_mut()
        .read_json()
        .context("parsing GraphQL response")?;

    if let Some(errors) = parsed.errors {
        let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
        return Err(anyhow!("GraphQL errors: {}", msgs.join("; ")));
    }

    parsed
        .data
        .ok_or_else(|| anyhow!("GraphQL response contained no data"))
}
