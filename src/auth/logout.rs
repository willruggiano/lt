use anyhow::Result;

use crate::config;

pub fn run() -> Result<()> {
    match config::load_token()? {
        None => println!("Not logged in."),
        Some(_) => {
            config::remove_token()?;
            println!("Logged out.");
        }
    }
    Ok(())
}
