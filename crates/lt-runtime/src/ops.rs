//! The operation seam: the [`Query`]/[`Fill`]/[`Mutation`] traits every
//! operation implements (docs/design/operation-seam-adr.md, "Decision 1"),
//! their per-operation impls -- thin, calling `lt-storage`'s public cache
//! functions -- and the generic local-read/upstream-refresh drivers built on
//! top of them, plus the [`Operation`] dispatch trait behind
//! [`crate::Runtime::execute`] (docs/design/unified-execute-adr.md, "Decision
//! 2"). Fragment-to-SQL lowering stays crate-private in `lt-storage`; this
//! module only ever calls its `pub fn`s.

use anyhow::{Context, Result};
use lt_storage::db;
use lt_storage::db::Connection;
use lt_types::comments::{CommentCreateMutation, CommentsQuery, CommentsVariables};
use lt_types::detail::{IssueDetailData, IssueDetailQuery};
use lt_types::graphql::GraphqlOperation;
use lt_types::issues::{IssueCreateMutation, IssueUpdateMutation, IssuesQuery};
use lt_types::members::{TeamMembersQuery, UserConnection};
use lt_types::new_issue::{NewIssueData, NewIssueQuery};
use lt_types::states::{AllWorkflowStatesQuery, TeamStatesQuery, WorkflowStateConnection};
use lt_types::teams::{TeamConnection, TeamsQuery};
use lt_types::types::WorkflowState;
use lt_types::viewer::ViewerQuery;
use lt_upstream::client::{GraphqlTransport, execute};

use crate::runtime::Runtime;

/// A local, cache-backed read of an operation's result.
pub trait Query: GraphqlOperation {
    fn query(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output>;
}

/// Write an already-fetched operation response into the cache: the read
/// path's fetch-and-fill, shared by [`refresh`] and the sync drivers
/// (`sync::sync_pages`, `sync::sync_workflow_states`). Every query-kind
/// operation implements this; a mutation-kind operation's local write is
/// [`Mutation::enqueue`] instead -- the two never overlap on one operation.
pub trait Fill: GraphqlOperation {
    fn fill(conn: &Connection, vars: &Self::Variables, out: &Self::Output) -> Result<()>;
}

/// The drainer's ack context: the op-log row's own identity, as recorded by
/// [`Mutation::enqueue`] and read back at replay.
pub struct AckContext<'a> {
    pub seq: i64,
    pub id: &'a str,
}

/// The op-log's mutation-side vocabulary: the optimistic local write plus its
/// enqueue and ack. Implemented only by the three real mutations
/// (`IssueUpdateMutation`, `IssueCreateMutation`, `CommentCreateMutation`) --
/// a query operation's fetched-response cache write is [`Fill`] instead, so
/// this trait carries no query-only "unsupported" defaults.
pub trait Mutation: GraphqlOperation {
    /// Write the operation's optimistic local effect and enqueue its op-log
    /// row from `vars`, atomically. Returns the id it wrote under (`vars.id`
    /// for an update, the fabricated id for a create) so the caller can read
    /// the optimistic entity straight back out of the cache.
    fn enqueue(conn: &Connection, vars: Self::Variables) -> Result<String>;

    /// Rebuild the wire variables for a pending replay by re-reading the row
    /// the op-log points at (the op-log stores no variables).
    fn replay_vars(conn: &Connection, id: &str) -> Result<Self::Variables>;

    /// Reconcile the base and retire the op once the drainer has `out`, the
    /// mutation's decoded response.
    fn ack(conn: &Connection, ctx: AckContext<'_>, out: Self::Output) -> Result<()>;
}

// ---------------------------------------------------------------------------
// Query / Fill impls
// ---------------------------------------------------------------------------

impl Query for IssuesQuery {
    fn query(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output> {
        db::query_issues(conn, vars)
    }
}

impl Fill for IssuesQuery {
    /// An issue upsert also writes its referenced team and workflow-state
    /// rows (`db::upsert_issues`).
    fn fill(conn: &Connection, _vars: &Self::Variables, out: &Self::Output) -> Result<()> {
        db::upsert_issues(conn, &out.nodes)
    }
}

impl Query for TeamsQuery {
    fn query(conn: &Connection, _vars: &Self::Variables) -> Result<Self::Output> {
        Ok(TeamConnection {
            nodes: db::query_teams(conn)?,
        })
    }
}

impl Fill for TeamsQuery {
    fn fill(conn: &Connection, _vars: &Self::Variables, out: &Self::Output) -> Result<()> {
        db::upsert_teams(conn, &out.nodes)
    }
}

impl Query for TeamStatesQuery {
    fn query(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output> {
        Ok(WorkflowStateConnection {
            nodes: db::query_team_states(conn, &vars.team_id)?,
        })
    }
}

impl Fill for TeamStatesQuery {
    /// The team id must come from the variables, not `out`: a `WorkflowState`
    /// carries only `{id, name, position}`, with no back-reference to its team.
    fn fill(conn: &Connection, vars: &Self::Variables, out: &Self::Output) -> Result<()> {
        for state in &out.nodes {
            db::upsert_team_state(conn, &vars.team_id, state)?;
        }
        Ok(())
    }
}

impl Fill for AllWorkflowStatesQuery {
    /// Unlike [`TeamStatesQuery`], each node carries its own team id, so one
    /// page can span every team.
    fn fill(conn: &Connection, _vars: &Self::Variables, out: &Self::Output) -> Result<()> {
        for state in &out.nodes {
            let team_id = state.team.id.inner();
            db::upsert_team_state(
                conn,
                team_id,
                &WorkflowState {
                    id: state.id.clone(),
                    name: state.name.clone(),
                    position: state.position,
                },
            )?;
        }
        Ok(())
    }
}

impl Query for TeamMembersQuery {
    fn query(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output> {
        Ok(UserConnection {
            nodes: db::query_team_members(conn, &vars.team_id)?,
        })
    }
}

impl Fill for TeamMembersQuery {
    /// Replace-set semantics preserved: users are upserted, then the team's
    /// membership rows are replaced wholesale so a member no longer on the
    /// team is dropped rather than left stale.
    fn fill(conn: &Connection, vars: &Self::Variables, out: &Self::Output) -> Result<()> {
        db::upsert_users(conn, &out.nodes)?;
        let member_ids: Vec<&str> = out.nodes.iter().map(|u| u.id.inner()).collect();
        db::replace_team_memberships(conn, &vars.team_id, &member_ids)
    }
}

impl Query for NewIssueQuery {
    fn query(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output> {
        let teams = db::query_teams(conn)?;
        let (states, members) = if vars.has_team {
            (
                db::query_team_states(conn, &vars.team_id)?,
                db::query_team_members(conn, &vars.team_id)?,
            )
        } else {
            (Vec::new(), Vec::new())
        };
        let viewer = db::viewer(conn)?;
        Ok(NewIssueData {
            teams,
            states,
            members,
            viewer,
        })
    }
}

impl Fill for NewIssueQuery {
    /// `out.viewer` is never persisted here: it is always `None` from the
    /// wire (`NewIssueQuery`'s document does not select it, see
    /// `lt_types::new_issue`), and the display value is sourced from the
    /// cache via `Query` instead.
    fn fill(conn: &Connection, vars: &Self::Variables, out: &Self::Output) -> Result<()> {
        db::upsert_teams(conn, &out.teams)?;
        if vars.has_team {
            for state in &out.states {
                db::upsert_team_state(conn, &vars.team_id, state)?;
            }
            db::upsert_users(conn, &out.members)?;
            let member_ids: Vec<&str> = out.members.iter().map(|u| u.id.inner()).collect();
            db::replace_team_memberships(conn, &vars.team_id, &member_ids)?;
        }
        Ok(())
    }
}

impl Query for ViewerQuery {
    fn query(conn: &Connection, _vars: &Self::Variables) -> Result<Self::Output> {
        db::viewer(conn)
    }
}

impl Fill for ViewerQuery {
    fn fill(conn: &Connection, _vars: &Self::Variables, out: &Self::Output) -> Result<()> {
        let Some(viewer) = out else {
            return Ok(());
        };
        db::set_viewer(conn, viewer)
    }
}

impl Query for IssueDetailQuery {
    /// `None` when the id is locally absent: the current detail view opens
    /// from a listed (already-cached) issue, so absence means a stale cache
    /// after an upstream delete, not a bug to panic over.
    fn query(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output> {
        let Some(issue) = db::query_issue_by_id(conn, &vars.id)? else {
            return Ok(None);
        };
        let comments = db::query_comments(conn, &vars.id)?;
        let children = db::query_children(conn, &vars.id)?;
        Ok(Some(IssueDetailData {
            issue,
            comments,
            children,
            // The local cache always holds the whole thread (comments append
            // to exhaustion on every refresh), so there is never a next page.
            comments_cursor: None,
        }))
    }
}

impl Fill for IssueDetailQuery {
    /// The issue and its children go through the issue upsert path; comments
    /// replace the set, same as the per-entity comment upsert did.
    fn fill(conn: &Connection, vars: &Self::Variables, out: &Self::Output) -> Result<()> {
        let Some(data) = out else {
            return Ok(());
        };

        let mut nodes = Vec::with_capacity(1 + data.children.len());
        nodes.push(data.issue.clone());
        nodes.extend(data.children.iter().cloned());
        db::upsert_issues(conn, &nodes)?;

        db::delete_comments_for_issue(conn, &vars.id)?;
        db::upsert_comments(conn, &data.comments)
    }
}

impl Fill for CommentsQuery {
    /// Appends the page rather than replacing the set: this operation only
    /// ever runs mid-pagination, in [`IssueDetailQuery`]'s own [`Refresh`]
    /// impl below, after that operation's own fill has already replaced the
    /// set with the first page. A delete-first here would wipe that page's
    /// inserts.
    fn fill(conn: &Connection, _vars: &Self::Variables, out: &Self::Output) -> Result<()> {
        db::upsert_comments(conn, &out.nodes)
    }
}

// ---------------------------------------------------------------------------
// Mutation impls -- the three real op-log mutations
// ---------------------------------------------------------------------------

impl Mutation for IssueUpdateMutation {
    fn enqueue(conn: &Connection, vars: Self::Variables) -> Result<String> {
        db::op_log::enqueue_issue_update(conn, vars)
    }

    fn replay_vars(conn: &Connection, id: &str) -> Result<Self::Variables> {
        db::op_log::issue_update_replay_vars(conn, id)
    }

    fn ack(conn: &Connection, ctx: AckContext<'_>, out: Self::Output) -> Result<()> {
        db::op_log::ack_issue_update(conn, ctx.seq, ctx.id, out.as_ref())
    }
}

impl Mutation for IssueCreateMutation {
    fn enqueue(conn: &Connection, vars: Self::Variables) -> Result<String> {
        db::op_log::enqueue_issue_create(conn, &vars)
    }

    fn replay_vars(conn: &Connection, id: &str) -> Result<Self::Variables> {
        db::op_log::issue_create_replay_vars(conn, id)
    }

    fn ack(conn: &Connection, ctx: AckContext<'_>, out: Self::Output) -> Result<()> {
        db::op_log::ack_issue_create(conn, ctx.seq, ctx.id, &out)
    }
}

impl Mutation for CommentCreateMutation {
    fn enqueue(conn: &Connection, vars: Self::Variables) -> Result<String> {
        db::op_log::enqueue_comment_create(conn, &vars)
    }

    fn replay_vars(conn: &Connection, id: &str) -> Result<Self::Variables> {
        db::op_log::comment_create_replay_vars(conn, id)
    }

    /// The comment row already carries its `issue_id` (set at enqueue); ack
    /// only attaches the server id and stamps `synced_at`.
    fn ack(conn: &Connection, ctx: AckContext<'_>, out: Self::Output) -> Result<()> {
        db::op_log::ack_comment_create(conn, ctx.seq, ctx.id, &out)
    }
}

// ---------------------------------------------------------------------------
// Generic drivers
// ---------------------------------------------------------------------------

/// One-shot local read: `Op::query` over `conn`. The search overlay's
/// debounced preview and the CLI's cached reads share this path.
pub fn load<Op: Query>(conn: &Connection, vars: &Op::Variables) -> Result<Op::Output> {
    Op::query(conn, vars)
}

/// Upstream refresh: fetch `Op` through `transport`, then fill its output
/// into the cache.
pub fn refresh<Op>(
    conn: &Connection,
    transport: &dyn GraphqlTransport,
    vars: Op::Variables,
) -> Result<()>
where
    Op: Fill,
    Op::Variables: Clone,
    Op::Output: TryFrom<Op, Error = anyhow::Error>,
{
    let out = execute::<Op>(transport, vars.clone())?;
    Op::fill(conn, &vars, &out)
}

/// How a composed view's one-shot freshness refresh at open
/// (docs/design/unified-execute-adr.md, "Decision 3") brings its operation up
/// to date from upstream. Distinct from [`Fill`], which only knows how to
/// write an already-fetched result into the cache: fetching needs
/// `lt-upstream`, which `lt-storage` does not depend on, so this lives here
/// rather than on the storage-side trait. Every operation [`crate::Runtime::refresh`]
/// can drive implements it: the generic single-page [`refresh`] driver for
/// most operations, [`IssueDetailQuery`]'s own impl (below) for its
/// fetch-to-exhaustion comment pagination.
pub trait Refresh: Fill {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<()>;
}

impl Refresh for IssuesQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<()> {
        refresh::<IssuesQuery>(conn, transport, vars)
    }
}

impl Refresh for TeamsQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<()> {
        refresh::<TeamsQuery>(conn, transport, vars)
    }
}

impl Refresh for TeamStatesQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<()> {
        refresh::<TeamStatesQuery>(conn, transport, vars)
    }
}

impl Refresh for TeamMembersQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<()> {
        refresh::<TeamMembersQuery>(conn, transport, vars)
    }
}

impl Refresh for IssueDetailQuery {
    /// One wire request for the issue plus its first page of
    /// comments/children, applied through [`IssueDetailQuery`]'s own
    /// replace-set comment semantics; then [`CommentsQuery`] pages the
    /// remainder of the comment thread to exhaustion -- multiple wire
    /// requests, one refresh call. Each later page appends rather than
    /// replacing ([`CommentsQuery`]'s own `Fill::fill`): a delete-first per
    /// page would wipe the previous page's inserts. Children stay a single
    /// first-page fetch (capped at 250 by the document itself).
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<()> {
        let out = execute::<IssueDetailQuery>(transport, vars.clone())?;
        let mut cursor = out.as_ref().and_then(|data| data.comments_cursor.clone());
        IssueDetailQuery::fill(conn, &vars, &out)?;

        while let Some(after) = cursor {
            let page_vars = CommentsVariables {
                id: vars.id.clone(),
                after: Some(after),
            };
            let page = execute::<CommentsQuery>(transport, page_vars.clone())?;
            cursor = page
                .page_info
                .has_next_page
                .then_some(page.page_info.end_cursor.clone())
                .flatten();
            CommentsQuery::fill(conn, &page_vars, &page)?;
        }

        Ok(())
    }
}

impl Refresh for NewIssueQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<()> {
        refresh::<NewIssueQuery>(conn, transport, vars)
    }
}

impl Refresh for ViewerQuery {
    fn refresh(
        conn: &Connection,
        transport: &dyn GraphqlTransport,
        vars: Self::Variables,
    ) -> Result<()> {
        refresh::<ViewerQuery>(conn, transport, vars)
    }
}

/// [`Runtime::execute`]'s dispatch, by operation kind
/// (docs/design/unified-execute-adr.md, "Decision 2"): a query op reads the
/// cache. `Query` and `Mutation` are disjoint traits, so this is hand-written
/// per operation rather than a blanket impl -- three lines choosing the seam,
/// an ENG-16 codegen target.
pub trait Operation: lt_types::graphql::GraphqlOperation {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output>;
}

/// Shared body for every query-kind [`Operation`] impl: a cache-first read
/// over a fresh connection.
fn query_execute<Op: Query>(runtime: &Runtime, vars: &Op::Variables) -> Result<Op::Output> {
    Op::query(&runtime.connect()?, vars)
}

impl Operation for IssuesQuery {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output> {
        query_execute::<Self>(runtime, &vars)
    }
}

impl Operation for TeamsQuery {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output> {
        query_execute::<Self>(runtime, &vars)
    }
}

impl Operation for TeamStatesQuery {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output> {
        query_execute::<Self>(runtime, &vars)
    }
}

impl Operation for TeamMembersQuery {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output> {
        query_execute::<Self>(runtime, &vars)
    }
}

impl Operation for NewIssueQuery {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output> {
        query_execute::<Self>(runtime, &vars)
    }
}

impl Operation for ViewerQuery {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output> {
        query_execute::<Self>(runtime, &vars)
    }
}

impl Operation for IssueDetailQuery {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output> {
        query_execute::<Self>(runtime, &vars)
    }
}

/// Shared body for every mutation-kind [`Operation`] impl: enqueue the
/// optimistic local write, emit `Update`, and nudge the loop to drain
/// promptly -- the write-side mirror of [`query_execute`]. Returns the id
/// the write was recorded under, so the caller can read the optimistic
/// entity back out of the cache.
fn mutation_execute<M: Mutation>(runtime: &Runtime, vars: M::Variables) -> Result<String> {
    let conn = runtime.connect()?;
    let id = M::enqueue(&conn, vars)?;
    runtime.emit_update();
    runtime.request_drain();
    Ok(id)
}

impl Operation for IssueCreateMutation {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output> {
        let id = mutation_execute::<Self>(runtime, vars)?;
        db::query_issue_by_id(&runtime.connect()?, &id)?
            .context("optimistic issue create vanished from the cache")
    }
}

impl Operation for IssueUpdateMutation {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output> {
        let id = mutation_execute::<Self>(runtime, vars)?;
        db::query_issue_by_id(&runtime.connect()?, &id)
    }
}

impl Operation for CommentCreateMutation {
    fn execute(runtime: &Runtime, vars: Self::Variables) -> Result<Self::Output> {
        let issue_id = vars.input.issue_id.clone();
        let id = mutation_execute::<Self>(runtime, vars)?;
        db::query_comments(&runtime.connect()?, &issue_id)?
            .into_iter()
            .find(|c| c.id.inner() == id)
            .context("optimistic comment create vanished from the cache")
    }
}

#[cfg(test)]
mod tests {
    use lt_storage::db;
    use lt_types::comments::Comment;
    use lt_types::detail::IssueDetailVariables;
    use lt_types::members::{TeamMembersQuery, TeamVariables as MembersTeamVariables};
    use lt_types::states::{
        AllWorkflowStatesVariables, TeamRef, TeamStatesQuery, TeamVariables as StatesTeamVariables,
        WorkflowStateWithTeam, WorkflowStateWithTeamConnection,
    };
    use lt_types::teams::TeamsQuery;
    use lt_types::types;
    use lt_upstream::client::FakeTransport;
    use serde_json::json;

    use super::*;

    fn conn() -> rusqlite::Connection {
        db::Database::memory().unwrap().connect().unwrap()
    }

    #[test]
    fn refresh_teams_upserts_the_fetched_set() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({
            "teams": { "nodes": [
                { "id": "t1", "name": "Eng" },
                { "id": "t2", "name": "Design" }
            ] }
        })]);
        refresh::<TeamsQuery>(&conn, &transport, ()).unwrap();
        let teams = db::query_teams(&conn).unwrap();
        assert_eq!(
            teams.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["Design", "Eng"]
        );
    }

    #[test]
    fn refresh_teams_missing_data_returns_error() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({})]);
        assert!(refresh::<TeamsQuery>(&conn, &transport, ()).is_err());
    }

    #[test]
    fn teams_query_fill_writes_teams() {
        let conn = conn();
        let teams = TeamConnection {
            nodes: vec![types::Team {
                id: "t1".into(),
                name: "Eng".to_string(),
            }],
        };
        TeamsQuery::fill(&conn, &(), &teams).unwrap();
        assert_eq!(db::query_teams(&conn).unwrap()[0].name, "Eng");
    }

    #[test]
    fn refresh_team_states_and_members_writes_positions_and_memberships() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({ "team": { "states": { "nodes": [
            { "id": "s1", "name": "Todo", "position": 1.0 },
            { "id": "s2", "name": "Done", "position": 2.0 }
        ] } } })]);
        refresh::<TeamStatesQuery>(
            &conn,
            &transport,
            StatesTeamVariables {
                team_id: "t1".to_string(),
            },
        )
        .unwrap();

        let transport = FakeTransport::new(vec![json!({ "team": { "members": { "nodes": [
            { "id": "u1", "name": "Ada" },
            { "id": "u2", "name": "Grace" }
        ] } } })]);
        refresh::<TeamMembersQuery>(
            &conn,
            &transport,
            MembersTeamVariables {
                team_id: "t1".to_string(),
            },
        )
        .unwrap();

        let states = db::query_team_states(&conn, "t1").unwrap();
        assert_eq!(
            states.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["Todo", "Done"]
        );
        let members = db::query_team_members(&conn, "t1").unwrap();
        assert_eq!(
            members.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
            ["Ada", "Grace"]
        );
    }

    #[test]
    fn team_states_query_read_preserves_position_order() {
        let conn = conn();
        db::upsert_team_state(
            &conn,
            "t1",
            &WorkflowState {
                id: "s-todo".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        db::upsert_team_state(
            &conn,
            "t1",
            &WorkflowState {
                id: "s-zeta".into(),
                name: "Zeta".to_string(),
                position: 2.0,
            },
        )
        .unwrap();

        let vars = StatesTeamVariables {
            team_id: "t1".to_string(),
        };
        let states = TeamStatesQuery::query(&conn, &vars).unwrap();
        assert_eq!(
            states
                .nodes
                .iter()
                .map(|s| (s.name.as_str(), s.position))
                .collect::<Vec<_>>(),
            [("Todo", 1.0), ("Zeta", 2.0)]
        );
    }

    #[test]
    fn all_workflow_states_query_fill_scopes_each_state_to_its_own_team() {
        let conn = conn();
        let out = WorkflowStateWithTeamConnection {
            nodes: vec![
                WorkflowStateWithTeam {
                    id: "s1".into(),
                    name: "Todo".to_string(),
                    position: 1.0,
                    team: TeamRef { id: "t1".into() },
                },
                WorkflowStateWithTeam {
                    id: "s2".into(),
                    name: "Done".to_string(),
                    position: 2.0,
                    team: TeamRef { id: "t1".into() },
                },
                WorkflowStateWithTeam {
                    id: "s3".into(),
                    name: "Backlog".to_string(),
                    position: 1.0,
                    team: TeamRef { id: "t2".into() },
                },
            ],
            page_info: lt_types::pagination::PageInfo::default(),
        };

        let vars = AllWorkflowStatesVariables {
            first: 250,
            after: None,
        };
        AllWorkflowStatesQuery::fill(&conn, &vars, &out).unwrap();

        let t1_states = db::query_team_states(&conn, "t1").unwrap();
        assert_eq!(
            t1_states
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>(),
            ["Todo", "Done"]
        );
        let t2_states = db::query_team_states(&conn, "t2").unwrap();
        assert_eq!(t2_states[0].name, "Backlog");
    }

    #[test]
    fn refresh_team_members_replaces_stale_memberships() {
        let conn = conn();
        let vars = MembersTeamVariables {
            team_id: "t1".to_string(),
        };
        let transport = FakeTransport::new(vec![json!({ "team": { "members": { "nodes": [
            { "id": "u1", "name": "Ada" }, { "id": "u2", "name": "Grace" }
        ] } } })]);
        refresh::<TeamMembersQuery>(&conn, &transport, vars.clone()).unwrap();

        let transport = FakeTransport::new(vec![json!({ "team": { "members": { "nodes": [
            { "id": "u1", "name": "Ada" }
        ] } } })]);
        refresh::<TeamMembersQuery>(&conn, &transport, vars).unwrap();

        let members = db::query_team_members(&conn, "t1").unwrap();
        assert_eq!(
            members.iter().map(|u| u.name.as_str()).collect::<Vec<_>>(),
            ["Ada"]
        );
    }

    fn new_issue_vars(team_id: Option<&str>) -> lt_types::new_issue::NewIssueVariables {
        lt_types::new_issue::NewIssueVariables::new(team_id.map(str::to_string))
    }

    #[test]
    fn new_issue_query_read_without_a_team_leaves_states_and_members_empty() {
        let conn = conn();
        db::upsert_teams(
            &conn,
            &[types::Team {
                id: "t1".into(),
                name: "Eng".to_string(),
            }],
        )
        .unwrap();

        let data = NewIssueQuery::query(&conn, &new_issue_vars(None)).unwrap();
        assert_eq!(data.teams.len(), 1);
        assert!(data.states.is_empty());
        assert!(data.members.is_empty());
        assert!(data.viewer.is_none());
    }

    #[test]
    fn new_issue_query_read_with_a_team_includes_its_states_and_members() {
        let conn = conn();
        db::upsert_team_state(
            &conn,
            "t1",
            &WorkflowState {
                id: "s1".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        db::upsert_users(
            &conn,
            &[types::User {
                id: "u1".into(),
                name: "Ada".to_string(),
            }],
        )
        .unwrap();
        db::replace_team_memberships(&conn, "t1", &["u1"]).unwrap();

        let data = NewIssueQuery::query(&conn, &new_issue_vars(Some("t1"))).unwrap();
        assert_eq!(data.states.len(), 1);
        assert_eq!(data.members.len(), 1);
    }

    #[test]
    fn new_issue_query_fill_writes_teams_and_team_scoped_data() {
        let conn = conn();
        let vars = new_issue_vars(Some("t1"));
        let out = NewIssueData {
            teams: vec![types::Team {
                id: "t1".into(),
                name: "Eng".to_string(),
            }],
            states: vec![WorkflowState {
                id: "s1".into(),
                name: "Todo".to_string(),
                position: 1.0,
            }],
            members: vec![types::User {
                id: "u1".into(),
                name: "Ada".to_string(),
            }],
            viewer: None,
        };
        NewIssueQuery::fill(&conn, &vars, &out).unwrap();
        assert_eq!(db::query_teams(&conn).unwrap()[0].name, "Eng");
        assert_eq!(db::query_team_states(&conn, "t1").unwrap()[0].name, "Todo");
        assert_eq!(db::query_team_members(&conn, "t1").unwrap()[0].name, "Ada");
    }

    #[test]
    fn new_issue_query_fill_without_a_team_only_writes_teams() {
        let conn = conn();
        let vars = new_issue_vars(None);
        let out = NewIssueData {
            teams: vec![types::Team {
                id: "t1".into(),
                name: "Eng".to_string(),
            }],
            states: Vec::new(),
            members: Vec::new(),
            viewer: None,
        };
        NewIssueQuery::fill(&conn, &vars, &out).unwrap();
        assert_eq!(db::query_teams(&conn).unwrap()[0].name, "Eng");
        assert!(db::query_team_states(&conn, "t1").unwrap().is_empty());
        assert!(db::query_team_members(&conn, "t1").unwrap().is_empty());
    }

    fn comment_node(id: &str, created_at: &str) -> serde_json::Value {
        json!({
            "id": id, "body": "b",
            "createdAt": created_at, "updatedAt": created_at,
            "user": { "id": "u1", "name": "Alice" },
            "issueId": "i1"
        })
    }

    /// A `comments` connection page: `has_next`/`cursor` drive the refresh's
    /// pagination loop.
    fn comments_page(
        comments: &[serde_json::Value],
        has_next: bool,
        cursor: Option<&str>,
    ) -> serde_json::Value {
        json!({
            "nodes": comments,
            "pageInfo": { "hasNextPage": has_next, "endCursor": cursor }
        })
    }

    /// A composed `IssueDetailQuery` wire response envelope: the shared issue
    /// fixture plus its comments/children connections.
    fn issue_detail_response(
        id: &str,
        comments_page: serde_json::Value,
        children: &[serde_json::Value],
    ) -> serde_json::Value {
        let mut issue = lt_types::issues::sample_issue_node(id);
        issue["comments"] = comments_page;
        issue["children"] = json!({
            "nodes": children,
            "pageInfo": { "hasNextPage": false, "endCursor": null }
        });
        json!({ "issue": issue })
    }

    fn comments_page_response(
        comments: &[serde_json::Value],
        has_next: bool,
        cursor: Option<&str>,
    ) -> serde_json::Value {
        json!({ "issue": { "comments": comments_page(comments, has_next, cursor) }})
    }

    fn detail_vars(id: &str) -> IssueDetailVariables {
        IssueDetailVariables { id: id.to_string() }
    }

    #[test]
    fn read_is_none_for_a_locally_absent_issue() {
        let conn = conn();
        assert!(
            IssueDetailQuery::query(&conn, &detail_vars("missing"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn read_joins_issue_comments_and_children() {
        let conn = conn();
        db::upsert_team_state(
            &conn,
            "ENG",
            &WorkflowState {
                id: "s-todo".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        let parent = lt_storage::db::op_log::sample_base_issue("1");
        let mut child = lt_storage::db::op_log::sample_base_issue("2");
        child.parent = Some(lt_types::types::Parent {
            id: "1".into(),
            identifier: "ENG-1".to_string(),
        });
        db::upsert_issues(&conn, &[parent, child]).unwrap();
        db::upsert_comments(
            &conn,
            &[Comment {
                id: "c1".into(),
                body: "hi".to_string(),
                created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                user: None,
                issue_id: Some("1".to_string()),
            }],
        )
        .unwrap();

        let data = IssueDetailQuery::query(&conn, &detail_vars("1"))
            .unwrap()
            .unwrap();
        assert_eq!(data.issue.identifier, "ENG-1");
        assert_eq!(data.comments.len(), 1);
        assert_eq!(data.children.len(), 1);
        assert_eq!(data.children[0].identifier, "ENG-2");
    }

    #[test]
    fn fill_of_none_is_a_noop() {
        let conn = conn();
        IssueDetailQuery::fill(&conn, &detail_vars("1"), &None).unwrap();
        assert!(db::query_issue_by_id(&conn, "1").unwrap().is_none());
    }

    #[test]
    fn fill_writes_issue_children_and_comments() {
        let conn = conn();
        db::upsert_team_state(
            &conn,
            "ENG",
            &WorkflowState {
                id: "s-todo".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        let data = IssueDetailData {
            issue: lt_storage::db::op_log::sample_base_issue("1"),
            comments: vec![Comment {
                id: "c1".into(),
                body: "hi".to_string(),
                created_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                updated_at: "2026-01-01T00:00:00Z".parse().unwrap(),
                user: None,
                issue_id: Some("1".to_string()),
            }],
            children: vec![lt_storage::db::op_log::sample_base_issue("2")],
            comments_cursor: None,
        };
        IssueDetailQuery::fill(&conn, &detail_vars("1"), &Some(data)).unwrap();
        assert!(db::query_issue_by_id(&conn, "1").unwrap().is_some());
        assert!(db::query_issue_by_id(&conn, "2").unwrap().is_some());
        assert_eq!(db::query_comments(&conn, "1").unwrap().len(), 1);
    }

    #[test]
    fn refresh_writes_the_issue_children_and_the_first_comment_page() {
        let conn = conn();
        // `sample_issue_node`'s state must already be locally known (sync owns
        // workflow states; issue upserts never write them).
        db::upsert_team_state(
            &conn,
            "ENG",
            &lt_types::types::WorkflowState {
                id: "s".into(),
                name: "Todo".to_string(),
                position: 1.0,
            },
        )
        .unwrap();
        db::upsert_comments(
            &conn,
            &[lt_types::comments::Comment {
                id: "old".into(),
                body: "stale".to_string(),
                created_at: "2025-01-01T00:00:00Z".parse().unwrap(),
                updated_at: "2025-01-01T00:00:00Z".parse().unwrap(),
                user: None,
                issue_id: Some("i1".to_string()),
            }],
        )
        .unwrap();

        let transport = FakeTransport::new(vec![issue_detail_response(
            "i1",
            comments_page(&[comment_node("c1", "2026-01-01T00:00:00Z")], false, None),
            &[lt_types::issues::sample_issue_node("child-1")],
        )]);

        IssueDetailQuery::refresh(&conn, &transport, detail_vars("i1")).unwrap();

        assert!(db::query_issue_by_id(&conn, "i1").unwrap().is_some());
        assert!(db::query_issue_by_id(&conn, "child-1").unwrap().is_some());
        let rows = db::query_comments(&conn, "i1").unwrap();
        assert_eq!(
            rows.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1"]
        );
        assert_eq!(
            transport.calls.borrow().len(),
            1,
            "no page had a next cursor"
        );
    }

    #[test]
    fn refresh_appends_paginated_comment_pages_to_the_first_page() {
        let conn = conn();
        let transport = FakeTransport::new(vec![
            // The composed document's own first page, with more to come.
            issue_detail_response(
                "i1",
                comments_page(
                    &[comment_node("c1", "2026-01-01T00:00:00Z")],
                    true,
                    Some("cur1"),
                ),
                &[],
            ),
            comments_page_response(
                &[comment_node("c2", "2026-01-02T00:00:00Z")],
                true,
                Some("cur2"),
            ),
            comments_page_response(&[comment_node("c3", "2026-01-03T00:00:00Z")], false, None),
        ]);

        IssueDetailQuery::refresh(&conn, &transport, detail_vars("i1")).unwrap();

        let rows = db::query_comments(&conn, "i1").unwrap();
        assert_eq!(
            rows.iter().map(|c| c.id.inner()).collect::<Vec<_>>(),
            ["c1", "c2", "c3"]
        );
        assert_eq!(transport.variables(1)["after"], json!("cur1"));
        assert_eq!(transport.variables(2)["after"], json!("cur2"));
    }

    #[test]
    fn refresh_wire_decode_error_propagates() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({ "issue": null })]);
        assert!(IssueDetailQuery::refresh(&conn, &transport, detail_vars("i1")).is_err());
    }

    #[test]
    fn refresh_new_issue_upserts_teams_and_team_scoped_data() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({
            "teams": { "nodes": [{ "id": "t1", "name": "Eng" }] },
            "team": {
                "states": { "nodes": [{ "id": "s1", "name": "Todo", "position": 1.0 }] },
                "members": { "nodes": [{ "id": "u1", "name": "Ada" }] }
            }
        })]);

        refresh::<NewIssueQuery>(
            &conn,
            &transport,
            lt_types::new_issue::NewIssueVariables::new(Some("t1".to_string())),
        )
        .unwrap();

        assert_eq!(db::query_teams(&conn).unwrap()[0].name, "Eng");
        assert_eq!(db::query_team_members(&conn, "t1").unwrap()[0].name, "Ada");
    }

    #[test]
    fn viewer_query_apply_of_none_is_a_noop() {
        let conn = conn();
        ViewerQuery::fill(&conn, &(), &None).unwrap();
        assert!(ViewerQuery::query(&conn, &()).unwrap().is_none());
    }

    #[test]
    fn refresh_viewer_persists_and_reports_viewer() {
        let conn = conn();
        let transport = FakeTransport::new(vec![json!({
            "viewer": { "id": "u1", "name": "Ada", "organization": { "id": "o1", "name": "Acme", "urlKey": "acme" } }
        })]);

        refresh::<ViewerQuery>(&conn, &transport, ()).unwrap();

        assert_eq!(db::viewer(&conn).unwrap().unwrap().user.name, "Ada");
    }
}
