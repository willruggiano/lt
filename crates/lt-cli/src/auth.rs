//! The `lt auth` command surface. The OAuth/login/token logic lives in
//! `lt-upstream` behind the runtime; this is only the clap dispatch.

use std::io::Write;

use anyhow::Result;
use clap::Subcommand;
use lt_runtime::auth;

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
        AuthCommands::Login => auth::login(),
        AuthCommands::Status => auth::status(out),
        AuthCommands::Logout => auth::logout(out),
    }
}
