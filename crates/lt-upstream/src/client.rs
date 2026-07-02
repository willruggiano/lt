use anyhow::{Context, Result, anyhow};
use lt_types::graphql::GraphqlOperation;
use lt_types::types::GraphqlResponse;
use serde_json::Value;

const GRAPHQL_URL: &str = "https://api.linear.app/graphql";

/// Abstraction over the Linear GraphQL endpoint.
///
/// The method is intentionally non-generic so the trait stays object-safe:
/// callers take `&dyn GraphqlTransport` and tests can substitute a fake. Typed
/// deserialization happens in the free [`execute`] function.
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

/// Run one [`GraphqlOperation`] through `transport`, sending its typed
/// variables and decoding the response into its domain output.
pub fn execute<Op: GraphqlOperation>(
    transport: &dyn GraphqlTransport,
    variables: Op::Variables,
) -> Result<Op::Output> {
    let op = Op::operation(variables);
    let vars = serde_json::to_value(&op.variables).context("serializing GraphQL variables")?;
    let data = transport.query(&op.query, vars)?;
    serde_json::from_value::<Op>(data)
        .with_context(|| format!("deserializing {} response", Op::NAME))?
        .extract()
}

/// Test double for [`GraphqlTransport`]: returns scripted `data` payloads in
/// order and records the `(query, variables)` of each call. Shared across the
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
impl GraphqlTransport for FakeTransport {
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
        let states = execute::<lt_types::states::WorkflowStatesQuery>(
            &transport,
            lt_types::states::TeamVariables {
                team_id: "t1".to_string(),
            },
        )
        .unwrap();
        assert!(states.is_empty());
        assert_eq!(transport.variables(0), json!({ "teamId": "t1" }));
    }

    #[test]
    fn execute_decode_error_names_the_operation() {
        // Missing the required `team` field, so decoding into the operation
        // envelope fails.
        let transport = FakeTransport::new(vec![json!({})]);
        let Err(err) = execute::<lt_types::states::WorkflowStatesQuery>(
            &transport,
            lt_types::states::TeamVariables {
                team_id: "t1".to_string(),
            },
        ) else {
            panic!("expected a decode error");
        };
        let err = err.to_string();
        assert!(err.contains("workflowStates"), "got: {err}");
    }
}
