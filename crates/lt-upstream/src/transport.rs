use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::query::graphql::GraphqlOperation;
use crate::query::types::GraphqlResponse;

const GRAPHQL_URL: &str = "https://api.linear.app/graphql";

/// Abstraction over the Linear GraphQL endpoint.
///
/// The method is intentionally non-generic so the trait stays object-safe:
/// callers take `&dyn Transport` and tests can substitute a fake. Typed
/// deserialization happens in the free [`execute`] function.
pub trait Transport {
    /// Execute `query` with `variables` and return the GraphQL `data` payload,
    /// or an error when the transport fails or the response carries `errors`.
    fn query(&self, query: &str, variables: Value) -> Result<Value>;
}

/// Production transport: a Bearer-authenticated POST to Linear over HTTP,
/// under a token fixed at construction.
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

impl Transport for HttpTransport {
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

/// Production transport for `lt-runtime`'s long-lived `Runtime`: loads (and
/// silently refreshes, per [`crate::auth::refresh::load_or_refresh_token`])
/// the stored OAuth token on every call, rather than fixing it once like
/// [`HttpTransport`]. Reconciles `Runtime<S, T>` holding one `T` for its whole
/// lifetime with a token that can expire mid-run.
#[derive(Default)]
pub struct RefreshingHttpTransport;

impl Transport for RefreshingHttpTransport {
    fn query(&self, query: &str, variables: Value) -> Result<Value> {
        let token = crate::auth::refresh::load_or_refresh_token()?;
        HttpTransport::new(token.access_token).query(query, variables)
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

/// Run one [`GraphqlOperation`] through `transport`, sending its typed
/// variables and decoding the response into its domain output: the decoded
/// envelope recomposes into `Op::Output` via its `TryFrom` impl.
pub fn execute<Op>(transport: &dyn Transport, variables: Op::Variables) -> Result<Op::Output>
where
    Op: GraphqlOperation,
    Op::Output: TryFrom<Op, Error = anyhow::Error>,
{
    let op = Op::operation(variables);
    let vars = serde_json::to_value(&op.variables).context("serializing GraphQL variables")?;
    let data = transport.query(&op.query, vars)?;
    let decoded = serde_json::from_value::<Op>(data)
        .with_context(|| format!("deserializing {} response", Op::NAME))?;
    Op::Output::try_from(decoded)
}

/// Test double for [`Transport`]: returns scripted `data` payloads in order
/// and records the `(query, variables)` of each call. Shared across the
/// crate's fetcher tests and the `lt-runtime` sync/comment tests (via the
/// `test-util` feature).
#[cfg(any(test, feature = "test-util"))]
pub struct FakeTransport {
    responses: std::cell::RefCell<std::collections::VecDeque<Result<Value>>>,
    pub calls: std::cell::RefCell<Vec<(String, Value)>>,
}

#[cfg(any(test, feature = "test-util"))]
impl FakeTransport {
    /// Script a sequence of successful `data` payloads, one per `query` call.
    pub fn new(responses: Vec<Value>) -> Self {
        Self {
            responses: std::cell::RefCell::new(responses.into_iter().map(Ok).collect()),
            calls: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The variables passed to the call at `index`.
    pub fn variables(&self, index: usize) -> Value {
        self.calls.borrow()[index].1.clone()
    }
}

#[cfg(any(test, feature = "test-util"))]
impl Transport for FakeTransport {
    fn query(&self, query: &str, variables: Value) -> Result<Value> {
        self.calls.borrow_mut().push((query.to_string(), variables));
        self.responses
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| Err(anyhow!("FakeTransport: no scripted response")))
    }
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

    #[test]
    fn execute_sends_typed_variables_under_graphql_names() {
        let transport = FakeTransport::new(vec![json!({
            "team": { "states": { "nodes": [] } }
        })]);
        let states = execute::<crate::query::states::WorkflowStatesQuery>(
            &transport,
            crate::query::states::TeamVariables {
                team_id: "t1".to_string(),
            },
        )
        .unwrap();
        assert!(states.nodes.is_empty());
        assert_eq!(transport.variables(0), json!({ "teamId": "t1" }));
    }

    #[test]
    fn execute_decode_error_names_the_operation() {
        // Missing the required `team` field, so decoding into the operation
        // envelope fails.
        let transport = FakeTransport::new(vec![json!({})]);
        let Err(err) = execute::<crate::query::states::WorkflowStatesQuery>(
            &transport,
            crate::query::states::TeamVariables {
                team_id: "t1".to_string(),
            },
        ) else {
            panic!("expected a decode error");
        };
        let err = err.to_string();
        assert!(err.contains("workflowStates"), "got: {err}");
    }
}
