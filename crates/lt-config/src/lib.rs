use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Profiles -- separate auth + database per account/workspace
// ---------------------------------------------------------------------------

static PROFILE: OnceLock<String> = OnceLock::new();

/// Select the active profile for this process.  Must be called once at
/// startup, before any path helper is used.  `None` selects the profile
/// named "default".
pub fn set_profile(name: Option<String>) -> Result<()> {
    if let Some(ref n) = name
        && (n.is_empty()
            || !n
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        anyhow::bail!("invalid profile name {n:?}: use only letters, digits, '-' and '_'");
    }
    PROFILE
        .set(name.unwrap_or_else(|| "default".to_string()))
        .map_err(|_| anyhow::anyhow!("set_profile called more than once"))
}

fn profile() -> &'static str {
    PROFILE.get().map_or("default", String::as_str)
}

/// Append the per-profile subdirectory to a base `lt` directory.
pub fn profile_dir(base: &Path) -> PathBuf {
    base.join("profiles").join(profile())
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
            .map_or(0, |d| d.as_secs());
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
    let dir = profile_dir(
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
    // Stamp issued_at so expiry checks work after the token is reloaded.
    // Preserve any issued_at already set by the caller.
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
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

#[cfg(test)]
mod tests {
    use super::*;

    fn token(expires_in: Option<u64>, issued_at: Option<u64>) -> AuthToken {
        AuthToken {
            access_token: "tok".to_string(),
            token_type: "Bearer".to_string(),
            expires_in,
            scope: None,
            issued_at,
        }
    }

    #[test]
    fn is_expired_false_without_expiry_metadata() {
        // Missing either field means we cannot prove expiry, so treat as valid.
        assert!(!token(None, Some(0)).is_expired());
        assert!(!token(Some(3600), None).is_expired());
        assert!(!token(None, None).is_expired());
    }

    #[test]
    fn is_expired_true_when_lifetime_elapsed() {
        // Issued at the epoch with a 1s lifetime is unambiguously expired by now.
        assert!(token(Some(1), Some(0)).is_expired());
    }

    #[test]
    fn is_expired_false_for_token_far_in_future() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(!token(Some(3600), Some(now)).is_expired());
    }

    #[test]
    fn set_profile_rejects_invalid_names() {
        assert!(set_profile(Some(String::new())).is_err());
        assert!(set_profile(Some("has space".to_string())).is_err());
        assert!(set_profile(Some("slash/path".to_string())).is_err());
    }

    #[test]
    fn profile_dir_appends_profiles_and_active_profile() {
        // No profile is selected in the test process, so it resolves to "default".
        let dir = profile_dir(Path::new("/base/lt"));
        assert_eq!(dir, PathBuf::from("/base/lt/profiles/default"));
    }

    #[test]
    fn auth_token_json_roundtrips() {
        let original = token(Some(3600), Some(1_000_000));
        let json = serde_json::to_string(&original).unwrap();
        let parsed: AuthToken = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.access_token, "tok");
        assert_eq!(parsed.expires_in, Some(3600));
        assert_eq!(parsed.issued_at, Some(1_000_000));
    }

    #[test]
    fn auth_token_issued_at_defaults_when_absent() {
        // Tokens saved before issued_at existed must still deserialize.
        let parsed: AuthToken =
            serde_json::from_str(r#"{"access_token":"a","token_type":"Bearer"}"#).unwrap();
        assert_eq!(parsed.issued_at, None);
        assert!(!parsed.is_expired());
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
