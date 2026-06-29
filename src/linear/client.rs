use anyhow::{Context, Result, anyhow};
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::types::GraphqlResponse;

const GRAPHQL_URL: &str = "https://api.linear.app/graphql";

/// Abstraction over the Linear GraphQL endpoint.
///
/// The method is intentionally non-generic so the trait stays object-safe:
/// callers take `&dyn GraphqlTransport` and tests can substitute a fake. Typed
/// deserialization happens in the free [`query_as`] helper.
pub trait GraphqlTransport {
    /// Execute `query` with `variables` and return the GraphQL `data` payload,
    /// or an error when the transport fails or the response carries `errors`.
    fn query(&self, query: &str, variables: Value) -> Result<Value>;
}

/// Production transport: a Bearer-authenticated POST to Linear over HTTP.
pub struct HttpTransport {
    token: String,
}

impl HttpTransport {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

impl GraphqlTransport for HttpTransport {
    fn query(&self, query: &str, variables: Value) -> Result<Value> {
        // Build the body by moving `variables` in (the json! macro would only
        // borrow it, leaving the by-value parameter unconsumed).
        let mut body = serde_json::Map::with_capacity(2);
        body.insert("query".to_owned(), Value::from(query));
        body.insert("variables".to_owned(), variables);
        let body = Value::Object(body);

        let mut response = ureq::post(GRAPHQL_URL)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .send_json(&body)
            .context("querying Linear GraphQL API")?;

        let raw: Value = response
            .body_mut()
            .read_json()
            .context("parsing GraphQL response")?;

        parse_graphql_response(raw)
    }
}

/// Unwrap a GraphQL response envelope: return the `data` payload, or an error
/// when `errors` is present or `data` is absent.
fn parse_graphql_response(body: Value) -> Result<Value> {
    let parsed: GraphqlResponse<Value> =
        serde_json::from_value(body).context("parsing GraphQL response")?;

    if let Some(errors) = parsed.errors {
        let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
        return Err(anyhow!("GraphQL errors: {}", msgs.join("; ")));
    }

    parsed
        .data
        .ok_or_else(|| anyhow!("GraphQL response contained no data"))
}

/// Execute a GraphQL query through `transport` and deserialize the `data`
/// payload into `T`.
pub fn query_as<T: DeserializeOwned>(
    transport: &dyn GraphqlTransport,
    query: &str,
    variables: Value,
) -> Result<T> {
    let data = transport.query(query, variables)?;
    serde_json::from_value(data).context("deserializing GraphQL data")
}

/// Build an [`HttpTransport`] from `token` and run `query`, deserializing the
/// `data` payload into `T`.
///
/// Retained as a thin convenience while callers migrate to passing an explicit
/// `&dyn GraphqlTransport`.
pub fn graphql_query<T: DeserializeOwned>(token: &str, query: &str, variables: Value) -> Result<T> {
    query_as(&HttpTransport::new(token), query, variables)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parse_returns_data_payload() {
        let body = json!({ "data": { "viewer": { "id": "u1" } } });
        let data = parse_graphql_response(body).unwrap();
        assert_eq!(data, json!({ "viewer": { "id": "u1" } }));
    }

    #[test]
    fn parse_joins_graphql_errors() {
        let body = json!({ "errors": [{ "message": "bad" }, { "message": "worse" }] });
        let err = parse_graphql_response(body).unwrap_err().to_string();
        assert!(err.contains("bad; worse"), "got: {err}");
    }

    #[test]
    fn parse_rejects_missing_data() {
        let err = parse_graphql_response(json!({})).unwrap_err().to_string();
        assert!(err.contains("no data"), "got: {err}");
    }
}
