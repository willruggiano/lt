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

    /// Does this operation's result depend on `key`? Over-approximation is
    /// safe: a spurious re-read is an idempotent projection of current truth.
    fn reads(vars: &Self::Variables, key: &EntityKey) -> bool;
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
