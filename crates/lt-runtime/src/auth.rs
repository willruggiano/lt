//! Auth command entry points. The OAuth/login/token logic lives in
//! `lt-upstream`; the runtime re-exports the command surface so `lt-cli` drives
//! auth without naming `lt-upstream`.

pub use lt_upstream::auth::{login, logout, viewer_from_config};
