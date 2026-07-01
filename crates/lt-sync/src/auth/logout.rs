use std::io::Write;

use anyhow::Result;

pub fn run(out: &mut dyn Write) -> Result<()> {
    if lt_config::load_token()?.is_none() {
        writeln!(out, "Not logged in.")?;
    } else {
        lt_config::remove_token()?;
        writeln!(out, "Logged out.")?;
    }
    Ok(())
}
