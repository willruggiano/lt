use anyhow::Result;

use crate::config;

pub fn run() -> Result<()> {
    if let None = config::load_token()? { println!("Not logged in.") } else {
        config::remove_token()?;
        println!("Logged out.");
    }
    Ok(())
}
