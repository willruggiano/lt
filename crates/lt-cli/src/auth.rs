//! The `lt auth` command surface. The OAuth/login/token logic lives in
//! `lt-sync`; this is only the clap dispatch.

use std::io::Write;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum AuthCommands {
    /// Log in to Linear via `OAuth2`
    Login,
    /// Show the currently authenticated identity
    Status,
    /// Log out and remove local credentials
    Logout,
}

pub fn run(out: &mut dyn Write, cmd: &AuthCommands) -> Result<()> {
    match cmd {
        AuthCommands::Login => lt_sync::auth::run_login(),
        AuthCommands::Status => lt_sync::auth::run_status(out),
        AuthCommands::Logout => lt_sync::auth::run_logout(out),
    }
}
