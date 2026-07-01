//! Sync command entry points: the full/delta/probe run functions the CLI's
//! `lt sync` dispatch drives. The engine lives in [`crate::sync`]; this is the
//! command-facing surface.

pub use crate::sync::{delta, full, probe};
