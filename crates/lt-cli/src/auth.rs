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
        AuthCommands::Status => print_status(out),
        AuthCommands::Logout => print_logout(out),
    }
}

/// Print the currently authenticated identity (the `lt auth status` output).
fn print_status(out: &mut dyn Write) -> Result<()> {
    let viewer = auth::viewer_from_config()?;
    writeln!(out, "user:         {}", viewer.user.name)?;
    writeln!(out, "id:           {}", viewer.user.id.inner())?;
    writeln!(
        out,
        "organization: {} ({})",
        viewer.organization.name, viewer.organization.url_key
    )?;
    Ok(())
}

/// Print the result of removing local credentials (the `lt auth logout`
/// output).
fn print_logout(out: &mut dyn Write) -> Result<()> {
    if auth::logout()? {
        writeln!(out, "Logged out.")?;
    } else {
        writeln!(out, "Not logged in.")?;
    }
    Ok(())
}
