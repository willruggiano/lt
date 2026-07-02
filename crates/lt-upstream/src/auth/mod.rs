//! The auth domain: OAuth2 login/status/logout plus token refresh.
//!
//! The command entrypoints are re-exported straight from their submodules —
//! `upstream::auth::login()`, `::viewer_from_config()`, `::logout()` — rather
//! than wrapped.

mod login;
mod logout;
pub mod refresh;
mod status;

/// Interactive `OAuth2` login (the `lt auth login` path).
pub use login::run as login;
/// Non-interactive `OAuth2` login -- errors instead of prompting when
/// credentials are missing. Safe to call from a background thread (the TUI
/// re-auth path, bd-vhp).
pub use login::run_non_interactive as login_non_interactive;
/// Remove the stored auth token (the `lt auth logout` data path); printing
/// lives in `lt-cli`.
pub use logout::run as logout;
/// Fetch the currently authenticated identity (the `lt auth status` data
/// path); printing lives in `lt-cli`.
pub use status::viewer_from_config;
