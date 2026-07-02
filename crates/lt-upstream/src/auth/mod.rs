//! The auth domain: OAuth2 login/status/logout plus token refresh.
//!
//! The command entrypoints are re-exported straight from their submodules —
//! `upstream::auth::login()`, `::viewer_from_config()`, `::logout()` — rather
//! than wrapped.

mod login;
mod logout;
pub mod refresh;
mod status;

pub use login::{run as login, run_non_interactive as login_non_interactive};
pub use logout::run as logout;
pub use status::viewer_from_config;
