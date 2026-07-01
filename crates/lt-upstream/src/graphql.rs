//! Shared GraphQL operation helpers used across the domain modules.
//!
//! `post_create` serves every `*Create` replay (`issues`, `comments`).
//! `fetch_team_scoped` serves every `team(id) { <conn> { nodes } }` read
//! (`states`, `members`) via [`lt_types::graphql::TeamScopedQuery`], so the
//! two operations decode through one generic function despite selecting
//! different connection fields.

use anyhow::{Result, bail};
use lt_types::graphql::TeamScopedQuery;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::client::{GraphqlTransport, query_as};

/// Run a `team(id) { <conn> { nodes } }` query and return its nodes.
pub(crate) fn fetch_team_scoped<R: TeamScopedQuery>(
    transport: &dyn GraphqlTransport,
    query: &str,
    team_id: &str,
) -> Result<Vec<R::Node>> {
    let data: R = query_as(transport, query, json!({ "teamId": team_id }))?;
    Ok(data.into_nodes())
}

/// A `*Create` response envelope: a success flag plus the created entity. The
/// entity is optional because some payloads (e.g. `IssuePayload.issue`) are
/// nullable in the schema even on success.
pub(crate) trait CreatePayload: DeserializeOwned {
    type Created;
    fn into_created(self) -> (bool, Option<Self::Created>);
}

/// Replay a `*Create` mutation from its stored variables, returning the server's
/// created entity for temp-row reconciliation.
pub(crate) fn post_create<R: CreatePayload>(
    transport: &dyn GraphqlTransport,
    mutation: &str,
    op: &str,
    variables: serde_json::Value,
) -> Result<R::Created> {
    let (success, created) = query_as::<R>(transport, mutation, variables)?.into_created();
    if !success {
        bail!("{op} returned success=false");
    }
    created.ok_or_else(|| anyhow::anyhow!("{op} returned no entity"))
}
