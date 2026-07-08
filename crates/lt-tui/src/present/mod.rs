//! Per-entity presentation: thin local wrappers around `lt-upstream` entities,
//! implementing (or producing) the ratatui widgets that display them. The
//! orphan rule bars `impl From<&Issue> for Row` (both foreign to `lt-tui`),
//! and a ratatui dependency in `lt-upstream` would point the wrong way; these
//! wrappers keep presentation logic beside the entity it renders instead
//! (docs/design/operation-seam-adr.md, Decision 9).

pub(crate) mod comment;
pub(crate) mod issue;
