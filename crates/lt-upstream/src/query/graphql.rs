//! The `GraphqlOperation` trait: pairs a cynic-built query string with its
//! typed variables and the decoded response envelope, so `lt-upstream`'s
//! generic `execute` can run and decode any operation through one code path.
//! The domain-level output is recomposed from the envelope by an `impl
//! TryFrom<Op> for Op::Output`, beside each operation.

use serde::Serialize;
use serde::de::DeserializeOwned;

/// One GraphQL operation: its wire variables, the decoded response envelope
/// (`Self`), and the domain-level output recomposed from it (an `impl
/// TryFrom<Self> for Self::Output`, beside the operation).
pub trait GraphqlOperation: DeserializeOwned + Sized {
    type Variables: Serialize;
    type Output;
    /// Operation name for error context ("issueCreate").
    const NAME: &'static str;
    /// Pair the query string with its typed variables via cynic.
    fn operation(variables: Self::Variables) -> cynic::Operation<Self, Self::Variables>;
}

/// Gate a mutation payload on its success flag.
pub(crate) fn ensure_success(op: &str, success: bool) -> anyhow::Result<()> {
    if !success {
        anyhow::bail!("{op} returned success=false");
    }
    Ok(())
}

/// Gate a mutation payload on its success flag, then return `value` as the
/// extracted output. Shared by every `*Payload { success, <entity> }`
/// mutation whose entity is already the correct `Output` type (unlike
/// `issueCreate`, whose entity is optional and needs its own "no entity"
/// error on success).
pub(crate) fn extract_on_success<T>(op: &str, success: bool, value: T) -> anyhow::Result<T> {
    ensure_success(op, success)?;
    Ok(value)
}
