use anyhow::Result;

/// Remove the stored auth token, if any (the `lt auth logout` data path).
/// Returns `true` if a token was present and removed, `false` if the caller
/// was already logged out. Presentation (`Logged out.` / `Not logged in.`)
/// lives in the CLI layer.
pub fn run() -> Result<bool> {
    if lt_config::load_token()?.is_none() {
        return Ok(false);
    }
    lt_config::remove_token()?;
    Ok(true)
}
