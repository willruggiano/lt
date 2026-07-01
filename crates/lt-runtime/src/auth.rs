//! Auth command entry points. The OAuth/login/token logic lives in
//! `lt-upstream`; the runtime re-exports the command surface so `lt-cli` drives
//! auth without naming `lt-upstream`.

/// Interactive `OAuth2` login (the `lt auth login` path).
pub use lt_upstream::auth::login;
/// Log out and remove local credentials (`lt auth logout`).
pub use lt_upstream::auth::logout;
/// Show the currently authenticated identity (`lt auth status`).
pub use lt_upstream::auth::status;
