//! Shared GraphQL operation helpers used across the domain modules.
//!
//! These generics keep the per-domain modules (`issues`, `comments`, `teams`,
//! `states`, `members`) free of copy-pasted decode/replay boilerplate: one
//! `post_create` serves every `*Create` replay, one `fetch_team_items` serves
//! every `team(id) { items }` list query.

use anyhow::{Result, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;

use crate::client::{GraphqlTransport, query_as};

/// A `*Create` response envelope: a success flag plus the created entity.
pub(crate) trait CreatePayload: DeserializeOwned {
    type Created;
    fn into_created(self) -> (bool, Self::Created);
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
    Ok(created)
}

/// The `team(id) { items: <conn> { nodes } }` envelope, shared by the workflow-
/// state and team-member queries via the `items:` field alias.
#[derive(Deserialize)]
struct TeamItems<T> {
    team: TeamItemsTeam<T>,
}

#[derive(Deserialize)]
struct TeamItemsTeam<T> {
    items: ItemsConnection<T>,
}

#[derive(Deserialize)]
struct ItemsConnection<T> {
    nodes: Vec<T>,
}

/// Run a `team(id) { items: <conn> { nodes } }` query and return the nodes.
/// Both team-scoped list queries alias their connection to `items`, so one
/// generic decode serves both.
pub(crate) fn fetch_team_items<T: DeserializeOwned>(
    transport: &dyn GraphqlTransport,
    query: &str,
    team_id: &str,
) -> Result<Vec<T>> {
    let data: TeamItems<T> = query_as(transport, query, json!({ "teamId": team_id }))?;
    Ok(data.team.items.nodes)
}
