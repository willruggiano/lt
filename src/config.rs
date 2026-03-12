use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthToken {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: Option<u64>,
    pub scope: Option<String>,
    /// Unix timestamp (seconds) at which this token was saved.
    /// Absent in tokens saved before this field was introduced.
    #[serde(default)]
    pub issued_at: Option<u64>,
}

impl AuthToken {
    /// Return true if the token is known to have expired.
    ///
    /// Returns false when `expires_in` or `issued_at` is absent, so that
    /// tokens from older saves or tokens without an expiry are always
    /// considered valid (we let the API reject them if they are actually bad).
    pub fn is_expired(&self) -> bool {
        let (Some(expires_in), Some(issued_at)) = (self.expires_in, self.issued_at) else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now >= issued_at + expires_in
    }
}

pub fn config_dir() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .ok_or_else(|| anyhow::anyhow!("could not determine config directory"))?
        .join("lt");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating config directory {}", dir.display()))?;
    Ok(dir)
}

pub fn log_dir() -> Result<PathBuf> {
    let dir = state_dir()?.join("logs");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating log directory {}", dir.display()))?;
    Ok(dir)
}

pub fn state_dir() -> Result<PathBuf> {
    let dir = dirs::state_dir()
        .ok_or_else(|| anyhow::anyhow!("could not determine state directory"))?
        .join("lt");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating state directory {}", dir.display()))?;
    Ok(dir)
}

fn token_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("auth.json"))
}

fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.json"))
}

pub fn load_token() -> Result<Option<AuthToken>> {
    let path = token_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let data =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(Some(serde_json::from_str(&data)?))
}

pub fn save_token(token: &AuthToken) -> Result<()> {
    let path = token_path()?;
    // Stamp issued_at so expiry checks work after the token is reloaded.
    // Preserve any issued_at already set by the caller.
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stamped = AuthToken {
        access_token: token.access_token.clone(),
        token_type: token.token_type.clone(),
        expires_in: token.expires_in,
        scope: token.scope.clone(),
        issued_at: Some(token.issued_at.unwrap_or(now_secs)),
    };
    let data = serde_json::to_string_pretty(&stamped)?;
    write_private_file(&path, &data).with_context(|| format!("writing {}", path.display()))
}

pub fn remove_token() -> Result<()> {
    let path = token_path()?;
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(())
}

pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    let data =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(serde_json::from_str(&data)?)
}

/// Write a file with mode 0600 on Unix so credentials are not world-readable.
fn write_private_file(path: &PathBuf, content: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(content.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)?;
    }
    Ok(())
}
