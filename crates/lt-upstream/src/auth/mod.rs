//! The auth domain: OAuth2 login/status/logout plus token refresh.
//!
//! The command entrypoints are re-exported straight from their submodules —
//! `upstream::auth::login()`, `::status()`, `::logout()` — rather than wrapped.

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
/// Log out and remove local credentials (`lt auth logout`).
pub use logout::run as logout;
/// Show the currently authenticated identity (`lt auth status`).
pub use status::run as status;
