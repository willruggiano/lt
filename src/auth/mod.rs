mod login;
mod logout;
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
