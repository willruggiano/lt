//! The `Query`/`Mutation` seam over [`GraphqlOperation`]: the local anchor
//! that makes the operation type the sole vocabulary of both sides of the
//! cache (docs/design/unified-execute-adr.md, "Decision 2"). Impls live
//! beside the statement registry they call, in the module that owns the
//! entity (`db::issues`, `db::teams`, `db::comments`); SQL text stays
//! crate-private per docs/design/type-safe-sql-adr.md.

use anyhow::Result;
use lt_types::graphql::GraphqlOperation;
use rusqlite::Connection;

/// A normalized cache table (plus the owning id where one exists), reported by
/// a mutation and matched by a read's [`Query::reads`] predicate
/// (docs/design/operation-seam-adr.md, "Decision 5").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntityKey {
    Issue,
    Comment { issue_id: String },
    Teams,
    WorkflowStates { team_id: String },
    TeamMemberships { team_id: String },
    Viewer,
}

/// A local, cache-backed read of an operation's result.
pub trait Query: GraphqlOperation {
    fn query(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output>;

    /// The entity slices this operation's result depends on, concrete for
    /// `vars`. Over-approximation is safe: a spurious re-read is an
    /// idempotent projection of current truth.
    fn reads(vars: &Self::Variables) -> Vec<EntityKey>;
}

/// The drainer's ack context: the outbox row's own identity (`seq`,
/// `entity_id`, as recorded by [`Mutation::enqueue`]) and the variables it
/// replayed, grouped so [`Mutation::ack`] stays under the argument-count lint.
pub struct AckContext<'a, V> {
    pub seq: i64,
    pub entity_id: &'a str,
    pub vars: &'a V,
}

/// The result of [`Mutation::enqueue`]: the id its optimistic local write was
/// recorded under (`vars.id` for an update, the freshly minted temp id for a
/// create) plus every entity slice it touched. The id lets a caller
/// (`Runtime::execute`, docs/design/unified-execute-adr.md "Decision 1") read
/// the optimistic entity straight back out of the cache.
pub struct Enqueued {
    pub entity_id: String,
    pub touched: Vec<EntityKey>,
}

/// Every local write into the replica: applying an already-fetched operation
/// response, and the outbox's mutation-side vocabulary -- the optimistic
/// local write plus its enqueue and ack (docs/design/unified-execute-adr.md,
/// "Decision 2"; the mutation's own wire name, `GraphqlOperation::NAME`, is
/// the outbox's `op_type` discriminator, so no parallel constant exists). A
/// single operation type uses only the methods its kind needs; the others
/// keep their "unsupported" default.
pub trait Mutation: GraphqlOperation {
    /// Write an already-fetched response into the cache and report every
    /// entity slice touched. Only the query-kind operations (fetched via
    /// `client::execute` and applied here) override this.
    fn apply(
        _conn: &Connection,
        _vars: &Self::Variables,
        _out: &Self::Output,
    ) -> Result<Vec<EntityKey>> {
        anyhow::bail!("{} has no fetched-response cache write", Self::NAME)
    }

    /// Write the operation's local optimistic effect (a `pending_overlay`
    /// row, an optimistic temp row, a local comment row, ...) and enqueue its
    /// outbox command from `vars`, atomically. Returns the id it wrote and
    /// the entity keys touched. Only the outbox mutations override this.
    fn enqueue(_conn: &Connection, _vars: Self::Variables) -> Result<Enqueued> {
        anyhow::bail!("{} is not an outbox mutation", Self::NAME)
    }

    /// Reconcile the base and retire the command's local effect once the
    /// drainer has `out`, the mutation's decoded response. Only the outbox
    /// mutations override this.
    fn ack(
        _conn: &Connection,
        _ctx: AckContext<'_, Self::Variables>,
        _out: Self::Output,
    ) -> Result<Vec<EntityKey>> {
        anyhow::bail!("{} is not an outbox mutation", Self::NAME)
    }
}
