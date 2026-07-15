use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

static WORKSPACE: OnceLock<String> = OnceLock::new();

/// Select the active workspace for this process.  Must be called once at
/// startup, before any path helper is used. `None` selects the workspace
/// named "default".
pub fn set_workspace(workspace: Option<String>) -> Result<()> {
    if let Some(ref n) = workspace
        && (n.is_empty()
            || !n
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        anyhow::bail!("invalid workspace name {n:?}: use only letters, digits, '-' and '_'");
    }
    WORKSPACE
        .set(workspace.unwrap_or_else(|| "default".to_string()))
        .map_err(|_| anyhow::anyhow!("set_workspace called more than once"))
}

fn workspace() -> &'static str {
    WORKSPACE.get().map_or("default", String::as_str)
}

/// Append the per-profile subdirectory to a base `lt` directory.
pub fn workspace_dir(base: &Path) -> PathBuf {
    base.join("workspaces").join(workspace())
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct Config {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthToken {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: Duration,
    pub scope: String,
    /// Time at which this token was issued.
    pub issued_at: DateTime<Utc>,
    pub refresh_token: String,
}

impl AuthToken {
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.issued_at + self.expires_in
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
    let dir = workspace_dir(
        &dirs::state_dir()
            .ok_or_else(|| anyhow::anyhow!("could not determine state directory"))?
            .join("lt"),
    );
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
    let data = serde_json::to_string_pretty(token)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn token(expires_in_secs: u64, issued_at: DateTime<Utc>) -> AuthToken {
        AuthToken {
            access_token: "tok".to_string(),
            token_type: "Bearer".to_string(),
            expires_in: Duration::from_secs(expires_in_secs),
            scope: "read".to_string(),
            issued_at,
            refresh_token: "refresh-tok".to_string(),
        }
    }

    #[test]
    fn is_expired_true_when_lifetime_elapsed() {
        // Issued at the epoch with a 1s lifetime is unambiguously expired by now.
        assert!(token(1, DateTime::from_timestamp(0, 0).unwrap()).is_expired());
    }

    #[test]
    fn is_expired_false_for_token_far_in_future() {
        assert!(!token(3600, Utc::now()).is_expired());
    }

    #[test]
    fn set_profile_rejects_invalid_names() {
        assert!(set_workspace(Some(String::new())).is_err());
        assert!(set_workspace(Some("has space".to_string())).is_err());
        assert!(set_workspace(Some("slash/path".to_string())).is_err());
    }

    #[test]
    fn workspace_dir_appends_workspaces_and_active_workspace() {
        // No workspace is selected in the test process, so it resolves to "default".
        let dir = workspace_dir(Path::new("/base/lt"));
        assert_eq!(dir, PathBuf::from("/base/lt/workspaces/default"));
    }

    #[test]
    fn auth_token_json_roundtrips() {
        let issued_at = DateTime::from_timestamp(1_000_000, 0).unwrap();
        let original = token(3600, issued_at);
        let json = serde_json::to_string(&original).unwrap();
        let parsed: AuthToken = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.access_token, "tok");
        assert_eq!(parsed.expires_in, Duration::from_secs(3600));
        assert_eq!(parsed.issued_at, issued_at);
        assert_eq!(parsed.refresh_token, "refresh-tok".to_string());
    }

    #[test]
    fn auth_token_missing_required_fields_fails_to_parse() {
        assert!(
            serde_json::from_str::<AuthToken>(r#"{"access_token":"a","token_type":"Bearer"}"#)
                .is_err()
        );
    }

    #[test]
    fn config_defaults_to_empty_credentials() {
        let cfg = Config::default();
        assert!(cfg.client_id.is_none());
        assert!(cfg.client_secret.is_none());
        let parsed: Config = serde_json::from_str("{}").unwrap();
        assert!(parsed.client_id.is_none());
    }
}
