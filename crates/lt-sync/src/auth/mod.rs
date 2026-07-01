mod login;
mod logout;
pub mod refresh;
mod status;

use std::io::Write;

use anyhow::Result;

/// Interactive `OAuth2` login (the `lt auth login` path).
pub fn run_login() -> Result<()> {
    login::run()
}

/// Show the currently authenticated identity (`lt auth status`).
pub fn run_status(out: &mut dyn Write) -> Result<()> {
    status::run(out)
}

/// Log out and remove local credentials (`lt auth logout`).
pub fn run_logout(out: &mut dyn Write) -> Result<()> {
    logout::run(out)
}

/// Non-interactive `OAuth2` login -- errors instead of prompting when
/// credentials are missing. Safe to call from a background thread
/// (used by the TUI re-auth path, bd-vhp).
pub fn login_non_interactive() -> Result<()> {
    login::run_non_interactive()
}
