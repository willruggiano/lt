//! The `Read`/`Upsert` seam over [`GraphqlOperation`]: the local anchor that
//! makes the operation type the sole vocabulary of both sides of the cache
//! (docs/design/operation-seam-adr.md, "Decision 1"). Impls live beside the
//! statement registry they call, in the module that owns the entity
//! (`db::issues`, `db::teams`, `db::comments`); SQL text stays crate-private
//! per docs/design/type-safe-sql-adr.md.

use anyhow::Result;
use lt_types::graphql::GraphqlOperation;
use rusqlite::Connection;

/// A normalized cache table (plus the owning id where one exists), reported by
/// an upsert and matched by a read's [`Read::reads`] predicate
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
pub trait Read: GraphqlOperation {
    fn read(conn: &Connection, vars: &Self::Variables) -> Result<Self::Output>;

    /// The entity slices this operation's result depends on, concrete for
    /// `vars`. Over-approximation is safe: a spurious re-read is an
    /// idempotent projection of current truth.
    fn reads(vars: &Self::Variables) -> Vec<EntityKey>;
}

/// Writing an operation's fetched output into the cache.
pub trait Upsert: GraphqlOperation {
    /// Write `out` into the cache and report every entity slice touched.
    fn upsert(
        conn: &Connection,
        vars: &Self::Variables,
        out: &Self::Output,
    ) -> Result<Vec<EntityKey>>;
}

/// The drainer's ack context: the outbox row's own identity (`seq`,
/// `entity_id`, as recorded by [`Mutate::enqueue`]) and the variables it
/// replayed, grouped so [`Mutate::ack`] stays under the argument-count lint.
pub struct AckContext<'a, V> {
    pub seq: i64,
    pub entity_id: &'a str,
    pub vars: &'a V,
}

/// A local write: the outbox's mutation-side vocabulary, mirroring
/// `Read`/`Upsert` on the query side (docs/design/operation-seam-adr.md,
/// Non-goals: "Mutations" -- this trait systematizes that binding). The
/// mutation's own wire name (`GraphqlOperation::NAME`) is the outbox's
/// `op_type` discriminator, so no parallel constant exists.
pub trait Mutate: GraphqlOperation {
    /// Write the operation's local optimistic effect (a `pending_overlay`
    /// row, an optimistic temp row, a local comment row, ...) and enqueue its
    /// outbox command from `vars`, atomically. Returns the entity keys
    /// touched.
    fn enqueue(conn: &Connection, vars: Self::Variables) -> Result<Vec<EntityKey>>;

    /// Reconcile the base and retire the command's local effect once the
    /// drainer has `out`, the mutation's decoded response.
    fn ack(
        conn: &Connection,
        ctx: AckContext<'_, Self::Variables>,
        out: Self::Output,
    ) -> Result<Vec<EntityKey>>;
}
