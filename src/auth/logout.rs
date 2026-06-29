use std::io::Write;

use anyhow::Result;

use crate::config;

pub fn run(out: &mut dyn Write) -> Result<()> {
    if config::load_token()?.is_none() {
        writeln!(out, "Not logged in.")?;
    } else {
        config::remove_token()?;
        writeln!(out, "Logged out.")?;
    }
    Ok(())
}
