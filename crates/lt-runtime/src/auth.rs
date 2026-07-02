//! Auth command entry points. The OAuth/login/token logic lives in
//! `lt-upstream`; the runtime re-exports the command surface so `lt-cli` drives
//! auth without naming `lt-upstream`.

/// Interactive `OAuth2` login (the `lt auth login` path).
pub use lt_upstream::auth::login;
/// Remove the stored auth token (the `lt auth logout` data path); printing
/// lives in `lt-cli`.
pub use lt_upstream::auth::logout;
/// Fetch the currently authenticated identity (the `lt auth status` data
/// path); printing lives in `lt-cli`.
pub use lt_upstream::auth::viewer_from_config;
