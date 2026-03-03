mod login;
mod logout;
pub mod refresh;
mod status;

use anyhow::Result;
use clap::Subcommand;

#[derive(Subcommand)]
pub enum AuthCommands {
    /// Log in to Linear via OAuth2
    Login,
    /// Show the currently authenticated identity
    Status,
    /// Log out and remove local credentials
    Logout,
}

pub fn run(cmd: AuthCommands) -> Result<()> {
    match cmd {
        AuthCommands::Login => login::run(),
        AuthCommands::Status => status::run(),
        AuthCommands::Logout => logout::run(),
    }
}

/// Non-interactive OAuth2 login -- errors instead of prompting when
/// credentials are missing. Safe to call from a background thread
/// (used by the TUI re-auth path, bd-vhp).
pub fn login_non_interactive() -> Result<()> {
    login::run_non_interactive()
}
