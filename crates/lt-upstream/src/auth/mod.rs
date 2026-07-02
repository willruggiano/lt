//! The auth domain: OAuth2 login/status/logout plus token refresh.

mod login;
mod logout;
pub mod refresh;
mod status;

pub use login::{run as login, run_non_interactive as login_non_interactive};
pub use logout::run as logout;
pub use status::viewer_from_config;
